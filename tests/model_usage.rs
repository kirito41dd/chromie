mod support;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use chromie::{
    embedding::Save, entity::model_usage_daily as daily, search::Search, space_key::SpaceKey,
    store::Store, web_session::WebSessions,
};
use migration::{Alias, MigratorTrait, SchemaManager, Table};
use sea_orm::{ActiveModelTrait, ConnectOptions, Database, EntityTrait, PaginatorTrait, Set};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use tokio::task::JoinSet;

async fn embed(
    State(mode): State<Arc<AtomicU8>>,
    Json(input): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let current = mode.load(Ordering::SeqCst);
    let dimensions = input["dimensions"].as_u64().unwrap() as usize + usize::from(current == 2);
    let mut body = json!({"data":[{"index":0,"embedding":vec![1.0;dimensions]}],"usage":{"prompt_tokens":23,"total_tokens":23}});
    match current {
        1 => {
            body.as_object_mut().unwrap().remove("usage");
        }
        3 => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error":"fictional-private-provider-text","usage":{"total_tokens":7}})),
            );
        }
        4 => body["usage"] = json!({"total_tokens":-1}),
        5 => {
            tokio::time::sleep(std::time::Duration::from_secs(11)).await;
        }
        _ => {}
    }
    (StatusCode::OK, Json(body))
}
fn input(url: &str, model: &str) -> Save {
    Save {
        similarity_threshold: None,
        batch_size: None,
        expected_revision: 0,
        base_url: url.into(),
        model: model.into(),
        model_version: "".into(),
        dimensions: 3,
        max_concurrency: Some(4),
        api_key: None,
        clear_api_key: false,
        confirm_rebuild: false,
        force_rebuild: false,
    }
}
async fn seed(store: &Store, date: time::Date, model: &str, count: u64, tokens: u64) {
    daily::ActiveModel {
        usage_date: Set(date),
        model_key: Set(Sha256::digest(model.as_bytes()).to_vec()),
        model_name: Set(model.into()),
        request_count: Set(count),
        total_tokens: Set(tokens),
        failed_count: Set(0),
        unknown_usage_count: Set(0),
        updated_at: Set(chromie::types::now()),
    }
    .insert(&store.db)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "需要独立 MySQL、Qdrant；模型使用本机虚构接口"]
