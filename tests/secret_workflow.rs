#![cfg(unix)]
use chromie::{
    agent::{KeyKind, UpdateAgent},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    web_session::WebSessions,
};
use migration::MigratorTrait;
use sea_orm::{ColumnTrait, ConnectOptions, Database, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use uuid::Uuid;
#[allow(dead_code)]
mod support;

async fn command(config: &Path, args: Vec<String>) -> Output {
    let config = config.to_owned();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_chromie"))
            .arg("secret")
            .args(args)
            .arg("--config")
            .arg(config)
            .env_remove("DATABASE_URL")
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}
async fn rpc(client: &reqwest::Client, url: &str, key: &str, method: &str, params: Value) -> Value {
    client
        .post(url)
        .bearer_auth(key)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn no_secrets(value: &str, secret: &str, key: &str) {
    assert!(!value.contains(secret));
    assert!(!value.contains(key));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn topic_guides_and_native_secret_cli_work_without_a_source_checkout() {
    let database = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(
        reqwest::Url::parse(&database)
            .unwrap()
            .path()
            .ends_with("_test")
    );
    let mut options = ConnectOptions::new(database);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new("http://127.0.0.1:9", None).unwrap(),
    ))
    .await;
    let suffix: String = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
        .collect();
    let slug = SpaceKey::parse(&suffix).unwrap();
    store
        .create_space(
            slug.clone(),
            format!("虚构 Secret 指南-{suffix}"),
            "fictional-guide-password".into(),
        )
        .await
        .unwrap();
    let space = store.resolve_space(&slug).await.unwrap();
    let (agent, mcp, hook) = support::grant(&store, space, false).await;
    let router = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/spaces/{}/mcp", slug.as_str());
    let initialized=rpc(&client,&endpoint,&mcp,"initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fictional-secret-guide","version":"0"}})).await;
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    let discovery: String = instructions.chars().take(512).collect();
    assert!(discovery.starts_with("Chromie 是跨对话、跨设备共享的云端记忆空间"));
    assert!(discovery.contains("主动 recall"));
    assert!(discovery.contains("用 remember 保存"));
    assert!(discovery.contains("不强求固定模板或机器信息"));
    assert!(discovery.contains("记忆不代替当前用户授权"));
    assert!(discovery.contains("记住我的 QQ 密码"));
    assert!(discovery.contains("get_setup_guide(topic=\"secret\")"));
    assert!(discovery.contains("用户指定的保存位置优先"));
    assert!(discovery.contains("不能将连接失败解释为不支持 Secret"));
    assert!(discovery.contains("收到回执先 get_memory"));
    assert!(discovery.contains("按需 correct_memory"));
    assert!(discovery.contains("原文已由用户主动提供且明确要求代存"));
    assert!(discovery.contains("不要求重复输入"));
    assert!(discovery.contains("不反复确认已给出的授权"));
    assert!(discovery.contains("不为补说明读取原文"));
    assert!(instructions.contains("不能自行选定缩写含义"));
    assert!(instructions.contains("不自行改动原始采集策略"));
    let tools = rpc(&client, &endpoint, &mcp, "tools/list", json!({})).await;
    let schema = &tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "get_setup_guide")
        .unwrap()["inputSchema"];
    assert!(schema.to_string().contains("secret"));
    assert!(schema.to_string().contains("diagnostics"));
    let setup = rpc(
        &client,
        &endpoint,
        &mcp,
        "tools/call",
        json!({"name":"get_setup_guide","arguments":{}}),
    )
    .await;
    assert_eq!(setup["result"]["structuredContent"]["topic"], "setup");
    assert_eq!(
        setup["result"]["structuredContent"]["secret_access_enabled"],
        false
    );
    let guide=rpc(&client,&endpoint,&mcp,"tools/call",json!({"name":"get_setup_guide","arguments":{"topic":"secret","purpose":"虚构 Secret 流程验证"}})).await;
    let guide = &guide["result"]["structuredContent"];
    assert!(
        guide["guide"]
            .as_str()
            .unwrap()
            .contains("chromie secret put")
    );
    let secret_guide = guide["guide"].as_str().unwrap();
    assert!(secret_guide.contains("已经主动提供原文且明确要求代存"));
    assert!(secret_guide.contains("不要求重复输入"));
    assert!(secret_guide.contains("一次性本地工具输入"));
    assert!(secret_guide.contains("--format text --kind password"));
    assert!(!secret_guide.contains("Agent 不能把密码写入脚本"));
    assert_eq!(guide["agent_id"], agent.to_string());
    assert_eq!(guide["secret_access_enabled"], false);
    assert_eq!(
        guide["access_path"],
        format!("/spaces/{}/access", slug.as_str())
    );
    for (topic, phrase) in [("capture", "消息采集"), ("diagnostics", "短缩写")] {
        let response = rpc(
            &client,
            &endpoint,
            &mcp,
            "tools/call",
            json!({"name":"get_setup_guide","arguments":{"topic":topic}}),
        )
        .await;
        assert_eq!(response["result"]["structuredContent"]["topic"], topic);
        assert!(
            response["result"]["structuredContent"]["guide"]
                .as_str()
                .unwrap()
                .contains(phrase)
        );
    }
    let invalid = rpc(
        &client,
        &endpoint,
        &mcp,
        "tools/call",
        json!({"name":"get_setup_guide","arguments":{"topic":"unknown"}}),
    )
    .await;
    assert!(invalid.get("error").is_some() || invalid["result"]["isError"] == true);
    let root = std::env::temp_dir().join(format!("chromie-secret-workflow-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.join("client.json");
    fs::write(&config,serde_json::to_vec(&json!({"url":base,"space":slug.as_str(),"agent_id":agent,"mcp_key":mcp,"hook_key":hook})).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let source = root.join("credential.bin");
    // 模拟用户已提供并授权代存：写私有中转文件，CLI 不要求再次隐藏输入。
    let secret = "0012-fictional-real-service-secret-alpha";
    fs::write(&source, secret).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let args = vec![
        "put".into(),
        "--label".into(),
        "虚构服务密码".into(),
        "--file".into(),
        source.to_string_lossy().into(),
        "--format".into(),
        "text".into(),
        "--kind".into(),
        "password".into(),
        "--request-id".into(),
        Uuid::now_v7().to_string(),
    ];
    let denied = command(&config, args.clone()).await;
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("secret_access_denied"));
    no_secrets(&String::from_utf8_lossy(&denied.stderr), secret, &mcp);
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: None,
                allow_secrets: Some(true),
            },
        )
        .await
        .unwrap();
    let allowed = rpc(
        &client,
        &endpoint,
        &mcp,
        "tools/call",
        json!({"name":"get_setup_guide","arguments":{"topic":"secret"}}),
    )
    .await;
    assert_eq!(
        allowed["result"]["structuredContent"]["secret_access_enabled"],
        true
    );
    no_secrets(&allowed.to_string(), secret, &mcp);
    assert!(!allowed.to_string().contains(&hook));
    let saved = command(&config, args.clone()).await;
    assert!(saved.status.success(), "真实 CLI put 失败，输出未回显");
    no_secrets(&String::from_utf8_lossy(&saved.stdout), secret, &mcp);
    let receipt: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(receipt["revision"], 1);
    let replay = command(&config, args.clone()).await;
    assert!(replay.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap()["replayed"],
        true
    );
    let metadata = rpc(
        &client,
        &endpoint,
        &mcp,
        "tools/call",
        json!({"name":"get_memory","arguments":{"memory_id":receipt["memory_id"]}}),
    )
    .await;
    assert_eq!(
        metadata["result"]["structuredContent"]["kind"],
        "secret_ref"
    );
    assert_eq!(
        metadata["result"]["structuredContent"]["secret_stored"],
        true
    );
    assert_eq!(
        metadata["result"]["structuredContent"]["contract"],
        json!({"version":1,"format":"text","kind":"password"})
    );
    no_secrets(&metadata.to_string(), secret, &mcp);
    // 录入后仅通过公开元数据补全背景；载荷、引用与记忆数量均不改变。
    let memory_id = Uuid::parse_str(receipt["memory_id"].as_str().unwrap()).unwrap();
    let payload_before = chromie::entity::secrets::Entity::find_by_id((
        space.bytes(),
        memory_id.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    let description = "Aurora 测试部署凭据；环境 staging；用途 aurora-deploy-marker";
    assert!(description.len() <= 256);
    let request_id = Uuid::now_v7();
    let correction = json!({"name":"correct_memory","arguments":{
        "memory_id":memory_id,"expected_revision":metadata["result"]["structuredContent"]["revision"],
        "request_id":request_id,"content":description,
        "scope":{"kind":"project","project_key":"aurora-demo"},"reason":"补全用户说明的用途与适用环境"
    }});
    let corrected = rpc(&client, &endpoint, &mcp, "tools/call", correction.clone()).await;
    assert_eq!(corrected["result"]["isError"], false);
    assert_eq!(
        corrected["result"]["structuredContent"]["memory_id"],
        receipt["memory_id"]
    );
    assert_eq!(corrected["result"]["structuredContent"]["revision"], 2);
    no_secrets(&corrected.to_string(), secret, &mcp);
    let repeated = rpc(&client, &endpoint, &mcp, "tools/call", correction).await;
    assert_eq!(repeated["result"]["structuredContent"]["revision"], 2);
    let enriched = rpc(
        &client,
        &endpoint,
        &mcp,
        "tools/call",
        json!({"name":"get_memory","arguments":{"memory_id":memory_id}}),
    )
    .await;
    let enriched = &enriched["result"]["structuredContent"];
    assert_eq!(enriched["content"], description);
    assert_eq!(enriched["project_key"], "aurora-demo");
    assert_eq!(enriched["revision"], 2);
    assert_eq!(enriched["kind"], "secret_ref");
    assert_eq!(enriched["secret_stored"], true);
    assert_eq!(enriched["secret_ref"], receipt["secret_ref"]);
    assert_eq!(enriched["source_refs"], json!([]));
    let payload_after = chromie::entity::secrets::Entity::find_by_id((
        space.bytes(),
        memory_id.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(payload_after.ciphertext, payload_before.ciphertext);
    assert_eq!(payload_after.nonce, payload_before.nonce);
    assert_eq!(
        payload_after.payload_revision,
        payload_before.payload_revision
    );
    assert_eq!(
        chromie::entity::memories::Entity::find()
            .filter(chromie::entity::memories::Column::SpaceId.eq(space.bytes()))
            .count(&store.db)
            .await
            .unwrap(),
        1
    );
    // 重放旧录入回执仍返回旧修订；回执之后应读取元数据的当前修订。
    let old_receipt = command(&config, args).await;
    assert!(old_receipt.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&old_receipt.stdout).unwrap()["revision"],
        1
    );
    let recalled=rpc(&client,&endpoint,&mcp,"tools/call",json!({"name":"recall","arguments":{"query":"aurora-deploy-marker","project_key":"aurora-demo"}})).await;
    assert_eq!(
        recalled["result"]["structuredContent"]["memories"][0]["memory_id"],
        receipt["memory_id"]
    );
    assert_eq!(
        recalled["result"]["structuredContent"]["memories"][0]["content"],
        description
    );
    no_secrets(&recalled.to_string(), secret, &mcp);
    let output = root.join("out.bin");
    let get = vec![
        "get".into(),
        "--ref".into(),
        receipt["secret_ref"].as_str().unwrap().into(),
        "--out".into(),
        output.to_string_lossy().into(),
    ];
    let fetched = command(&config, get.clone()).await;
    assert!(fetched.status.success());
    assert_eq!(fs::read(&output).unwrap(), secret.as_bytes());
    no_secrets(&String::from_utf8_lossy(&fetched.stdout), secret, &mcp);
    fs::write(&source, "fictional-real-service-secret-beta").unwrap();
    let updated = command(
        &config,
        vec![
            "update".into(),
            "--ref".into(),
            receipt["secret_ref"].as_str().unwrap().into(),
            "--revision".into(),
            "2".into(),
            "--file".into(),
            source.to_string_lossy().into(),
        ],
    )
    .await;
    assert!(updated.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&updated.stdout).unwrap()["revision"],
        3
    );
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: None,
                allow_secrets: Some(false),
            },
        )
        .await
        .unwrap();
    let mut replace = get;
    replace.push("--replace".into());
    assert!(!command(&config, replace).await.status.success());
    assert_eq!(fs::read(&output).unwrap(), secret.as_bytes());
    assert!(
        store
            .authenticate_agent(space, KeyKind::Mcp, &mcp)
            .await
            .is_ok()
    );
    task.abort();
    fs::remove_dir_all(root).unwrap();
}
