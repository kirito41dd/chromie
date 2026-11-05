mod support;
use chromie::{
    entity, search::Search, secret_crypto::SecretCipher, server_key::ServerKey,
    space_key::SpaceKey, store::Store, types::*, web_session::WebSessions,
};
use migration::MigratorTrait;
use p256::{
    SecretKey,
    pkcs8::{EncodePrivateKey, LineEnding},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, IntoActiveModel,
    QueryFilter, Set,
};
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "fictional-secret-test-password";
const FIRST: &[u8] = b"fictional_secret_body_alpha_987654321\0\xff\n";
const SECOND: &[u8] = b"fictional_secret_body_beta_123456789\n";

fn key(prefix: &str) -> SpaceKey {
    let suffix: String = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .rev()
        .take(20)
        .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
        .collect();
    SpaceKey::parse(&format!("{prefix}-{suffix}")).unwrap()
}

fn fixture_key(byte: u8) -> ServerKey {
    // 公开的确定性虚构测试密钥，仅用于独立测试数据库，绝不用于开发／生产实例。
    let pem = SecretKey::from_slice(&[byte; 32])
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    ServerKey::from_pem(&pem).unwrap()
}

async fn fixture() -> (Store, SpaceKey, SpaceKey, ServerKey) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let root = fixture_key(41);
    let store = Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    );
    let store = support::enable(store).await;
    let work = key("secret");
    let life = key("other");
    for slug in [&work, &life] {
        store
            .create_space(
                slug.clone(),
                format!("虚构 Secret 验证 {}", Uuid::now_v7()),
                PASSWORD.into(),
            )
            .await
            .unwrap();
    }
    (store, work, life, root)
}

async fn server(store: Store, root: &ServerKey) -> (String, tokio::task::JoinHandle<()>) {
    let router =
        chromie::http::router_with_sessions(store, None, WebSessions::from_key(root).unwrap())
            .await
            .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, task)
}

