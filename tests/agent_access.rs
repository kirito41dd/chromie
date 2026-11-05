use chromie::{
    agent::{CreateAgent, KeyKind, UpdateAgent},
    entity,
    search::Search,
    secret_crypto::SecretCipher,
    server_key::ServerKey,
    space_key::SpaceKey,
    store::Store,
    types::*,
    web_session::WebSessions,
};
use migration::MigratorTrait;
use p256::{
    SecretKey,
    pkcs8::{EncodePrivateKey, LineEnding},
};
use sea_orm::{ConnectOptions, Database, EntityTrait};
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "fictional-agent-space-password";

async fn fixture() -> (Store, SpaceId, SpaceId, ServerKey) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let pem = SecretKey::from_slice(&[41; 32])
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    let root = ServerKey::from_pem(&pem).unwrap();
    let store = Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    )
    .enable_secrets(SecretCipher::from_key(&root).unwrap())
    .await
    .unwrap();
    let mut spaces = Vec::new();
    for prefix in ["agents", "other"] {
        let suffix: String = Uuid::now_v7()
            .simple()
            .to_string()
            .chars()
            .rev()
            .take(20)
            .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
            .collect();
        let slug = SpaceKey::parse(&format!("{prefix}-{suffix}")).unwrap();
        store
            .create_space(
                slug.clone(),
                format!("虚构接入测试-{}", Uuid::now_v7()),
                PASSWORD.into(),
            )
            .await
            .unwrap();
        spaces.push(store.resolve_space(&slug).await.unwrap());
    }
    (store, spaces[0], spaces[1], root)
}

async fn server(store: Store, root: &ServerKey) -> (String, tokio::task::JoinHandle<()>) {
    let router =
        chromie::http::router_with_sessions(store, None, WebSessions::from_key(root).unwrap())
            .await
            .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}

async fn agent(store: &Store, space: SpaceId, name: &str) -> Uuid {
    store
        .create_agent(
            space,
            CreateAgent {
                agent_name: name.into(),
            },
        )
        .await
        .unwrap()
        .agent_id
}

