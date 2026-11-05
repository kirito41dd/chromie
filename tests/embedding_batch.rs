#[allow(dead_code)]
mod support;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use chromie::{
    embedding::Save,
    entity::{embedding_generations as generations, embedding_tasks as tasks},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Forget, Recall, Remember, Scope, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::{ColumnTrait, ConnectOptions, Database, EntityTrait, QueryFilter, sea_query::Expr};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Vec<String>>>,
    mode: AtomicUsize,
    block: AtomicBool,
    started: Notify,
    release: Notify,
    active: AtomicUsize,
    peak: AtomicUsize,
}
struct Active(Arc<Mock>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
fn vector(text: &str) -> Vec<f32> {
    let n = text
        .strip_prefix("batch-note-")
        .and_then(|n| n.parse::<f32>().ok())
        .unwrap_or(99.0);
    vec![1.0, n / 40.0, 0.25]
}
async fn embed(
    State(mock): State<Arc<Mock>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let texts = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!((1..=10).contains(&texts.len()));
    let active = mock.active.fetch_add(1, Ordering::SeqCst) + 1;
    mock.peak.fetch_max(active, Ordering::SeqCst);
    let _active = Active(mock.clone());
    mock.calls.lock().await.push(texts.clone());
    if mock.block.swap(false, Ordering::SeqCst) {
        mock.started.notify_one();
        mock.release.notified().await;
    }
    if mock.mode.load(Ordering::SeqCst) == 1 {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({})));
    }
    let mut data = texts
        .iter()
        .enumerate()
        .map(|(index, text)| json!({"index":index,"embedding":vector(text)}))
        .rev()
        .collect::<Vec<_>>();
    if mock.mode.load(Ordering::SeqCst) == 2 && data.len() > 1 {
        data[0]["index"] = data[1]["index"].clone();
    }
    (
        StatusCode::OK,
        Json(json!({"data":data,"usage":{"total_tokens":texts.len()*7}})),
    )
}
fn input(state: &Value, url: &str) -> Save {
    serde_json::from_value(json!({"expected_revision":state["revision"],"base_url":url,"model":"text-embedding-v4","dimensions":3,"max_concurrency":1,"api_key":null,"confirm_rebuild":true})).unwrap()
}
async fn put(store: &Store, space: SpaceId, text: &str) -> Uuid {
    let value = store
        .remember(
            space,
            Remember {
                content: text.into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    Uuid::parse_str(value["memory_id"].as_str().unwrap()).unwrap()
}
async fn flush(store: &Store) {
    for _ in 0..20 {
        if store.sync_embedding_due().await.unwrap() == 0 {
            return;
        }
    }
    panic!("批量任务未排空");
}
async fn current(store: &Store, owner: SpaceId) -> Value {
    store.model_settings(owner).await.unwrap()
}
async fn requests(store: &Store, owner: SpaceId) -> u64 {
    store.model_usage(owner).await.unwrap()["all_time"]["requests"]
        .as_u64()
        .unwrap()
}
fn point_uuid(id: &qdrant_client::qdrant::PointId) -> Uuid {
    let Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(text)) = &id.point_id_options
    else {
        panic!("需要 UUID 点标识")
    };
    Uuid::parse_str(text).unwrap()
}
async fn points(
    store: &Store,
    space: SpaceId,
    generation: Uuid,
    ids: &[Uuid],
) -> Vec<qdrant_client::qdrant::RetrievedPoint> {
    let _ = store;
    qdrant_client::Qdrant::from_url(&std::env::var("CHROMIE_QDRANT_URL").unwrap())
        .api_key(std::env::var("CHROMIE_QDRANT_API_KEY").ok())
        .build()
        .unwrap()
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(
                format!(
                    "chromie_vec_{}_{}",
                    space.as_uuid().simple(),
                    generation.simple()
                ),
                ids.iter()
                    .map(|id| qdrant_client::qdrant::PointId::from(id.to_string()))
                    .collect::<Vec<_>>(),
            )
            .with_vectors(true)
            .with_payload(true),
        )
        .await
        .unwrap()
        .result
}
async fn reset_due(store: &Store) {
    tasks::Entity::update_many()
        .col_expr(tasks::Column::RunAfter, Expr::value(chromie::types::now()))
        .filter(tasks::Column::State.eq("retry"))
        .exec(&store.db)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "需要隔离 MySQL、Qdrant；只使用本机确定性批量模型"]
