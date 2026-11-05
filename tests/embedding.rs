mod support;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use chromie::{
    embedding::{Change, Save},
    entity::{embedding_generations, embedding_tasks},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Forget, Recall, Remember, Scope, SecretCreate, SpaceId},
    web_session::WebSessions,
};
use migration::MigratorTrait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, Set,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

const PASSWORD: &str = "fictional-embedding-web-password";
const KEY: &str = "fictional-embedding-api-key";
const ROTATED_KEY: &str = "fictional-rotated-embedding-api-key";
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<String>>,
    fail: AtomicBool,
    wrong_dimension: AtomicBool,
    rotated_key_seen: AtomicBool,
    blocked: Mutex<Option<String>>,
    started: Notify,
    release: Notify,
}
async fn embed(
    State(state): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some(concat!("Chromie/", env!("CARGO_PKG_VERSION"))),
        "连接测试、后台索引与语义查询均须发送 Chromie UA"
    );
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    if authorization == Some(&format!("Bearer {ROTATED_KEY}")) {
        state.rotated_key_seen.store(true, Ordering::SeqCst);
    }
    if authorization != Some(&format!("Bearer {KEY}"))
        && authorization != Some(&format!("Bearer {ROTATED_KEY}"))
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"fictional-private-provider-error"})),
        );
    }
    let texts = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    state.calls.lock().await.extend(texts.clone());
    if state.fail.load(Ordering::SeqCst) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":KEY})));
    }
    if state
        .blocked
        .lock()
        .await
        .as_ref()
        .is_some_and(|blocked| texts.contains(blocked))
    {
        state.started.notify_one();
        state.release.notified().await;
    }
    let data = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let mut vector = if text.contains("麒麟") || text.contains("crimsonsignal") {
                vec![1.0, 0.0, 0.0]
            } else if text.contains("大象") || text.contains("azuresignal") {
                vec![0.0, 1.0, 0.0]
            } else {
                vec![0.0, 0.0, 1.0]
            };
            if body["model"] != "text-embedding-v4" {
                vector.swap(0, 1);
            }
            vector.resize(body["dimensions"].as_u64().unwrap() as usize, 0.0);
            if state.wrong_dimension.load(Ordering::SeqCst) {
                vector.push(0.0);
            }
            json!({"index":index,"embedding":vector})
        })
        .rev()
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(json!({"data":data})))
}
async fn mock(state: Arc<Mock>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(state);
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn put(store: &Store, space: SpaceId, text: &str) -> Uuid {
    let result = store
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
    Uuid::parse_str(result["memory_id"].as_str().unwrap()).unwrap()
}
async fn query(store: &Store, space: SpaceId, text: &str) -> Value {
    store
        .recall(
            space,
            Recall {
                query: text.into(),
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap()
}
fn save(current: &Value, url: &str, model: &str) -> Save {
    Save {
        similarity_threshold: None,
        batch_size: None,
        expected_revision: current["revision"].as_i64().unwrap(),
        base_url: url.into(),
        model: model.into(),
        model_version: "".into(),
        dimensions: 3,
        max_concurrency: Some(4),
        api_key: None,
        clear_api_key: false,
        confirm_rebuild: true,
        force_rebuild: false,
    }
}
async fn drain(store: &Store, space: SpaceId) -> Value {
    for _ in 0..100 {
        store.sync_embedding_due().await.unwrap();
        store.sync_due().await.unwrap();
        let state = store.model_settings(space).await.unwrap();
        if state["pending"].is_null() {
            return state;
        }
    }
    panic!("虚构模型未完成重建")
}
async fn login(client: &reqwest::Client, base: &str, slug: &str) -> String {
    let response = client
        .post(format!("{base}/spaces/{slug}/login"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
#[ignore = "需要隔离 MySQL、Qdrant；模型为本机确定性虚构服务"]
async fn optional_instance_embedding_handles_ownership_generations_races_recovery_and_fallback() {
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
    for slug in ["model-admin", "model-other"] {
        store
            .create_space(
                SpaceKey::parse(slug).unwrap(),
                format!("虚构模型验收-{slug}"),
                PASSWORD.into(),
            )
            .await
            .unwrap();
    }
    let owner = store
        .resolve_space(&SpaceKey::parse("model-admin").unwrap())
        .await
        .unwrap();
    let other = store
        .resolve_space(&SpaceKey::parse("model-other").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(owner)).await.unwrap();
    assert!(store.initialize_model_admin(Some(other)).await.is_err());
    let a = put(&store, owner, "麒麟项目使用中文进行开发讨论").await;
    let b = put(&store, other, "大象项目每周五执行发布检查").await;
    let secret = store
        .create_secret(
            owner,
            SecretCreate {
                label: "虚构凭据的公开说明".into(),
                request_id: Uuid::now_v7(),
                project_key: None,
                contract: None,
            },
            b"DO-NOT-EMBED-fictional-secret-payload",
        )
        .await
        .unwrap();
    assert_eq!(
        query(&store, owner, "crimsonsignal").await["retrieval_mode"],
        "keyword"
    );
    let state = Arc::new(Mock::default());
    let (base, model_task) = mock(state.clone()).await;
    let (base2, model_task2) = mock(state.clone()).await;
    let current = store.model_settings(owner).await.unwrap();
    assert_eq!(current["enabled"], false);
    let mut input = save(&current, &base, "text-embedding-v4");
    input.api_key = Some(KEY.into());
    let tested = store.test_model_connection(owner, input).await.unwrap();
    assert_eq!(tested["model"], "text-embedding-v4");
    assert_eq!(tested["dimensions"], 3);
    assert!(tested["elapsed_ms"].as_f64().unwrap() >= 0.0);
    assert_eq!(tested.as_object().unwrap().len(), 4, "不返回向量或凭据");
    assert_eq!(store.model_settings(owner).await.unwrap(), current);
    assert_eq!(
        embedding_generations::Entity::find()
            .count(&store.db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        embedding_tasks::Entity::find()
            .count(&store.db)
            .await
            .unwrap(),
        0
    );
    state.wrong_dimension.store(true, Ordering::SeqCst);
    let mut input = save(&current, &base, "text-embedding-v4");
    input.api_key = Some(KEY.into());
    assert_eq!(
        store
            .test_model_connection(owner, input)
            .await
            .unwrap_err()
            .code,
        "model_dimensions"
    );
    assert_eq!(store.model_settings(owner).await.unwrap(), current);
    let calls_before_save = state.calls.lock().await.len();
    let mut input = save(&current, &base, "text-embedding-v4");
    input.api_key = Some(KEY.into());
    state.wrong_dimension.store(true, Ordering::SeqCst);
    let error = store.save_model_settings(owner, input).await.unwrap_err();
    assert_eq!(error.code, "model_dimensions");
    assert_eq!(
        store.model_settings(owner).await.unwrap()["revision"],
        current["revision"]
    );
    state.wrong_dimension.store(false, Ordering::SeqCst);
    let mut input = save(&current, &base, "text-embedding-v4");
    input.api_key = Some(KEY.into());
    let building = store.save_model_settings(owner, input).await.unwrap();
    assert!(building["active"].is_null());
    assert_eq!(
        state.calls.lock().await.len(),
        calls_before_save + 20,
        "启用测试只发送固定虚构文本"
    );
    assert_eq!(store.model_settings(other).await.unwrap_err().status, 403);
    let (agent, token, _) = support::grant(&store, owner, false).await;
    let app = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web = format!("http://{}", listener.local_addr().unwrap());
    let web_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let cookie = login(&client, &web, "model-admin").await;
    let other_cookie = login(&client, &web, "model-other").await;
    for (slug, auth, status) in [
        ("model-admin", Some(cookie.as_str()), 200),
        ("model-other", Some(other_cookie.as_str()), 403),
        ("model-admin", None, 401),
    ] {
        let mut request = client.get(format!("{web}/spaces/{slug}/system/model"));
        if let Some(cookie) = auth {
            request = request.header("cookie", cookie);
        } else {
            request = request.bearer_auth(&token);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), status);
        assert!(!response.text().await.unwrap().contains(KEY));
    }
    let password_only = client
        .get(format!("{web}/spaces/model-admin/system/model"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(password_only.status(), 401);
    let malformed=client.post(format!("{web}/spaces/model-admin/system/model")).header("cookie",&cookie).json(&json!({"expected_revision":0,"base_url":base,"model":"text-embedding-v4","dimensions":KEY})).send().await.unwrap();
    assert_eq!(malformed.status(), 400);
    assert!(!malformed.text().await.unwrap().contains(KEY));
    // 已保存 Key 可用于未保存的新模型测试，不能创建代际或改变已有重建。
    let before_test = store.model_settings(owner).await.unwrap();
    let tasks_before = embedding_tasks::Entity::find()
        .count(&store.db)
        .await
        .unwrap();
    let generations_before = embedding_generations::Entity::find()
        .count(&store.db)
        .await
        .unwrap();
    let test_body = json!({"expected_revision":before_test["revision"],"base_url":base2,"model":"fictional-candidate-model","dimensions":3});
    for (slug, auth, expected) in [
        ("model-admin", Some(cookie.as_str()), 200),
        ("model-other", Some(other_cookie.as_str()), 403),
        ("model-admin", None, 401),
    ] {
        let mut request = client
            .post(format!("{web}/spaces/{slug}/system/model/test"))
            .json(&test_body);
        if let Some(cookie) = auth {
            request = request.header("cookie", cookie);
        } else {
            request = request.bearer_auth(&token);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), expected);
        let body = response.text().await.unwrap();
        assert!(!body.contains(KEY));
        if expected == 200 {
            let result: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(result["dimensions"], 3);
            assert_eq!(result["model"], "fictional-candidate-model");
        }
    }
    let response = client
        .post(format!("{web}/spaces/model-admin/system/model/test"))
        .header("x-chromie-password", PASSWORD)
        .json(&test_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let mut invalid_key = test_body.clone();
    invalid_key["api_key"] = json!("fictional-unaccepted-key");
    let response = client
        .post(format!("{web}/spaces/model-admin/system/model/test"))
        .header("cookie", &cookie)
        .json(&invalid_key)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body = response.text().await.unwrap();
    assert!(body.contains("model_auth"));
    assert!(!body.contains("fictional-private-provider-error"));
    let response = client
        .post(format!("{web}/spaces/model-admin/system/model/test"))
        .header("cookie", &cookie)
        .json(&json!({"dimensions":KEY}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(!response.text().await.unwrap().contains(KEY));
    let mut stale = test_body.clone();
    stale["expected_revision"] = json!(-1);
    let calls_before_stale = state.calls.lock().await.len();
    let response = client
        .post(format!("{web}/spaces/model-admin/system/model/test"))
        .header("cookie", &cookie)
        .json(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(calls_before_stale, state.calls.lock().await.len());
    assert_eq!(store.model_settings(owner).await.unwrap(), before_test);
    assert_eq!(
        embedding_tasks::Entity::find()
            .count(&store.db)
            .await
            .unwrap(),
        tasks_before
    );
    assert_eq!(
        embedding_generations::Entity::find()
            .count(&store.db)
            .await
            .unwrap(),
        generations_before
    );
    assert!(
        state
            .calls
            .lock()
            .await
            .iter()
            .all(|text| text == "Chromie 虚构连接测试：记住项目的开发约定。")
    );
    let calls_before_drain = store.model_usage(owner).await.unwrap()["all_time"]["requests"]
        .as_u64()
        .unwrap();
    let active = drain(&store, owner).await;
    assert_eq!(
        store.model_usage(owner).await.unwrap()["all_time"]["requests"],
        calls_before_drain + 2,
        "同一空间合批，两个空间累计两次模型调用"
    );
    let generation_a = active["active"]["generation_id"].clone();
    assert_eq!(active["active"]["progress"]["ready"], 3);
    store
        .create_space(
            SpaceKey::parse("model-empty").unwrap(),
            "虚构空空间".into(),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let empty = store
        .resolve_space(&SpaceKey::parse("model-empty").unwrap())
        .await
        .unwrap();
    let calls = state.calls.lock().await.len();
    assert!(
        query(&store, empty, "crimsonsignal").await["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        calls,
        state.calls.lock().await.len(),
        "空空间不调用模型，也不影响其他空间"
    );
    assert!(
        state.calls.lock().await.iter().any(|v| v.contains("大象")),
        "所有空间的历史记忆参与补建"
    );
    assert!(
        state
            .calls
            .lock()
            .await
            .iter()
            .all(|v| !v.contains("DO-NOT-EMBED")),
        "Secret 只处理公开说明"
    );
    let calls_before_query = store.model_usage(owner).await.unwrap()["all_time"]["requests"]
        .as_u64()
        .unwrap();
    let recalled = query(&store, owner, "crimsonsignal").await;
    assert_eq!(
        store.model_usage(owner).await.unwrap()["all_time"]["requests"],
        calls_before_query + 1,
        "语义查询也累计模型调用"
    );
    assert_eq!(recalled["retrieval_mode"], "hybrid");
    assert_eq!(recalled["memories"][0]["memory_id"], a.to_string());
    assert!(
        recalled["memories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["memory_id"] != b.to_string())
    );
    let unchanged = store
        .save_model_settings(owner, save(&active, &base2, "text-embedding-v4"))
        .await
        .unwrap();
    assert_eq!(unchanged["active"]["generation_id"], generation_a);
    assert!(unchanged["pending"].is_null());
    let mut rotated = save(&unchanged, &base2, "text-embedding-v4");
    rotated.api_key = Some(ROTATED_KEY.into());
    let unchanged = store.save_model_settings(owner, rotated).await.unwrap();
    assert_eq!(unchanged["active"]["generation_id"], generation_a);
    assert!(unchanged["pending"].is_null());
    assert!(state.rotated_key_seen.load(Ordering::SeqCst));
    let row = embedding_generations::Entity::find()
        .filter(embedding_generations::Column::State.eq("active"))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !row.key_cipher
            .as_ref()
            .unwrap()
            .windows(KEY.len())
            .any(|part| part == KEY.as_bytes())
    );
    // 超过一页的历史，验证游标恢复和不同空间的覆盖。
    for index in 0..70 {
        put(&store, other, &format!("虚构分页历史资料 {index}")).await;
    }
    for _ in 0..20 {
        store.sync_embedding_due().await.unwrap();
    }
    let mut changed = save(&unchanged, &base2, "same-dimensions-model-b");
    changed.confirm_rebuild = false;
    assert_eq!(
        store
            .save_model_settings(owner, changed)
            .await
            .unwrap_err()
            .code,
        "model_rebuild_confirmation_required"
    );
    let building = store
        .save_model_settings(owner, save(&unchanged, &base2, "same-dimensions-model-b"))
        .await
        .unwrap();
    assert_eq!(building["active"]["generation_id"], generation_a);
    assert_ne!(building["pending"]["generation_id"], generation_a);
    *state.blocked.lock().await = Some("麒麟项目使用中文进行开发讨论".into());
    let worker = tokio::spawn({
        let store = store.clone();
        async move { store.sync_embedding_due().await.unwrap() }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), state.started.notified())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        store.correct(
            owner,
            Correct {
                memory_id: a,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构更正".into(),
                content: Some("大象项目改为每周三发布".into()),
                scope: None,
                source_refs: None,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let secret_id = Uuid::parse_str(secret["memory_id"].as_str().unwrap()).unwrap();
    store
        .forget(
            owner,
            Forget {
                memory_id: Some(secret_id),
                event_id: None,
                expected_revision: Some(1),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    let fresh = put(&store, owner, "麒麟项目新增的编码约定").await;
    store
        .create_space(
            SpaceKey::parse("model-added").unwrap(),
            "重建期间新增的虚构空间".into(),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let added = store
        .resolve_space(&SpaceKey::parse("model-added").unwrap())
        .await
        .unwrap();
    let added_memory = put(&store, added, "大象：新空间也要同步新代际").await;
    let _ = query(&store, added, "azuresignal").await;
    assert_eq!(
        query(&store, owner, "crimsonsignal").await["semantic_available"],
        true,
        "新空间尚无索引不能使其他空间的模型退避"
    );
    *state.blocked.lock().await = None;
    state.release.notify_waiters();
    worker.await.unwrap();
    drain(&store, owner).await;
    // 同时写入、领取任务及修改连接，验证三条路径遵守相同锁序。
    let writes = async {
        for index in 0..20 {
            put(&store, owner, &format!("虚构并发普通资料 {index}")).await;
        }
    };
    let jobs = async {
        for _ in 0..20 {
            store.sync_embedding_due().await.unwrap();
            tokio::task::yield_now().await;
        }
    };
    let connections = async {
        for _ in 0..3 {
            let current = store.model_settings(owner).await.unwrap();
            store
                .save_model_settings(owner, save(&current, &base2, "same-dimensions-model-b"))
                .await
                .unwrap();
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(writes, jobs, connections);
    })
    .await
    .unwrap();
    let active_b = store.model_settings(owner).await.unwrap();
    assert_eq!(
        query(&store, added, "azuresignal").await["memories"][0]["memory_id"],
        added_memory.to_string()
    );
    assert_ne!(active_b["active"]["generation_id"], generation_a);
    assert_eq!(
        active_b["active"]["profile"]["model"],
        "same-dimensions-model-b"
    );
    let result = query(&store, owner, "crimsonsignal").await;
    assert_eq!(result["memories"][0]["memory_id"], fresh.to_string());
    assert!(
        result["memories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["memory_id"] != a.to_string()
                && row["memory_id"] != secret_id.to_string())
    );
    state.fail.store(true, Ordering::SeqCst);
    let degraded = query(&store, owner, "大象").await;
    assert_eq!(degraded["retrieval_mode"], "keyword");
    assert!(!degraded.to_string().contains(KEY));
    let calls = state.calls.lock().await.len();
    let _ = query(&store, owner, "大象").await;
    assert_eq!(
        calls,
        state.calls.lock().await.len(),
        "短暂熔断避免每次等待失败模型"
    );
    state.fail.store(false, Ordering::SeqCst);
    let revived = store
        .save_model_settings(owner, save(&active_b, &base2, "same-dimensions-model-b"))
        .await
        .unwrap();
    let mut force = save(&revived, &base2, "same-dimensions-model-b");
    force.force_rebuild = true;
    let forcing = store.save_model_settings(owner, force).await.unwrap();
    let cancelled = forcing["pending"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    *state.blocked.lock().await = Some("大象项目改为每周三发布".into());
    let in_flight = tokio::spawn({
        let store = store.clone();
        async move { store.sync_embedding_due().await.unwrap() }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), state.started.notified())
        .await
        .unwrap();
    let replacement = store
        .save_model_settings(owner, save(&forcing, &base2, "model-c"))
        .await
        .unwrap();
    assert_ne!(replacement["pending"]["generation_id"], cancelled);
    let cancelled_row = embedding_generations::Entity::find_by_id(
        Uuid::parse_str(&cancelled).unwrap().as_bytes().to_vec(),
    )
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cancelled_row.state, "cancelled");
    let usage_before_cancelled_result =
        store.model_usage(owner).await.unwrap()["all_time"]["requests"]
            .as_u64()
            .unwrap();
    *state.blocked.lock().await = None;
    state.release.notify_waiters();
    in_flight.await.unwrap();
    assert!(
        store.model_usage(owner).await.unwrap()["all_time"]["requests"]
            .as_u64()
            .unwrap()
            > usage_before_cancelled_result,
        "已取消代际的在途计算仍计入实际调用"
    );
    let final_state = drain(&store, owner).await;
    let recovery = put(&store, owner, "麒麟：恢复过期任务租约").await;
    let gid = Uuid::parse_str(final_state["active"]["generation_id"].as_str().unwrap()).unwrap();
    let job = embedding_tasks::Entity::find_by_id((
        gid.as_bytes().to_vec(),
        owner.bytes(),
        recovery.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    let mut job = job.into_active_model();
    job.state = Set("running".into());
    job.lease_token = Set(Some(Uuid::now_v7().as_bytes().to_vec()));
    job.lease_until = Set(Some(chromie::types::now() - time::Duration::seconds(1)));
    job.update(&store.db).await.unwrap();
    let restarted = support::enable(Store::new(
        store.db.clone(),
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    ))
    .await;
    restarted.sync_embedding_due().await.unwrap();
    let recovered = embedding_tasks::Entity::find_by_id((
        gid.as_bytes().to_vec(),
        owner.bytes(),
        recovery.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.state, "done");
    let disabled = store
        .disable_embedding(
            owner,
            Change {
                expected_revision: final_state["revision"].as_i64().unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(disabled["enabled"], false);
    let calls = state.calls.lock().await.len();
    let _ = query(&store, owner, "麒麟").await;
    put(&store, owner, "停用期间的普通记忆").await;
    store.sync_embedding_due().await.unwrap();
    assert_eq!(state.calls.lock().await.len(), calls);
    store
        .forget(
            owner,
            Forget {
                memory_id: Some(recovery),
                event_id: None,
                expected_revision: Some(1),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    for _ in 0..4 {
        store.sync_embedding_due().await.unwrap();
    }
    assert_eq!(
        state.calls.lock().await.len(),
        calls,
        "停用期间的遗忘只删除索引，不能调用模型"
    );
    let old = embedding_generations::Entity::find()
        .filter(embedding_generations::Column::State.is_in(["retired", "cancelled"]))
        .all(&store.db)
        .await
        .unwrap();
    for generation in old {
        if generation.id == gid.as_bytes() {
            continue;
        }
        let mut old = generation.into_active_model();
        old.retired_at = Set(Some(chromie::types::now() - time::Duration::hours(2)));
        old.update(&store.db).await.unwrap();
    }
    for _ in 0..5 {
        store.sync_embedding_due().await.unwrap();
    }
    let cleaned = embedding_generations::Entity::find_by_id(
        Uuid::parse_str(generation_a.as_str().unwrap())
            .unwrap()
            .as_bytes()
            .to_vec(),
    )
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cleaned.state, "cleaned");
    assert!(cleaned.key_cipher.is_none());
    let qdrant = qdrant_client::Qdrant::from_url(&std::env::var("CHROMIE_QDRANT_URL").unwrap())
        .api_key(std::env::var("CHROMIE_QDRANT_API_KEY").ok())
        .build()
        .unwrap();
    assert!(
        !qdrant
            .collection_exists(format!(
                "chromie_vec_{}_{}",
                owner.as_uuid().simple(),
                Uuid::parse_str(generation_a.as_str().unwrap())
                    .unwrap()
                    .simple()
            ))
            .await
            .unwrap()
    );
    assert!(
        qdrant
            .collection_exists(format!(
                "chromie_vec_{}_{}",
                owner.as_uuid().simple(),
                gid.simple()
            ))
            .await
            .unwrap(),
        "保留当前配置的代际不能被清理"
    );
    let deleted = qdrant
        .get_points(qdrant_client::qdrant::GetPointsBuilder::new(
            format!("chromie_vec_{}_{}", owner.as_uuid().simple(), gid.simple()),
            vec![qdrant_client::qdrant::PointId::from(recovery.to_string())],
        ))
        .await
        .unwrap();
    assert!(
        deleted.result.is_empty(),
        "关闭 embedding 后仍完成向量删除传播"
    );
    store
        .rename_space("model-admin", SpaceKey::parse("model-renamed").unwrap())
        .await
        .unwrap();
    assert!(store.can_manage_system(owner).await.unwrap());
    assert!(
        store
            .model_settings(owner)
            .await
            .unwrap()
            .to_string()
            .find(KEY)
            .is_none()
    );
    assert!(agent != Uuid::nil());
    web_task.abort();
    model_task.abort();
    model_task2.abort();
}