fn event() -> Value {
    json!({"id":Uuid::now_v7(),"client_type":"forged-client","session_id":"虚构接入会话","turn_id":"one","role":"user","body":"虚构来源验证消息","project_key":"agent-test","occurred_at":timestamp(now()).unwrap()})
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn codex_diagnostics_and_plugin_hook_use_separate_keys_without_echoing_credentials() {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
        process::{Command, Stdio},
    };
    let (store, space, _, root) = fixture().await;
    let id = agent(&store, space, "虚构插件测试接入").await;
    let mcp = store
        .reveal_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    let hook = store
        .reveal_agent_key(space, id, KeyKind::Hook)
        .await
        .unwrap();
    let (base, task) = server(store.clone(), &root).await;
    let directory = std::env::temp_dir().join(format!("chromie-plugin-{}", Uuid::now_v7()));
    fs::create_dir_all(directory.join("bin")).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("client.json");
    let connection = json!({"url":base,"space":store.space_key(space).await.unwrap().as_str(),"agent_id":id,"mcp_key":mcp.as_str(),"hook_key":hook.as_str()});
    fs::write(&path, serde_json::to_vec(&connection).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let checked = chromie::codex_check::run(&path, false).await.unwrap();
    assert_eq!(checked["mcp"]["connected"], true);
    assert_eq!(checked["capture_test"]["requested"], false);
    let tested = chromie::codex_check::run(&path, true).await.unwrap();
    assert_eq!(tested["capture_test"]["uploaded"], true);
    assert_eq!(tested["native_hook_verified"], false);
    assert!(
        !tested.to_string().contains(mcp.as_str()) && !tested.to_string().contains(hook.as_str())
    );
    symlink(env!("CARGO_BIN_EXE_chromie"), directory.join("bin/chromie")).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("integrations/codex/plugin/scripts/capture.sh");
    let mut child = Command::new("sh")
        .arg(script)
        .env("PLUGIN_ROOT", &directory)
        .env("CHROMIE_CLIENT_CONFIG", &path)
        .env("CHROMIE_QUEUE_DIR", directory.join("queue"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(serde_json::to_string(&json!({"hook_event_name":"UserPromptSubmit","session_id":"fictional-plugin-session","prompt":"虚构插件采集验证消息"})).unwrap().as_bytes()).unwrap();
    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        !text.contains(mcp.as_str())
            && !text.contains(hook.as_str())
            && !text.contains("虚构插件采集验证消息")
    );
    let result: Value = serde_json::from_str(&text).unwrap();
    let context = result["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("event_id="));
    assert!(context.contains("记住密码"));
    assert!(context.contains("get_setup_guide(topic=\"secret\")"));
    assert!(context.contains("未指定其他位置"));
    assert!(context.contains("收到 Secret 回执先 get_memory"));
    assert!(context.contains("按需 correct_memory"));
    assert!(context.contains("已给且授权代存"));
    assert!(context.contains("不要求重输"));
    assert!(context.chars().count() <= 220, "hook 指引保持简短");
    let event_id: String = context
        .split("event_id=")
        .nth(1)
        .unwrap()
        .chars()
        .take(36)
        .collect();
    let event_id = Uuid::parse_str(&event_id).unwrap();
    let stored =
        entity::source_events::Entity::find_by_id((space.bytes(), event_id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.body.as_deref(), Some("虚构插件采集验证消息"));
    assert_eq!(fs::read_dir(directory.join("queue")).unwrap().count(), 0);
    fs::remove_dir_all(directory).unwrap();
    task.abort();
}

async fn call(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let response = client.post(url).bearer_auth(key).header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":"tools/call","params":{"name":name,"arguments":arguments}})).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "MCP 请求失败：{}",
        response.status()
    );
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value["result"]["isError"], false,
        "MCP 工具失败，未打印请求或响应凭据"
    );
    value["result"]["structuredContent"].clone()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn keys_are_recoverable_encrypted_scoped_and_independently_revocable() {
    let (store, space, other, root) = fixture().await;
    let id = agent(&store, space, "codex-work").await;
    let mcp = store
        .reveal_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    let hook = store
        .reveal_agent_key(space, id, KeyKind::Hook)
        .await
        .unwrap();
    assert!(mcp.as_str() != hook.as_str());
    let row = entity::agent_credentials::Entity::find_by_id((
        space.bytes(),
        id.as_bytes().to_vec(),
        "mcp".to_owned(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert!(
        !row.ciphertext
            .windows(mcp.len())
            .any(|bytes| bytes == mcp.as_bytes())
    );
    assert!(
        store
            .authenticate_agent(other, KeyKind::Mcp, &mcp)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_agent(space, KeyKind::Hook, &mcp)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_agent(space, KeyKind::Mcp, &hook)
            .await
            .is_err()
    );
    let reloaded = Store::new(store.db.clone(), store.search.clone())
        .enable_secrets(SecretCipher::from_key(&root).unwrap())
        .await
        .unwrap();
    assert!(
        reloaded
            .reveal_agent_key(space, id, KeyKind::Mcp)
            .await
            .unwrap()
            .as_str()
            == mcp.as_str()
    );
    assert!(
        store
            .reveal_agent_key(other, id, KeyKind::Mcp)
            .await
            .is_err()
    );
    store
        .rotate_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    assert!(
        store
            .authenticate_agent(space, KeyKind::Mcp, &mcp)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_agent(space, KeyKind::Hook, &hook)
            .await
            .is_ok()
    );
    let next = store
        .reveal_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    store
        .update_agent(
            space,
            id,
            UpdateAgent {
                agent_name: Some("重命名的 Codex".into()),
                enabled: Some(false),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .authenticate_agent(space, KeyKind::Mcp, &next)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_agent(space, KeyKind::Hook, &hook)
            .await
            .is_err()
    );
    store
        .update_agent(
            space,
            id,
            UpdateAgent {
                agent_name: None,
                enabled: Some(true),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_agent(space, KeyKind::Mcp, &next)
            .await
            .unwrap()
            .agent_id(),
        id
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn web_management_agent_permissions_and_secret_access_are_separate() {
    let (store, space, other, root) = fixture().await;
    let id = agent(&store, space, "权限验证").await;
    let mcp = store
        .reveal_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    let hook = store
        .reveal_agent_key(space, id, KeyKind::Hook)
        .await
        .unwrap();
    let (base, task) = server(store.clone(), &root).await;
    let prefix = format!(
        "{base}/spaces/{}",
        store.space_key(space).await.unwrap().as_str()
    );
    let foreign = format!(
        "{base}/spaces/{}",
        store.space_key(other).await.unwrap().as_str()
    );
    let client = reqwest::Client::new();
    for key in [mcp.as_str(), hook.as_str()] {
        assert_eq!(
            client
                .get(format!("{prefix}/agents"))
                .bearer_auth(key)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .post(format!("{prefix}/agents/{id}/keys/mcp/reveal"))
                .bearer_auth(key)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    for endpoint in ["mcp", "events"] {
        assert_eq!(
            client
                .post(format!("{prefix}/{endpoint}"))
                .header("x-chromie-password", PASSWORD)
                .json(&event())
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let login = client
        .post(format!("{prefix}/login"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let create_body = json!({"agent_name":"通过登录态创建的接入"});
    assert_eq!(
        client
            .post(format!("{prefix}/agents"))
            .header("cookie", &cookie)
            .json(&json!({"agent_name":"不能再选择客户端","client_type":"codex"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(format!("{prefix}/agents"))
            .header("x-chromie-password", PASSWORD)
            .json(&create_body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{prefix}/agents"))
            .header("cookie", &cookie)
            .json(&create_body)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{prefix}/agents"))
            .header("cookie", &cookie)
            .json(&create_body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let list: Value = client
        .get(format!("{prefix}/agents"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!list.to_string().contains(mcp.as_str()));
    assert!(!list.to_string().contains(hook.as_str()));
    let revealed: Value = client
        .post(format!("{prefix}/agents/{id}/keys/mcp/reveal"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(revealed["key"].as_str() == Some(mcp.as_str()));
    assert_eq!(
        client
            .post(format!("{foreign}/agents/{id}/keys/mcp/reveal"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{prefix}/events"))
            .bearer_auth(&*mcp)
            .json(&event())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{prefix}/mcp"))
            .bearer_auth(&*hook)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let secret = client
        .post(format!("{prefix}/secrets"))
        .bearer_auth(&*mcp)
        .header("content-type", "application/octet-stream")
        .query(&[
            ("label", "虚构接入 Secret"),
            ("request_id", &Uuid::now_v7().to_string()),
        ])
        .body("fictional-agent-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(secret.status(), 403);
    store
        .update_agent(
            space,
            id,
            UpdateAgent {
                agent_name: None,
                enabled: None,
                allow_secrets: Some(true),
            },
        )
        .await
        .unwrap();
    let secret: Value = client
        .post(format!("{prefix}/secrets"))
        .bearer_auth(&*mcp)
        .header("content-type", "application/octet-stream")
        .query(&[
            ("label", "虚构接入 Secret"),
            ("request_id", &Uuid::now_v7().to_string()),
        ])
        .body("fictional-agent-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let secret_id = secret["memory_id"].as_str().unwrap();
    assert_eq!(
        client
            .get(format!("{prefix}/secrets/{secret_id}"))
            .bearer_auth(&*hook)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{prefix}/secrets/{secret_id}"))
            .bearer_auth(&*mcp)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    store
        .update_agent(
            space,
            id,
            UpdateAgent {
                agent_name: None,
                enabled: None,
                allow_secrets: Some(false),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{prefix}/secrets/{secret_id}"))
            .bearer_auth(&*mcp)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
    let status = |key: String| {
        let client = client.clone();
        let url = format!("{prefix}/mcp");
        let rpc = rpc.clone();
        async move {
            client
                .post(url)
                .bearer_auth(key)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-06-18")
                .json(&rpc)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(status(mcp.to_string()).await, 200);
    store
        .rotate_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    assert_eq!(status(mcp.to_string()).await, 401);
    let new_mcp = store
        .reveal_agent_key(space, id, KeyKind::Mcp)
        .await
        .unwrap();
    assert_eq!(status(new_mcp.to_string()).await, 200);
    store
        .update_agent(
            space,
            id,
            UpdateAgent {
                agent_name: None,
                enabled: Some(false),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(status(new_mcp.to_string()).await, 401);
    assert_eq!(
        client
            .post(format!("{prefix}/events"))
            .bearer_auth(&*hook)
            .json(&event())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn authenticated_sources_survive_rename_and_cannot_be_impersonated_or_replayed() {
    let (store, space, _, root) = fixture().await;
    let first = agent(&store, space, "codex-one").await;
    let second = agent(&store, space, "codex-two").await;
    let mcp = store
        .reveal_agent_key(space, first, KeyKind::Mcp)
        .await
        .unwrap();
    let other_mcp = store
        .reveal_agent_key(space, second, KeyKind::Mcp)
        .await
        .unwrap();
    let hook = store
        .reveal_agent_key(space, first, KeyKind::Hook)
        .await
        .unwrap();
    let other_hook = store
        .reveal_agent_key(space, second, KeyKind::Hook)
        .await
        .unwrap();
    let (base, task) = server(store.clone(), &root).await;
    let prefix = format!(
        "{base}/spaces/{}",
        store.space_key(space).await.unwrap().as_str()
    );
    let client = reqwest::Client::new();
    let input = event();
    assert_eq!(
        client
            .post(format!("{prefix}/events"))
            .bearer_auth(&*hook)
            .header("x-chromie-agent-id", second.to_string())
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{prefix}/events"))
            .bearer_auth(&*hook)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{prefix}/events"))
            .bearer_auth(&*other_hook)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let event_id = Uuid::parse_str(input["id"].as_str().unwrap()).unwrap();
    let row =
        entity::source_events::Entity::find_by_id((space.bytes(), event_id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.agent_id, Some(first.as_bytes().to_vec()));
    assert_eq!(row.client_type, "other");
    let arguments = json!({"request_id":Uuid::now_v7(),"content":"agent-origin-marker 虚构决定","source_refs":[event_id]});
    let saved = call(
        &client,
        &format!("{prefix}/mcp"),
        &mcp,
        "remember",
        arguments.clone(),
    )
    .await;
    let memory = saved["memory_id"].as_str().unwrap();
    let conflicting: Value = client.post(format!("{prefix}/mcp")).bearer_auth(&*other_mcp)
        .header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remember","arguments":arguments}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(
        conflicting["result"]["structuredContent"]["code"],
        "idempotency_conflict"
    );

    let card = call(
        &client,
        &format!("{prefix}/mcp"),
        &other_mcp,
        "get_memory",
        json!({"memory_id":memory}),
    )
    .await;
    assert_eq!(card["origin"]["agent_id"], first.to_string());
    store
        .update_agent(
            space,
            first,
            UpdateAgent {
                agent_name: Some("新的来源名称".into()),
                enabled: None,
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    let card = call(
        &client,
        &format!("{prefix}/mcp"),
        &other_mcp,
        "get_memory",
        json!({"memory_id":memory}),
    )
    .await;
    assert_eq!(card["origin"]["agent_name"], "新的来源名称");
    let corrected = call(&client,&format!("{prefix}/mcp"),&other_mcp,"correct_memory",json!({"memory_id":memory,"expected_revision":1,"request_id":Uuid::now_v7(),"reason":"虚构更正","content":"agent-origin-marker 修订决定"})).await;
    assert_eq!(corrected["revision"], 2);
    let card = call(
        &client,
        &format!("{prefix}/mcp"),
        &mcp,
        "get_memory",
        json!({"memory_id":memory}),
    )
    .await;
    assert_eq!(card["origin"]["agent_id"], second.to_string());
    assert_eq!(card["created_by"]["agent_id"], first.to_string());
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL、Qdrant 和本机采集二进制"]
#[cfg(unix)]
async fn private_hook_config_needs_no_space_password_and_queue_never_changes_agent() {
    use std::os::unix::fs::PermissionsExt;
    let (store, space, _, root) = fixture().await;
    let first = agent(&store, space, "私有配置采集").await;
    let second = agent(&store, space, "另一采集来源").await;
    let (base, task) = server(store.clone(), &root).await;
    let slug = store.space_key(space).await.unwrap();
    let directory = std::env::temp_dir().join(format!("chromie-agent-capture-{}", Uuid::now_v7()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = directory.join("client.json");
    let queue = directory.join("queue");
    let hook = store
        .reveal_agent_key(space, first, KeyKind::Hook)
        .await
        .unwrap();
    let other_hook = store
        .reveal_agent_key(space, second, KeyKind::Hook)
        .await
        .unwrap();
    let write = |id: Uuid, key: &str| {
        std::fs::write(
            &config,
            json!({"url":base,"space":slug.as_str(),"agent_id":id,"hook_key":key}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let run = |command: &'static str, input: Value| {
        let config = config.clone();
        let queue = queue.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_chromie"))
                .args([command, "--config"])
                .arg(config)
                .env("CHROMIE_QUEUE_DIR", queue)
                .env_remove("CHROMIE_PASSWORD")
                .env_remove("CHROMIE_HOOK_KEY")
                .env_remove("CHROMIE_MCP_KEY")
                .env_remove("CHROMIE_AGENT_ID")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        })
    };
    // 用第一位 Agent 的队列身份搭配第二位的密钥，服务拒绝冒认并保留原队列。
    write(first, &other_hook);
    let input = json!({"hook_event_name":"UserPromptSubmit","session_id":"虚构私有配置会话","prompt":"私有配置来源消息"});
    let failed = run("capture", input).await.unwrap();
    assert!(failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stdout).contains(other_hook.as_str()));
    let files = std::fs::read_dir(&queue)
        .unwrap()
        .map(|row| row.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let raw = std::fs::read(&files[0]).unwrap();
    assert!(
        !raw.windows(other_hook.len())
            .any(|bytes| bytes == other_hook.as_bytes())
    );
    write(second, &other_hook);
    let wrong_agent = run("flush", Value::Null).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&wrong_agent.stdout).unwrap()["uploaded"],
        0
    );
    assert!(files[0].exists());
    write(first, &hook);
    let correct = run("flush", Value::Null).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&correct.stdout).unwrap()["uploaded"],
        1
    );
    assert!(!files[0].exists());
    let pending: Value = serde_json::from_slice(&raw).unwrap();
    let event_id = Uuid::parse_str(pending["event"]["id"].as_str().unwrap()).unwrap();
    let row =
        entity::source_events::Entity::find_by_id((space.bytes(), event_id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.agent_id, Some(first.as_bytes().to_vec()));
    std::fs::remove_dir_all(directory).unwrap();
    task.abort();
}