async fn batches_preserve_mapping_usage_isolation_runtime_settings_and_stale_barriers() {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    ))
    .await;
    for slug in ["batch-admin", "batch-other"] {
        store
            .create_space(
                SpaceKey::parse(slug).unwrap(),
                format!("虚构批量测试空间-{slug}"),
                "fictional-batch-password".into(),
            )
            .await
            .unwrap();
    }
    let owner = store
        .resolve_space(&SpaceKey::parse("batch-admin").unwrap())
        .await
        .unwrap();
    let other = store
        .resolve_space(&SpaceKey::parse("batch-other").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(owner)).await.unwrap();
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut ids = vec![];
    for n in 0..23 {
        ids.push(put(&store, owner, &format!("batch-note-{n}")).await);
    }
    store
        .save_model_settings(owner, input(&current(&store, owner).await, &base))
        .await
        .unwrap();
    assert_eq!(
        mock.calls
            .lock()
            .await
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![10],
        "连接测试也验证批量"
    );
    mock.calls.lock().await.clear();
    flush(&store).await;
    assert_eq!(
        mock.calls
            .lock()
            .await
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![10, 10, 3]
    );
    assert_eq!(
        requests(&store, owner).await,
        4,
        "每日请求数按 HTTP 批次而非记忆条数"
    );
    assert_eq!(
        store.model_usage(owner).await.unwrap()["all_time"]["tokens"],
        (23 + 10) * 7
    );
    let value = current(&store, owner).await;
    let gid = Uuid::parse_str(value["active"]["generation_id"].as_str().unwrap()).unwrap();
    assert_eq!(value["active"]["batch_size"], 10);
    let fetched = points(&store, owner, gid, &ids).await;
    assert_eq!(fetched.len(), 23);
    for point in fetched {
        let id = point_uuid(point.id.as_ref().unwrap());
        let n = ids.iter().position(|known| *known == id).unwrap();
        let qdrant_client::qdrant::vector_output::Vector::Dense(dense) =
            point.vectors.unwrap().get_vector().unwrap()
        else {
            panic!("需要稠密向量")
        };
        assert!(
            (dense.data[1] / dense.data[0] - n as f32 / 40.0).abs() < 0.0001,
            "乱序响应仍按 index 对应记忆"
        );
    }
    // 纯调度修改不调用模型、不换代、不修改连接密文和已完成任务。
    let before = generations::Entity::find_by_id(gid.as_bytes().to_vec())
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    let calls = requests(&store, owner).await;
    let mut save = input(&value, &base);
    save.batch_size = Some(Some(chromie::embedding_batch::BatchSize::new(4).unwrap()));
    let value = store.save_model_settings(owner, save).await.unwrap();
    let after = generations::Entity::find_by_id(gid.as_bytes().to_vec())
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value["active"]["generation_id"], gid.to_string());
    assert!(value["pending"].is_null());
    assert_eq!(
        (
            before.fingerprint,
            before.connection_revision,
            before.key_cipher
        ),
        (
            after.fingerprint,
            after.connection_revision,
            after.key_cipher
        )
    );
    assert_eq!(requests(&store, owner).await, calls);
    mock.calls.lock().await.clear();
    for n in 23..32 {
        put(&store, owner, &format!("batch-note-{n}")).await;
    }
    flush(&store).await;
    assert_eq!(
        mock.calls
            .lock()
            .await
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![4, 4, 1]
    );
    mock.calls.lock().await.clear();
    put(&store, owner, "owner-only-a").await;
    put(&store, owner, "owner-only-b").await;
    put(&store, other, "other-only-a").await;
    put(&store, other, "other-only-b").await;
    flush(&store).await;
    for call in mock.calls.lock().await.iter() {
        assert_eq!(call.len(), 2);
        assert!(
            call.iter().all(|text| text.starts_with("owner-"))
                || call.iter().all(|text| text.starts_with("other-")),
            "不能跨空间合并"
        );
    }
    // 一个批次占一个共享额度，在线查询不合并，也不会绕过该额度。
    mock.calls.lock().await.clear();
    mock.block.store(true, Ordering::SeqCst);
    let changed = put(&store, owner, "batch-note-35").await;
    let forgotten = put(&store, owner, "batch-note-36").await;
    let worker = tokio::spawn({
        let store = store.clone();
        async move { store.sync_embedding_due().await.unwrap() }
    });
    tokio::time::timeout(Duration::from_secs(3), mock.started.notified())
        .await
        .unwrap();
    assert_eq!(mock.calls.lock().await[0].len(), 2);
    let query = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .recall(
                    owner,
                    Recall {
                        query: "batch-note-1".into(),
                        project_key: None,
                        max_chars: Some(8000),
                    },
                )
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        mock.calls.lock().await.len(),
        1,
        "查询必须等待批次释放同一模型额度"
    );
    store
        .correct(
            owner,
            Correct {
                memory_id: changed,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构并发更正".into(),
                content: Some("batch-note-37".into()),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    store
        .forget(
            owner,
            Forget {
                memory_id: Some(forgotten),
                event_id: None,
                expected_revision: Some(1),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    mock.release.notify_one();
    worker.await.unwrap();
    query.await.unwrap();
    flush(&store).await;
    assert_eq!(mock.peak.load(Ordering::SeqCst), 1);
    assert!(
        mock.calls
            .lock()
            .await
            .iter()
            .skip(1)
            .all(|call| call.len() == 1),
        "在线查询立即单条；更正独立重算"
    );
    let result = points(&store, owner, gid, &[changed, forgotten]).await;
    assert_eq!(result.len(), 1);
    assert_eq!(point_uuid(result[0].id.as_ref().unwrap()), changed);
    let qdrant_client::qdrant::vector_output::Vector::Dense(dense) =
        result[0].vectors.as_ref().unwrap().get_vector().unwrap()
    else {
        panic!("需要稠密向量")
    };
    assert!(
        (dense.data[1] / dense.data[0] - 37.0 / 40.0).abs() < 0.0001,
        "旧修订不能回写"
    );
    // HTTP 失败只计一次请求，任务各自退避；重试只处理尚未完成的条目。
    mock.mode.store(1, Ordering::SeqCst);
    mock.calls.lock().await.clear();
    for n in 40..44 {
        put(&store, owner, &format!("batch-note-{n}")).await;
    }
    let before = requests(&store, owner).await;
    store.sync_embedding_due().await.unwrap();
    assert_eq!(requests(&store, owner).await, before + 1);
    assert_eq!(mock.calls.lock().await.len(), 1);
    let failed = tasks::Entity::find()
        .filter(tasks::Column::State.eq("retry"))
        .all(&store.db)
        .await
        .unwrap();
    assert_eq!(failed.len(), 4);
    mock.mode.store(0, Ordering::SeqCst);
    reset_due(&store).await;
    flush(&store).await;
    assert_eq!(requests(&store, owner).await, before + 2);
    // 协议对应关系出错，整批暂停；降低批大小只恢复对应失败项。
    mock.mode.store(2, Ordering::SeqCst);
    for n in 44..48 {
        put(&store, owner, &format!("batch-note-{n}")).await;
    }
    store.sync_embedding_due().await.unwrap();
    let failed = tasks::Entity::find()
        .filter(tasks::Column::LastError.eq("model_batch_response"))
        .all(&store.db)
        .await
        .unwrap();
    assert_eq!(failed.len(), 4);
    assert!(
        failed
            .iter()
            .all(|row| row.state == "failed" && row.indexed_revision.is_none())
    );
    let mut save = input(&current(&store, owner).await, &base);
    save.batch_size = Some(Some(chromie::embedding_batch::BatchSize::new(1).unwrap()));
    store.save_model_settings(owner, save).await.unwrap();
    mock.calls.lock().await.clear();
    flush(&store).await;
    assert_eq!(
        mock.calls
            .lock()
            .await
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![1, 1, 1, 1]
    );
    // EB-001：兼容设置保存发生在旧批请求失败之前，不能把任务留在旧错误中。
    let mut save = input(&current(&store, owner).await, &base);
    save.batch_size = Some(Some(chromie::embedding_batch::BatchSize::new(10).unwrap()));
    store.save_model_settings(owner, save).await.unwrap();
    mock.calls.lock().await.clear();
    mock.block.store(true, Ordering::SeqCst);
    for n in 50..60 {
        put(&store, owner, &format!("batch-note-{n}")).await;
    }
    let worker = tokio::spawn({
        let store = store.clone();
        async move { store.sync_embedding_due().await.unwrap() }
    });
    tokio::time::timeout(Duration::from_secs(3), mock.started.notified())
        .await
        .unwrap();
    assert_eq!(mock.calls.lock().await[0].len(), 10);
    let mut save = input(&current(&store, owner).await, &base);
    save.batch_size = Some(Some(chromie::embedding_batch::BatchSize::new(1).unwrap()));
    store.save_model_settings(owner, save).await.unwrap();
    mock.release.notify_one();
    worker.await.unwrap();
    flush(&store).await;
    let calls = mock.calls.lock().await;
    assert_eq!(calls.len(), 11);
    assert!(calls.iter().skip(1).all(|call| call.len() == 1));
    drop(calls);
    let leftover = tasks::Entity::find()
        .filter(tasks::Column::State.ne("done"))
        .all(&store.db)
        .await
        .unwrap();
    assert!(
        leftover.is_empty(),
        "旧批次错误须自动在已保存的新大小下恢复"
    );
    // 新模型缺少字段不继承旧覆盖，未知 API 保守单条；原代际保持配置。
    mock.mode.store(0, Ordering::SeqCst);
    let mut save = input(&current(&store, owner).await, &base);
    save.batch_size = Some(Some(chromie::embedding_batch::BatchSize::new(4).unwrap()));
    store.save_model_settings(owner, save).await.unwrap();
    let mut save = input(&current(&store, owner).await, &base);
    save.model = "unknown-model".into();
    let next = store.save_model_settings(owner, save).await.unwrap();
    assert_eq!(next["pending"]["batch_size"], 1);
    assert!(next["pending"]["batch_size_override"].is_null());
    assert_eq!(next["active"]["batch_size"], 4);
    server.abort();
}