async fn daily_usage_aggregates_concurrency_models_dates_and_unknown_without_request_rows() {
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
    for slug in ["usage-admin", "usage-other"] {
        store
            .create_space(
                SpaceKey::parse(slug).unwrap(),
                format!("虚构用量验收-{slug}"),
                "fictional-usage-password".into(),
            )
            .await
            .unwrap();
    }
    let owner = store
        .resolve_space(&SpaceKey::parse("usage-admin").unwrap())
        .await
        .unwrap();
    let other = store
        .resolve_space(&SpaceKey::parse("usage-other").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(owner)).await.unwrap();
    assert_eq!(
        store.model_usage(owner).await.unwrap()["all_time"]["requests"],
        0
    );
    assert_eq!(store.model_usage(other).await.unwrap_err().status, 403);
    let mode = Arc::new(AtomicU8::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(mode.clone());
    let model_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut calls = JoinSet::new();
    for _ in 0..32 {
        let store = store.clone();
        let base = base.clone();
        calls.spawn(async move {
            store
                .test_model_connection(owner, input(&base, "fictional-model"))
                .await
                .unwrap();
        });
    }
    while let Some(call) = calls.join_next().await {
        call.unwrap();
    }
    let usage = store.model_usage(owner).await.unwrap();
    assert_eq!(usage["today"]["requests"], 32);
    assert_eq!(
        usage["today"]["tokens"],
        32 * 23,
        "prompt 与 total 不重复相加"
    );
    assert_eq!(
        daily::Entity::find().count(&store.db).await.unwrap(),
        1,
        "同模型同日只有一行"
    );
    let mut changed = input(&base, "fictional-model");
    changed.dimensions = 6;
    changed.model_version = "fictional-weights-b".into();
    store.test_model_connection(owner, changed).await.unwrap();
    assert_eq!(
        daily::Entity::find().count(&store.db).await.unwrap(),
        1,
        "维度和版本不拆每日模型统计"
    );
    for (value, error) in [
        (1, None),
        (2, Some("model_dimensions")),
        (3, Some("model_rate_limited")),
        (4, None),
        (5, Some("model_unreachable")),
    ] {
        mode.store(value, Ordering::SeqCst);
        let result = store
            .test_model_connection(owner, input(&base, "fictional-model"))
            .await;
        if let Some(error) = error {
            assert_eq!(result.unwrap_err().code, error);
        } else {
            result.unwrap();
        }
    }
    let usage = store.model_usage(owner).await.unwrap();
    assert_eq!(usage["today"]["requests"], 38);
    assert_eq!(
        usage["today"]["tokens"],
        34 * 23 + 7,
        "失败但已报告的用量也要累计"
    );
    assert_eq!(
        usage["today"]["unknown"], 3,
        "缺失、非法用量和超时不能按已知零处理"
    );
    assert_eq!(usage["today"]["failed"], 3);
    assert_eq!(
        store.model_settings(owner).await.unwrap()["enabled"],
        false,
        "测试不启用模型"
    );
    mode.store(0, Ordering::SeqCst);
    store
        .test_model_connection(owner, input(&base, "fictional-second-model"))
        .await
        .unwrap();
    assert_eq!(daily::Entity::find().count(&store.db).await.unwrap(), 2);
    let today = chromie::types::now().date();
    seed(
        &store,
        today - time::Duration::days(1),
        "fictional-model",
        2,
        200,
    )
    .await;
    seed(
        &store,
        today - time::Duration::days(6),
        "fictional-model",
        3,
        300,
    )
    .await;
    seed(
        &store,
        today - time::Duration::days(7),
        "fictional-model",
        4,
        400,
    )
    .await;
    let usage = store.model_usage(owner).await.unwrap();
    assert_eq!(usage["today"]["requests"], 39);
    assert_eq!(usage["seven_days"]["requests"], 44);
    assert_eq!(usage["all_time"]["requests"], 48);
    assert_eq!(usage["seven_days"]["tokens"], 35 * 23 + 7 + 500);
    assert_eq!(usage["all_time"]["tokens"], 35 * 23 + 7 + 900);
    assert_eq!(usage["days"].as_array().unwrap().len(), 4);
    // 新建 Store 模拟重启，统计来自 MySQL，而非进程缓存。
    let restarted = support::enable(Store::new(store.db.clone(), store.search.clone())).await;
    assert_eq!(restarted.model_usage(owner).await.unwrap(), usage);
    // 单独暂停测试表，验证统计不可用不会让有效模型调用失败。
    let manager = SchemaManager::new(&store.db);
    manager
        .rename_table(
            Table::rename()
                .table(
                    Alias::new("model_usage_daily"),
                    Alias::new("model_usage_daily_paused"),
                )
                .to_owned(),
        )
        .await
        .unwrap();
    let result = store
        .test_model_connection(owner, input(&base, "fictional-model"))
        .await;
    manager
        .rename_table(
            Table::rename()
                .table(
                    Alias::new("model_usage_daily_paused"),
                    Alias::new("model_usage_daily"),
                )
                .to_owned(),
        )
        .await
        .unwrap();
    result.unwrap();
    assert_eq!(
        store.model_usage(owner).await.unwrap(),
        usage,
        "统计故障不能重发模型或重复累计"
    );
    // 用量只进入管理 Cookie 响应；其他空间和 Agent 无法读取。
    let (_agent, token, _) = support::grant(&store, owner, false).await;
    let app = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for (slug, expected) in [("usage-admin", 200), ("usage-other", 403)] {
        let login = client
            .post(format!("{web}/spaces/{slug}/login"))
            .header("x-chromie-password", "fictional-usage-password")
            .send()
            .await
            .unwrap();
        let cookie = login.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let response = client
            .get(format!("{web}/spaces/{slug}/system/model"))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == 200 {
            assert_eq!(response.json::<Value>().await.unwrap()["usage"], usage);
        }
    }
    let response = client
        .get(format!("{web}/spaces/usage-admin/system/model"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    server.abort();
    model_server.abort();
}
