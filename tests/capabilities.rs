mod support;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use chromie::{
    agent::UpdateAgent,
    embedding::{Change, Save},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Remember, Scope},
    web_session::WebSessions,
};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
struct Model {
    fail: AtomicBool,
    calls: AtomicUsize,
}
async fn embed(State(model): State<Arc<Model>>) -> (StatusCode, Json<Value>) {
    model.calls.fetch_add(1, Ordering::SeqCst);
    if model.fail.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"fictional-provider-body"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]})),
    )
}
async fn rpc(client: &reqwest::Client, url: &str, key: &str, method: &str, params: Value) -> Value {
    let response = client
        .post(url)
        .bearer_auth(key)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":method,"params":params}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
async fn capabilities(client: &reqwest::Client, url: &str, key: &str) -> Value {
    let response = rpc(
        client,
        url,
        key,
        "tools/call",
        json!({"name":"get_capabilities","arguments":{}}),
    )
    .await;
    assert_ne!(response["result"]["isError"], true);
    let value = response["result"]["structuredContent"].clone();
    assert_eq!(value["capability_version"], 1);
    assert!(
        value["cache_ttl_seconds"]
            .as_u64()
            .is_some_and(|n| n > 0 && n <= 60)
    );
    assert!(!value.to_string().contains("fictional-capability-key"));
    assert!(!value.to_string().contains("base_url"));
    assert!(!value.to_string().contains("progress"));
    value
}
fn save(current: &Value, url: &str) -> Save {
    Save {
        similarity_threshold: None,
        batch_size: None,
        expected_revision: current["revision"].as_i64().unwrap(),
        base_url: url.into(),
        model: "fictional-capability-model".into(),
        model_version: "".into(),
        dimensions: 3,
        max_concurrency: Some(4),
        api_key: Some("fictional-capability-key".into()),
        clear_api_key: false,
        confirm_rebuild: true,
        force_rebuild: false,
    }
}

#[tokio::test]
#[ignore = "需要隔离的 MySQL、Qdrant；不连接日常或生产实例"]
async fn preflight_capabilities_follow_live_scope_and_generation_without_model_calls() {
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
    for slug in ["capability-work", "capability-empty"] {
        store
            .create_space(
                SpaceKey::parse(slug).unwrap(),
                format!("虚构能力验收-{slug}"),
                "fictional-capability-password".into(),
            )
            .await
            .unwrap();
    }
    let work = store
        .resolve_space(&SpaceKey::parse("capability-work").unwrap())
        .await
        .unwrap();
    let empty = store
        .resolve_space(&SpaceKey::parse("capability-empty").unwrap())
        .await
        .unwrap();
    store.initialize_model_admin(Some(work)).await.unwrap();
    let (agent, key, hook) = support::grant(&store, work, false).await;
    let (_, empty_key, _) = support::grant(&store, empty, false).await;
    let app = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let rpc_url = format!("{base}/spaces/capability-work/mcp");
    let tools = rpc(&client, &rpc_url, &key, "tools/list", json!({})).await;
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "get_capabilities")
        .unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    let initial = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(initial["retrieval"]["mode"], "keyword");
    assert_eq!(initial["retrieval"]["reason"], "not_configured");
    assert_eq!(initial["space"], "capability-work");
    // 新工具遵守已有 Agent 鉴权，不能使用上传密钥或另一空间的 MCP 密钥。
    for unauthorized in [&hook, &empty_key] {
        let response=client.post(&rpc_url).bearer_auth(unauthorized).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_capabilities","arguments":{}}})).send().await.unwrap();
        assert_eq!(response.status(), 401);
    }
    let model = Arc::new(Model::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embed))
        .with_state(model.clone());
    let model_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    store
        .remember(
            work,
            Remember {
                content: "虚构项目每周四核对发布计划".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    let config = store.model_settings(work).await.unwrap();
    let building = store
        .save_model_settings(work, save(&config, &model_url))
        .await
        .unwrap();
    let calls = model.calls.load(Ordering::SeqCst);
    let preflight = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(preflight["retrieval"]["mode"], "keyword");
    assert_eq!(preflight["retrieval"]["semantic_state"], "building");
    assert_eq!(
        preflight["retrieval"]["config_revision"],
        building["revision"]
    );
    assert_eq!(
        calls,
        model.calls.load(Ordering::SeqCst),
        "能力读取不能探测或调用模型"
    );
    for _ in 0..4 {
        store.sync_embedding_due().await.unwrap();
    }
    let ready = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(ready["retrieval"]["mode"], "hybrid");
    assert_eq!(ready["retrieval"]["semantic_state"], "ready");
    let empty_url = format!("{base}/spaces/capability-empty/mcp");
    let empty_state = capabilities(&client, &empty_url, &empty_key).await;
    assert_eq!(empty_state["retrieval"]["semantic_state"], "empty");
    assert_eq!(empty_state["retrieval"]["mode"], "keyword");
    assert!(!empty_state.to_string().contains("capability-work"));
    let current = store.model_settings(work).await.unwrap();
    let mut force = save(&current, &model_url);
    force.force_rebuild = true;
    let rebuilding = store.save_model_settings(work, force).await.unwrap();
    let calls = model.calls.load(Ordering::SeqCst);
    let preflight = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(
        preflight["retrieval"]["mode"], "hybrid",
        "旧代可用时不能误报为仅关键词"
    );
    assert_eq!(preflight["retrieval"]["semantic_state"], "ready");
    assert_eq!(preflight["retrieval"]["rebuilding"], true);
    assert_eq!(
        preflight["retrieval"]["config_revision"],
        rebuilding["revision"]
    );
    assert_eq!(calls, model.calls.load(Ordering::SeqCst));
    store
        .remember(
            work,
            Remember {
                content: "虚构项目新增的发布负责人".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        capabilities(&client, &rpc_url, &key).await["retrieval"]["semantic_state"],
        "updating"
    );
    assert_eq!(
        capabilities(&client, &empty_url, &empty_key).await["retrieval"]["semantic_state"],
        "empty",
        "其他空间的增量不能污染空空间状态"
    );
    // 能力与下一次执行可能不同；真实失败必须在 recall 中说明，而非继续声称语义可用。
    model.fail.store(true, Ordering::SeqCst);
    let recalled = rpc(
        &client,
        &rpc_url,
        &key,
        "tools/call",
        json!({"name":"recall","arguments":{"query":"虚构项目发布","max_chars":8000}}),
    )
    .await;
    let result = &recalled["result"]["structuredContent"];
    assert_eq!(result["retrieval_mode"], "keyword");
    assert_eq!(result["semantic_available"], false);
    assert_eq!(result["semantic_reason"], "model_server_error");
    assert_eq!(result["retrieval_revision"], rebuilding["revision"]);
    let calls = model.calls.load(Ordering::SeqCst);
    let degraded = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(degraded["retrieval"]["semantic_state"], "degraded");
    assert_eq!(degraded["retrieval"]["reason"], "retry_backoff");
    assert_eq!(calls, model.calls.load(Ordering::SeqCst));
    let disabled = store
        .disable_embedding(
            work,
            Change {
                expected_revision: rebuilding["revision"].as_i64().unwrap(),
            },
        )
        .await
        .unwrap();
    let preflight = capabilities(&client, &rpc_url, &key).await;
    assert_eq!(preflight["retrieval"]["semantic_state"], "disabled");
    assert_eq!(preflight["retrieval"]["reason"], "disabled");
    assert_eq!(
        preflight["retrieval"]["config_revision"],
        disabled["revision"]
    );
    // 先前读过能力不缓存授权；接入被停用后，新调用仍被拒绝。
    store
        .update_agent(
            work,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: Some(false),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    let response=client.post(&rpc_url).bearer_auth(key).json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_capabilities","arguments":{}}})).send().await.unwrap();
    assert_eq!(response.status(), 401);
    server.abort();
    model_server.abort();
}
