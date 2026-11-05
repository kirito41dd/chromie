mod support;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use chromie::{
    embedding::Save,
    entity::embedding_tasks as tasks,
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Remember, Scope, SpaceId, now},
};
use migration::MigratorTrait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, IntoActiveModel,
    QueryFilter, Set,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU16, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
struct Model {
    status: AtomicU16,
    calls: AtomicUsize,
}
async fn embed(State(model): State<Arc<Model>>) -> (StatusCode, Json<Value>) {
    model.calls.fetch_add(1, Ordering::SeqCst);
    let code = model.status.load(Ordering::SeqCst);
    if code > 0 {
        return (
            StatusCode::from_u16(code).unwrap(),
            Json(json!({"error":"虚构故障"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]})),
    )
}
fn save(value: &Value, url: &str) -> Save {
    Save {
        similarity_threshold: None,
        batch_size: None,
        expected_revision: value["revision"].as_i64().unwrap(),
        base_url: url.into(),
        model: "fictional-retry-model".into(),
        model_version: "".into(),
        dimensions: 3,
        max_concurrency: Some(4),
        api_key: None,
        clear_api_key: false,
        confirm_rebuild: false,
        force_rebuild: false,
    }
}
async fn put(store: &Store, space: SpaceId) -> Uuid {
    let receipt = store
        .remember(
            space,
            Remember {
                content: "虚构的后台自动恢复验收记忆".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    Uuid::parse_str(receipt["memory_id"].as_str().unwrap()).unwrap()
}
async fn task(store: &Store, id: Uuid) -> tasks::Model {
    tasks::Entity::find()
        .filter(tasks::Column::MemoryId.eq(id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap()
}
async fn due(store: &Store, id: Uuid) {
    let mut row = task(store, id).await.into_active_model();
    row.run_after = Set(now() - time::Duration::seconds(1));
    row.update(&store.db).await.unwrap();
}

#[tokio::test]
#[ignore = "需要隔离的 MySQL 与 Qdrant，不能连接日常实例"]
async fn transient_failures_resume_after_the_old_limit_and_restart_without_manual_retry() {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let search = Search::new(
        &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
        std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
    )
    .unwrap();
    let mut store = support::enable(Store::new(db.clone(), search.clone())).await;
    let slug = SpaceKey::parse("retry-admin").unwrap();
    store
        .create_space(
            slug.clone(),
            "虚构重试验收".into(),
            "fictional-retry-password".into(),
        )
        .await
        .unwrap();
    let space = store.resolve_space(&slug).await.unwrap();
    store.initialize_model_admin(Some(space)).await.unwrap();
    let model = Arc::new(Model::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(model.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let current = store.model_settings(space).await.unwrap();
    store
        .save_model_settings(space, save(&current, &base))
        .await
        .unwrap();
    for _ in 0..3 {
        store.sync_embedding_due().await.unwrap();
    }
    let memory = put(&store, space).await;
    model.status.store(503, Ordering::SeqCst);
    for attempt in 1..=7 {
        if attempt > 1 {
            due(&store, memory).await;
        }
        // 重启只重建运行时，重试次数和安排仍取自数据库。
        store = support::enable(Store::new(db.clone(), search.clone())).await;
        store.sync_embedding_due().await.unwrap();
        let row = task(&store, memory).await;
        assert_eq!(row.state, "retry");
        assert_eq!(row.attempts, attempt);
        assert_eq!(row.last_error.as_deref(), Some("model_server_error"));
        assert!((1..=300).contains(&(row.run_after - now()).whole_seconds()));
        let progress = store.model_settings(space).await.unwrap();
        assert_eq!(progress["active"]["progress"]["retry"], 1);
        assert!(progress["active"]["progress"]["next_retry_at"].is_string());
    }
    model.status.store(0, Ordering::SeqCst);
    let calls = model.calls.load(Ordering::SeqCst);
    store.sync_embedding_due().await.unwrap();
    assert_eq!(calls, model.calls.load(Ordering::SeqCst), "未到期不忙循环");
    due(&store, memory).await;
    store.sync_embedding_due().await.unwrap();
    assert_eq!(task(&store, memory).await.state, "done");
    // 旧版本已因重试上限停住的临时失败也会被重新领取。
    let mut legacy = task(&store, memory).await.into_active_model();
    legacy.state = Set("failed".into());
    legacy.attempts = Set(5);
    legacy.last_error = Set(Some("model_rate_limited".into()));
    legacy.run_after = Set(now() - time::Duration::seconds(1));
    legacy.update(&db).await.unwrap();
    store.sync_embedding_due().await.unwrap();
    assert_eq!(task(&store, memory).await.state, "done");
    let invalid = put(&store, space).await;
    model.status.store(401, Ordering::SeqCst);
    store.sync_embedding_due().await.unwrap();
    assert_eq!(task(&store, invalid).await.state, "failed");
    let calls = model.calls.load(Ordering::SeqCst);
    due(&store, invalid).await;
    store.sync_embedding_due().await.unwrap();
    assert_eq!(
        calls,
        model.calls.load(Ordering::SeqCst),
        "鉴权错误等待配置修复"
    );
    model.status.store(0, Ordering::SeqCst);
    let current = store.model_settings(space).await.unwrap();
    store
        .save_model_settings(space, save(&current, &base))
        .await
        .unwrap();
    store.sync_embedding_due().await.unwrap();
    assert_eq!(task(&store, invalid).await.state, "done");
    server.abort();
}
