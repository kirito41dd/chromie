mod support;
use axum::{Json, Router, extract::State, routing::post};
use chromie::{
    embedding::Save,
    entity::embedding_generations,
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Recall, Remember, Scope, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database, EntityTrait};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{sync::Notify, task::JoinSet};
use uuid::Uuid;

#[derive(Default)]
struct Model {
    blocked: AtomicBool,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    changed: Notify,
    release: Notify,
}
struct Active(Arc<Model>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}
async fn embed(State(model): State<Arc<Model>>, Json(body): Json<Value>) -> Json<Value> {
    let active = model.active.fetch_add(1, Ordering::SeqCst) + 1;
    model.peak.fetch_max(active, Ordering::SeqCst);
    model.calls.fetch_add(1, Ordering::SeqCst);
    let _active = Active(model.clone());
    let released = model.release.notified();
    tokio::pin!(released);
    released.as_mut().enable();
    model.changed.notify_waiters();
    if model.blocked.load(Ordering::SeqCst) {
        released.await;
    }
    Json(
        json!({"data":[{"index":0,"embedding":vec![1.0;body["dimensions"].as_u64().unwrap() as usize]}],"usage":{"prompt_tokens":5,"total_tokens":5}}),
    )
}
async fn server(model: Arc<Model>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(model);
    (
        url,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn calls(model: &Model, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let changed = model.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if model.calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
}
fn input(state: &Value, url: &str, limit: Option<u32>) -> Save {
    Save {
        similarity_threshold: None,
        batch_size: None,
        expected_revision: state["revision"].as_i64().unwrap(),
        base_url: url.into(),
        model: "fictional-shared-model".into(),
        model_version: "".into(),
        dimensions: 3,
        max_concurrency: limit,
        api_key: None,
        clear_api_key: false,
        confirm_rebuild: false,
        force_rebuild: false,
    }
}
async fn put(store: &Store, space: SpaceId, text: &str) {
    store
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
}
async fn count(store: &Store, owner: SpaceId) -> u64 {
    store.model_usage(owner).await.unwrap()["all_time"]["requests"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
#[ignore = "需要独立 MySQL、Qdrant；使用可阻塞的本机虚构模型"]
async fn all_businesses_share_model_limit_and_live_updates_keep_inflight_accounting() {
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
    for slug in ["limit-admin", "limit-other"] {
        store
            .create_space(
                SpaceKey::parse(slug).unwrap(),
                format!("虚构模型并发-{slug}"),
                "fictional-limit-password".into(),
            )
            .await
            .unwrap();
    }
    let owner = store
        .resolve_space(&SpaceKey::parse("limit-admin").unwrap())
        .await
        .unwrap();
    let other = store
        .resolve_space(&SpaceKey::parse("limit-other").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(owner)).await.unwrap();
    put(&store, owner, "虚构并发检索记录一").await;
    put(&store, other, "虚构并发检索记录二").await;
    let model = Arc::new(Model::default());
    let (base, a) = server(model.clone()).await;
    let (alternate, b) = server(model.clone()).await;
    let empty = store.model_settings(owner).await.unwrap();
    for invalid in [0, 65] {
        assert!(
            store
                .save_model_settings(owner, input(&empty, &base, Some(invalid)))
                .await
                .is_err()
        );
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    store
        .save_model_settings(owner, input(&empty, &base, Some(1)))
        .await
        .unwrap();
    store.sync_embedding_due().await.unwrap();
    store.sync_due().await.unwrap();
    let active = store.model_settings(owner).await.unwrap();
    assert!(active["pending"].is_null());
    assert_eq!(active["active"]["max_concurrency"], 1);
    let generation = Uuid::parse_str(active["active"]["generation_id"].as_str().unwrap()).unwrap();
    let before_generation =
        embedding_generations::Entity::find_by_id(generation.as_bytes().to_vec())
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    let before_calls = model.calls.load(Ordering::SeqCst);
    let before_usage = count(&store, owner).await;
    model.peak.store(0, Ordering::SeqCst);
    model.blocked.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let store = store.clone();
        let state = active.clone();
        let base = base.clone();
        async move {
            store
                .test_model_connection(owner, input(&state, &base, None))
                .await
                .unwrap()
        }
    });
    calls(&model, before_calls + 1).await;
    // 不同空间的召回、后台索引和另一个地址的连接测试都须等待同一模型。
    put(&store, owner, "虚构新并发索引资料").await;
    let worker = tokio::spawn({
        let store = store.clone();
        async move { store.sync_embedding_due().await.unwrap() }
    });
    let second = tokio::spawn({
        let store = store.clone();
        let state = active.clone();
        let base = alternate.clone();
        async move {
            store
                .test_model_connection(owner, input(&state, &base, Some(64)))
                .await
                .unwrap()
        }
    });
    let queried = store
        .recall(
            other,
            Recall {
                query: "虚构并发".into(),
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert_eq!(queried["semantic_available"], false);
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        before_calls + 1,
        "排队与本地超时未发请求"
    );
    assert_eq!(
        count(&store, owner).await,
        before_usage,
        "尚未完成的请求和等待超时不伪造用量"
    );
    model.blocked.store(false, Ordering::SeqCst);
    model.release.notify_waiters();
    first.await.unwrap();
    second.await.unwrap();
    worker.await.unwrap();
    assert_eq!(
        model.peak.load(Ordering::SeqCst),
        1,
        "三类业务和不同地址共用额度"
    );
    assert_eq!(count(&store, owner).await, before_usage + 3);
    let normal = store
        .recall(
            other,
            Recall {
                query: "虚构并发".into(),
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        normal["semantic_available"], true,
        "等待繁忙不能触发供应商故障退避"
    );
    // 满额时仍可仅调整并发；不请求模型，不改代际或密文连接版本。
    model.blocked.store(true, Ordering::SeqCst);
    model.peak.store(0, Ordering::SeqCst);
    let initial_calls = model.calls.load(Ordering::SeqCst);
    let mut inflight = JoinSet::new();
    inflight.spawn({
        let store = store.clone();
        let base = base.clone();
        let state = active.clone();
        async move {
            store
                .test_model_connection(owner, input(&state, &base, None))
                .await
                .unwrap()
        }
    });
    calls(&model, initial_calls + 1).await;
    let raised = tokio::time::timeout(
        Duration::from_secs(1),
        store.save_model_settings(owner, input(&active, &base, Some(2))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), initial_calls + 1);
    assert_eq!(
        raised["active"]["generation_id"],
        active["active"]["generation_id"]
    );
    assert!(raised["pending"].is_null());
    inflight.spawn({
        let store = store.clone();
        let base = base.clone();
        let state = raised.clone();
        async move {
            store
                .test_model_connection(owner, input(&state, &base, None))
                .await
                .unwrap()
        }
    });
    calls(&model, initial_calls + 2).await;
    let lowered = store
        .save_model_settings(owner, input(&raised, &base, Some(1)))
        .await
        .unwrap();
    assert_eq!(lowered["active"]["max_concurrency"], 1);
    let third = tokio::spawn({
        let store = store.clone();
        let base = base.clone();
        let state = lowered.clone();
        async move {
            store
                .test_model_connection(owner, input(&state, &base, None))
                .await
                .unwrap()
        }
    });
    model.release.notify_one();
    inflight.join_next().await.unwrap().unwrap();
    assert_eq!(model.active.load(Ordering::SeqCst), 1);
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        initial_calls + 2,
        "收紧后的在途数量仍占额度"
    );
    model.release.notify_one();
    inflight.join_next().await.unwrap().unwrap();
    calls(&model, initial_calls + 3).await;
    assert_eq!(model.active.load(Ordering::SeqCst), 1);
    model.release.notify_one();
    third.await.unwrap();
    model.blocked.store(false, Ordering::SeqCst);
    assert_eq!(model.peak.load(Ordering::SeqCst), 2);
    let after_generation =
        embedding_generations::Entity::find_by_id(generation.as_bytes().to_vec())
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        after_generation.connection_revision,
        before_generation.connection_revision
    );
    assert_eq!(after_generation.key_cipher, before_generation.key_cipher);
    let restored = support::enable(Store::new(store.db.clone(), store.search.clone())).await;
    assert_eq!(
        restored.model_settings(owner).await.unwrap()["active"]["max_concurrency"],
        1
    );
    // 旧客户端省略并发字段时，沿用已保存值，不能重置到默认四。
    let compatible = store
        .save_model_settings(owner, input(&lowered, &alternate, None))
        .await
        .unwrap();
    assert_eq!(compatible["active"]["max_concurrency"], 1);
    assert_eq!(
        compatible["active"]["generation_id"],
        active["active"]["generation_id"]
    );
    a.abort();
    b.abort();
}