async fn tool(client: &reqwest::Client, endpoint: &str, name: &str, arguments: Value) -> Value {
    let response: Value = client.post(endpoint)
        .header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(response["result"]["isError"], false, "MCP 工具必须成功");
    response["result"]["structuredContent"].clone()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn secret_http_and_mcp_keep_plaintext_outside_memory_and_handle_revision_forget_and_replay() {
    let (store, work, life, root) = fixture().await;
    let space = store.resolve_space(&work).await.unwrap();
    let (base, task) = server(store.clone(), &root).await;
    let endpoint = format!("{base}/spaces/{}/secrets", work.as_str());
    let mcp = format!("{base}/spaces/{}/mcp", work.as_str());
    let client = reqwest::Client::new();
    let (_, mcp_key, _) = support::grant(&store, space, false).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {mcp_key}").parse().unwrap(),
    );
    let mcp_client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let create = SecretCreate {
        contract: None,
        label: "credential_lookup_marker 示例网站凭据".into(),
        request_id: Uuid::now_v7(),
        project_key: Some("chromie-secret-test".into()),
    };
    let response = client
        .post(&endpoint)
        .header("x-chromie-password", PASSWORD)
        .header("content-type", "application/octet-stream")
        .query(&create)
        .body(FIRST)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["kind"], "secret_ref");
    let id = Uuid::parse_str(saved["memory_id"].as_str().unwrap()).unwrap();
    let fetch = format!("{endpoint}/{id}");
    let row = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    let encrypted = row.ciphertext.as_ref().unwrap();
    assert_eq!(encrypted.len(), FIRST.len() + 16);
    assert!(!encrypted.windows(FIRST.len()).any(|part| part == FIRST));
    assert_eq!(row.nonce.len(), 12);
    assert!(
        entity::source_events::Entity::find()
            .filter(entity::source_events::Column::SpaceId.eq(space.bytes()))
            .all(&store.db)
            .await
            .unwrap()
            .is_empty(),
        "录入不产生包含原文的采集事件"
    );
    let revisions = entity::memory_revisions::Entity::find()
        .filter(entity::memory_revisions::Column::SpaceId.eq(space.bytes()))
        .all(&store.db)
        .await
        .unwrap();
    assert!(revisions.iter().all(|row| {
        !row.content
            .as_deref()
            .unwrap_or_default()
            .contains("fictional_secret_body_alpha")
    }));
    let replay: Value = client
        .post(&endpoint)
        .header("x-chromie-password", PASSWORD)
        .header("content-type", "application/octet-stream")
        .query(&create)
        .body(FIRST)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["memory_id"], saved["memory_id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        client
            .post(&endpoint)
            .header("x-chromie-password", PASSWORD)
            .header("content-type", "application/octet-stream")
            .query(&create)
            .body(SECOND)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    for password in [None, Some("wrong-password")] {
        let mut request = client.get(&fetch);
        if let Some(password) = password {
            request = request.header("x-chromie-password", password)
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    assert_eq!(
        client
            .get(format!("{base}/spaces/{}/secrets/{id}", life.as_str()))
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(&fetch)
            .header("x-chromie-password", PASSWORD)
            .header("origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let login = client
        .post(format!("{base}/spaces/{}/login", work.as_str()))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        client
            .get(&fetch)
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "Web Cookie 不授予 Secret 取用权"
    );
    let ordinary = store
        .remember(
            space,
            Remember {
                content: "普通说明提到 Secret 和 secret://demo/example，不是凭据对象".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    let ordinary_id = ordinary["memory_id"].as_str().unwrap();
    let snapshot: Value = client
        .get(format!("{base}/spaces/{}/dashboard-data", work.as_str()))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["memory_count"], 2);
    assert_eq!(snapshot["secret_count"], 1);
    let secret_card = snapshot["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["memory_id"] == id.to_string())
        .unwrap();
    assert_eq!(secret_card["kind"], "secret_ref");
    assert_eq!(secret_card["secret_stored"], true);
    let normal_card = snapshot["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["memory_id"] == ordinary_id)
        .unwrap();
    assert_eq!(normal_card["kind"], "memory");
    assert!(normal_card["secret_stored"].is_null());
    assert!(!snapshot.to_string().contains("fictional_secret_body_alpha"));
    assert!(
        !snapshot.to_string().contains("ciphertext") && !snapshot.to_string().contains("nonce")
    );
    let other = store
        .dashboard(store.resolve_space(&life).await.unwrap())
        .await
        .unwrap();
    assert_eq!(other.secret_count, 0);
    let response = client
        .get(&fetch)
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.bytes().await.unwrap().as_ref(), FIRST);
    store.sync_due().await.unwrap();
    let found = tool(
        &mcp_client,
        &mcp,
        "recall",
        json!({"query":"credential_lookup_marker"}),
    )
    .await;
    assert_eq!(found["memories"][0]["kind"], "secret_ref");
    assert_eq!(found["memories"][0]["secret_stored"], true);
    assert_eq!(found["memories"][0]["secret_ref"], saved["secret_ref"]);
    let detail = tool(&mcp_client, &mcp, "get_memory", json!({"memory_id":id})).await;
    assert_eq!(detail["content"], create.label);
    let leaked = tool(
        &mcp_client,
        &mcp,
        "recall",
        // 只检索原文独有词，避免普通说明中的 Secret 公共词触发 BM25 命中。
        json!({"query":"987654321"}),
    )
    .await;
    assert!(leaked["memories"].as_array().unwrap().is_empty());
    let history = tool(
        &mcp_client,
        &mcp,
        "get_memory",
        json!({"memory_id":id,"view":"history"}),
    )
    .await;
    let evidence = tool(
        &mcp_client,
        &mcp,
        "get_memory",
        json!({"memory_id":id,"view":"evidence"}),
    )
    .await;
    for visible in [&saved, &found, &detail, &history, &evidence] {
        assert!(!visible.to_string().contains("fictional_secret_body_alpha"));
    }
    let corrected=tool(&mcp_client,&mcp,"correct_memory",json!({"memory_id":id,"expected_revision":1,"request_id":Uuid::now_v7(),"content":"credential_lookup_marker 更新公开说明","reason":"只更正说明"})).await;
    assert_eq!(corrected["revision"], 2);
    assert_eq!(
        client
            .get(&fetch)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        FIRST,
        "更正元数据不改变解密上下文"
    );
    let update = SecretUpdate {
        contract: None,
        request_id: Uuid::now_v7(),
        expected_revision: 2,
    };
    let updated: Value = client
        .put(&fetch)
        .header("x-chromie-password", PASSWORD)
        .header("content-type", "application/octet-stream")
        .query(&update)
        .body(SECOND)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["revision"], 3);
    let latest = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.payload_revision, 2);
    assert_ne!(latest.nonce, row.nonce);
    assert_eq!(
        client
            .get(&fetch)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        SECOND
    );
    let replay: Value = client
        .put(&fetch)
        .header("x-chromie-password", PASSWORD)
        .header("content-type", "application/octet-stream")
        .query(&update)
        .body(SECOND)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["revision"], 3);
    let stale = SecretUpdate {
        contract: None,
        request_id: Uuid::now_v7(),
        expected_revision: 2,
    };
    assert_eq!(
        client
            .put(&fetch)
            .header("x-chromie-password", PASSWORD)
            .header("content-type", "application/octet-stream")
            .query(&stale)
            .body(FIRST)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let reloaded = Store::new(store.db.clone(), store.search.clone())
        .enable_secrets(SecretCipher::from_key(&fixture_key(41)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        reloaded.read_secret(space, id).await.unwrap().as_slice(),
        SECOND
    );
    assert!(
        Store::new(store.db.clone(), store.search.clone())
            .enable_secrets(SecretCipher::from_key(&fixture_key(42)).unwrap())
            .await
            .is_err(),
        "换错私钥必须拒绝启用，不能覆盖密文"
    );
    let forgotten = tool(
        &mcp_client,
        &mcp,
        "forget_memory",
        json!({"memory_id":id,"expected_revision":3,"request_id":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(forgotten["hidden"], true);
    assert_eq!(
        client
            .get(&fetch)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let gone = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert!(gone.ciphertext.is_none());
    let snapshot = store.dashboard(space).await.unwrap();
    assert_eq!(snapshot.secret_count, 0);
    assert_eq!(snapshot.memory_count, 1);
    assert_eq!(snapshot.memories[0].kind, "memory");
    for (method, query, body) in [
        ("POST", serde_json::to_value(&create).unwrap(), FIRST),
        ("PUT", serde_json::to_value(&update).unwrap(), SECOND),
    ] {
        let request = if method == "POST" {
            client.post(&endpoint)
        } else {
            client.put(&fetch)
        };
        let replay: Value = request
            .header("x-chromie-password", PASSWORD)
            .header("content-type", "application/octet-stream")
            .query(&query)
            .body(body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(replay["hidden"], true, "旧请求不能复活密文");
    }
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn full_size_blob_tampering_and_space_rename_preserve_the_right_boundaries() {
    let (store, work, life, root) = fixture().await;
    let space = store.resolve_space(&work).await.unwrap();
    let large = vec![0xafu8; 60000];
    let saved = store
        .create_secret(
            space,
            SecretCreate {
                contract: None,
                label: "虚构的大文件".into(),
                request_id: Uuid::now_v7(),
                project_key: None,
            },
            &large,
        )
        .await
        .unwrap();
    let id = Uuid::parse_str(saved["memory_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        store.read_secret(space, id).await.unwrap().as_slice(),
        large
    );
    let target = key("renamed");
    store
        .rename_space(work.as_str(), target.clone())
        .await
        .unwrap();
    assert_eq!(
        store.read_secret(space, id).await.unwrap().as_slice(),
        large
    );
    let reference = store
        .get(
            space,
            GetMemory {
                memory_id: Some(id),
                event_id: None,
                view: View::Current,
                cursor: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();
    assert!(
        reference["secret_ref"]
            .as_str()
            .unwrap()
            .starts_with(&format!("secret://{}/", target.as_str()))
    );
    let (base, task) = server(store.clone(), &root).await;
    let endpoint = format!("{base}/spaces/{}/secrets", target.as_str());
    let client = reqwest::Client::new();
    for body in [vec![], vec![7u8; 60001]] {
        let response = client
            .post(&endpoint)
            .header("x-chromie-password", PASSWORD)
            .header("content-type", "application/octet-stream")
            .query(&SecretCreate {
                contract: None,
                label: "虚构无效大小".into(),
                request_id: Uuid::now_v7(),
                project_key: None,
            })
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let other = store.resolve_space(&life).await.unwrap();
    let row = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    let mut copied = row.clone().into_active_model();
    copied.space_id = Set(other.bytes());
    assert!(
        copied.insert(&store.db).await.is_err(),
        "数据库组合外键拒绝跨空间对象绑定"
    );
    let mut changed = row.into_active_model();
    let mut damaged = changed.ciphertext.clone().unwrap().unwrap();
    damaged[10] ^= 1;
    changed.ciphertext = Set(Some(damaged));
    changed.update(&store.db).await.unwrap();
    let response = client
        .get(format!("{endpoint}/{id}"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let error = response.text().await.unwrap();
    assert!(!error.contains("ciphertext") && !error.contains("afaf"));
    let mut missing = entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    missing.ciphertext = Set(None);
    missing.update(&store.db).await.unwrap();
    let visible = store
        .get(
            space,
            GetMemory {
                memory_id: Some(id),
                event_id: None,
                view: View::Current,
                cursor: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(visible["kind"], "secret_ref");
    assert_eq!(visible["secret_stored"], false);
    let snapshot = store.dashboard(space).await.unwrap();
    assert_eq!(snapshot.secret_count, 0);
    assert_eq!(snapshot.memories[0].secret_stored, Some(false));
    store
        .forget(
            space,
            Forget {
                memory_id: Some(id),
                event_id: None,
                expected_revision: Some(1),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL、Qdrant、Python 3 与 curl（Unix）"]
#[cfg(unix)]
async fn curl_helper_writes_private_files_without_printing_plaintext_and_preserves_old_files_on_failure()
 {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    let (store, work, _life, root) = fixture().await;
    let space = store.resolve_space(&work).await.unwrap();
    let (_, mcp_key, _) = support::grant(&store, space, true).await;
    let (base, task) = server(store.clone(), &root).await;
    let directory = std::env::temp_dir().join(format!("chromie-secret-test-{}", Uuid::now_v7()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let source = directory.join("input.bin");
    let target = directory.join("credential.bin");
    std::fs::write(&source, FIRST).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    let helper =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/codex/secret-curl.py");
    let run = |arguments: Vec<String>, password: &str| {
        let directory = directory.clone();
        let helper = helper.clone();
        let base = base.clone();
        let work = work.clone();
        let token = if password == PASSWORD {
            mcp_key.clone()
        } else {
            format!("cm_mcp_{}", "0".repeat(64))
        };
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("python3")
                .arg(helper)
                .args(arguments)
                .env("CHROMIE_URL", base)
                .env("CHROMIE_SPACE", work.as_str())
                .env("CHROMIE_MCP_KEY", token)
                .env_remove("CHROMIE_CLIENT_CONFIG")
                .current_dir(directory)
                .output()
                .unwrap()
        })
    };
    let put = run(
        vec![
            "put".into(),
            "helper_marker 虚构凭据".into(),
            source.to_string_lossy().into_owned(),
        ],
        PASSWORD,
    )
    .await
    .unwrap();
    assert!(
        put.status.success(),
        "帮助程序录入必须成功：{}",
        String::from_utf8_lossy(&put.stderr)
    );
    assert!(!String::from_utf8_lossy(&put.stdout).contains("fictional_secret_body_alpha"));
    let saved: Value = serde_json::from_slice(&put.stdout).unwrap();
    let reference = saved["secret_ref"].as_str().unwrap().to_owned();
    let get = run(
        vec![
            "get".into(),
            reference.clone(),
            target.to_string_lossy().into_owned(),
        ],
        PASSWORD,
    )
    .await
    .unwrap();
    assert!(get.status.success());
    assert_eq!(std::fs::read(&target).unwrap(), FIRST);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!String::from_utf8_lossy(&get.stdout).contains("fictional_secret_body_alpha"));
    let denied = run(
        vec![
            "get".into(),
            reference.clone(),
            target.to_string_lossy().into_owned(),
        ],
        "wrong-test-password",
    )
    .await
    .unwrap();
    assert!(!denied.status.success());
    assert_eq!(
        std::fs::read(&target).unwrap(),
        FIRST,
        "鉴权失败保留已有凭据文件"
    );
    let linked = directory.join("linked.bin");
    symlink(&target, &linked).unwrap();
    let rejected = run(
        vec![
            "get".into(),
            reference.clone(),
            linked.to_string_lossy().into_owned(),
        ],
        PASSWORD,
    )
    .await
    .unwrap();
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(&target).unwrap(), FIRST);
    std::fs::write(&source, SECOND).unwrap();
    let update = run(
        vec![
            "update".into(),
            reference.clone(),
            "1".into(),
            source.to_string_lossy().into_owned(),
        ],
        PASSWORD,
    )
    .await
    .unwrap();
    assert!(update.status.success());
    let get = run(
        vec![
            "get".into(),
            reference,
            target.to_string_lossy().into_owned(),
        ],
        PASSWORD,
    )
    .await
    .unwrap();
    assert!(get.status.success());
    assert_eq!(std::fs::read(&target).unwrap(), SECOND);
    let id = Uuid::parse_str(saved["memory_id"].as_str().unwrap()).unwrap();
    let space = store.resolve_space(&work).await.unwrap();
    store
        .forget(
            space,
            Forget {
                memory_id: Some(id),
                event_id: None,
                expected_revision: Some(2),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    task.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
