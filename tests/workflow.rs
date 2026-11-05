mod support;
use chromie::space_key::SpaceKey;
use chromie::{auth::Auth, entity, search::Search, store::Store, types::*};
use migration::MigratorTrait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, QueryFilter, Set,
};
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "仅用于自动测试的虚构密码-123";

fn test_key(prefix: &str) -> SpaceKey {
    let letters = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .rev()
        .take(20)
        .map(|digit| char::from(b'a' + digit.to_digit(16).unwrap() as u8))
        .collect::<String>();
    SpaceKey::parse(&format!("{prefix}-{letters}")).unwrap()
}

async fn fixture() -> (Store, SpaceId, SpaceId) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL")
        .expect("请设置隔离测试库，不能指向个人数据数据库");
    let parsed = reqwest::Url::parse(&url).expect("测试数据库 URL 无效");
    assert!(
        parsed
            .path()
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with("_test")),
        "测试数据库名必须以 _test 结尾，拒绝使用个人数据数据库"
    );
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let search = Search::new(
        &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
        std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
    )
    .unwrap();
    let store = support::enable(Store::new(db, search)).await;
    let first = store
        .create_space(
            test_key("work"),
            format!("测试工作-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let second = store
        .create_space(
            test_key("life"),
            format!("测试生活-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let auth = Auth::new(store.db.clone());
    (
        store,
        auth.verify(first, PASSWORD.into()).await.unwrap(),
        auth.verify(second, PASSWORD.into()).await.unwrap(),
    )
}

fn event(body: &str) -> Event {
    Event {
        id: Uuid::now_v7(),
        client_type: "codex".into(),
        session_id: "虚构测试会话".into(),
        turn_id: Some("虚构轮次".into()),
        role: "user".into(),
        body: body.into(),
        project_key: Some("chromie".into()),
        occurred_at: timestamp(now()).unwrap(),
    }
}

fn get(id: Uuid, view: View) -> GetMemory {
    GetMemory {
        memory_id: Some(id),
        event_id: None,
        view,
        cursor: None,
        max_chars: None,
    }
}
fn recall(query: &str) -> Recall {
    Recall {
        query: query.into(),
        project_key: None,
        max_chars: None,
    }
}
fn memory_id(value: &Value) -> Uuid {
    Uuid::parse_str(value["memory_id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn short_space_keys_are_unique_and_rename_preserves_data_and_authentication() {
    let (store, work, life) = fixture().await;
    let old = store.space_key(work).await.unwrap();
    let occupied = store.space_key(life).await.unwrap();
    let duplicate = store
        .create_space(
            old.clone(),
            format!("重复标识-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, "space_conflict");
    assert_eq!(
        store
            .rename_space(old.as_str(), occupied)
            .await
            .unwrap_err()
            .code,
        "space_conflict"
    );
    assert_eq!(store.space_key(work).await.unwrap(), old);
    store
        .remember(
            work,
            Remember {
                content: "更改空间标识时保留的虚构记忆".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    let new = test_key("notes");
    store.rename_space(old.as_str(), new.clone()).await.unwrap();
    assert_eq!(
        store.resolve_space(&new).await.unwrap().bytes(),
        work.bytes()
    );
    assert!(matches!(store.resolve_space(&old).await, Err(error) if error.code == "not_found"));
    let snapshot = store.dashboard(work).await.unwrap();
    assert_eq!(snapshot.space_id, new.as_str());
    assert_eq!(snapshot.memory_count, 1);
    assert_eq!(store.dashboard(life).await.unwrap().memory_count, 0);
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "http://{address}/spaces/{}/dashboard-data",
            new.as_str()
        ))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "旧密码继续绑定原空间");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["space_id"], new.as_str());
    assert_eq!(body["memory_count"], 1);
    for removed in [old.as_str().to_owned(), work.as_uuid().to_string()] {
        assert_eq!(
            client
                .get(format!("http://{address}/spaces/{removed}/dashboard-data"))
                .header("x-chromie-password", PASSWORD)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(
        client
            .get(format!(
                "http://{address}/spaces/{}/dashboard-data",
                new.as_str()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn dashboard_is_read_only_private_and_space_scoped() {
    let (store, work, life) = fixture().await;
    let work_key = store.space_key(work).await.unwrap();
    let life_key = store.space_key(life).await.unwrap();
    let raw = event("raw_dashboard_marker 原始对话正文不得进入看板快照");
    store.capture(work, raw.clone()).await.unwrap();
    let id = memory_id(
        &store
            .remember(
                work,
                Remember {
                    content: "看板只显示本空间已经保存的虚构决定。".into(),
                    request_id: Uuid::now_v7(),
                    scope: Scope::Global,
                    source_refs: vec![raw.id],
                },
            )
            .await
            .unwrap(),
    );
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!(
        "http://{address}/spaces/{}/dashboard-data",
        work_key.as_str()
    );
    let client = reqwest::Client::new();
    let denied = client.get(&url).send().await.unwrap();
    assert_eq!(denied.status(), 401);
    assert_eq!(denied.headers()["cache-control"], "no-store");
    assert_eq!(
        client
            .get(&url)
            .header("x-chromie-password", "虚构的错误密码")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(&url)
            .header("x-chromie-password", PASSWORD)
            .header("origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .get(&url)
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.text().await.unwrap();
    assert!(!body.contains("raw_dashboard_marker"));
    assert!(!body.contains(PASSWORD));
    let snapshot: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snapshot["space_id"], work_key.as_str().to_string());
    assert_eq!(snapshot["memory_count"], 1);
    assert_eq!(snapshot["event_count"], 1);
    assert_eq!(snapshot["pending_count"], 1);
    assert_eq!(snapshot["memories"][0]["source_count"], 1);
    assert_eq!(snapshot["memories"][0]["memory_id"], id.to_string());
    let other: Value = client
        .get(format!(
            "http://{address}/spaces/{}/dashboard-data",
            life_key.as_str()
        ))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["memory_count"], 0);
    assert_eq!(other["event_count"], 0);
    assert!(other["memories"].as_array().unwrap().is_empty());

    let page = client
        .get(format!(
            "http://{address}/spaces/{}/dashboard",
            work_key.as_str()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("connect-src 'self'")
    );
    let html = page.text().await.unwrap();
    assert!(html.contains("canvas"));
    assert!(!html.contains("看板只显示"));
    assert_eq!(
        client
            .get(format!("http://{address}/web/.dev.env"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .post(&url)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        405
    );

    store
        .correct(
            work,
            Correct {
                memory_id: id,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构的更正测试".into(),
                content: Some("更正后的看板内容".into()),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    let current = store.dashboard(work).await.unwrap();
    assert_eq!(current.memories[0].revision, 2);
    assert_eq!(current.memories[0].preview, "更正后的看板内容");
    store
        .forget(
            work,
            Forget {
                memory_id: Some(id),
                event_id: None,
                expected_revision: Some(2),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    let hidden = store.dashboard(work).await.unwrap();
    assert_eq!(hidden.memory_count, 0);
    assert_eq!(hidden.event_count, 0);
    assert!(hidden.memories.is_empty());
    server.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn durable_memory_workflow() {
    let (store, work, life) = fixture().await;
    let original = event("虚构决定：Chromie 使用 MySQL。raw_only_marker 不应被默认召回。");
    store.capture(work, original.clone()).await.unwrap();
    assert!(
        store.capture(work, original.clone()).await.unwrap()["duplicate"]
            .as_bool()
            .unwrap()
    );
    let mut changed = original.clone();
    changed.body = "同一事件 ID 不同内容".into();
    assert_eq!(
        store.capture(work, changed).await.unwrap_err().code,
        "idempotency_conflict"
    );
    let mut changed_time = original.clone();
    changed_time.occurred_at = "2026-01-01T00:00:00Z".into();
    assert_eq!(
        store.capture(work, changed_time).await.unwrap_err().code,
        "idempotency_conflict",
        "事件 ID 的重试不能改写发生时间"
    );
    assert!(
        store.recall(work, recall("raw_only_marker")).await.unwrap()["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let request = Remember {
        content: "Chromie 使用 MySQL 保存数据，采用 schema-first。".into(),
        request_id: Uuid::now_v7(),
        scope: Scope::Project {
            project_key: "chromie".into(),
        },
        source_refs: vec![original.id],
    };
    assert_eq!(
        store
            .remember(life, request.clone())
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    let written = store.remember(work, request.clone()).await.unwrap();
    let id = memory_id(&written);
    let life_memory = memory_id(
        &store
            .remember(
                life,
                Remember {
                    content: "生活空间的虚构独立记忆".into(),
                    request_id: Uuid::now_v7(),
                    scope: Scope::Global,
                    source_refs: vec![],
                },
            )
            .await
            .unwrap(),
    );
    assert!(
        entity::memory_sources::ActiveModel {
            space_id: Set(life.bytes()),
            memory_id: Set(life_memory.as_bytes().to_vec()),
            revision: Set(1),
            event_id: Set(original.id.as_bytes().to_vec()),
        }
        .insert(&store.db)
        .await
        .is_err(),
        "数据库组合外键也须拒绝跨空间来源"
    );
    assert_eq!(
        memory_id(&store.remember(work, request.clone()).await.unwrap()),
        id
    );
    let mut conflict = request.clone();
    conflict.content = "不能覆盖原幂等请求".into();
    assert_eq!(
        store.remember(work, conflict).await.unwrap_err().code,
        "idempotency_conflict"
    );
    assert_eq!(
        store
            .get(life, get(id, View::Current))
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    let fallback = store.recall(work, recall("MySQL")).await.unwrap();
    assert!(
        fallback["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["memory_id"] == id.to_string())
    );
    assert!(store.sync_due().await.unwrap() > 0);
    let hits = store.search.query(work, "MySQL", None).await.unwrap();
    assert!(
        hits.contains(&(id, 1)),
        "BM25 应在真实 Qdrant 返回刚写入的记忆"
    );
    assert!(
        store
            .search
            .query(life, "MySQL", None)
            .await
            .unwrap()
            .is_empty()
    );
    let evidence = store.get(work, get(id, View::Evidence)).await.unwrap();
    assert_eq!(evidence["sources"][0]["event_id"], original.id.to_string());

    let correction = Correct {
        memory_id: id,
        expected_revision: 1,
        request_id: Uuid::now_v7(),
        reason: "虚构更正".into(),
        content: Some("Chromie 改为 PostgreSQL，此条仅供测试。".into()),
        scope: None,
        source_refs: None,
    };
    let mut other = correction.clone();
    other.request_id = Uuid::now_v7();
    let (first, second) = tokio::join!(store.correct(work, correction), store.correct(work, other));
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        first.err().or_else(|| second.err()).unwrap().code,
        "revision_conflict"
    );
    let latest = store.get(work, get(id, View::Current)).await.unwrap();
    assert_eq!(latest["revision"], 2);
    let current = store.recall(work, recall("PostgreSQL")).await.unwrap();
    assert!(
        current["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["revision"] == 2)
    );
    assert!(
        store.recall(work, recall("MySQL")).await.unwrap()["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let history = store.get(work, get(id, View::History)).await.unwrap();
    assert_eq!(history["history"].as_array().unwrap().len(), 2);

    let forgotten = Forget {
        memory_id: Some(id),
        event_id: None,
        expected_revision: Some(2),
        request_id: Uuid::now_v7(),
    };
    let (sync, deleted) = tokio::join!(store.sync_due(), store.forget(work, forgotten.clone()));
    sync.unwrap();
    deleted.unwrap();
    store.sync_due().await.unwrap();
    assert!(
        store.get(work, get(id, View::Current)).await.unwrap()["hidden"]
            .as_bool()
            .unwrap()
    );
    assert!(
        store.get(work, get(id, View::History)).await.unwrap()["hidden"]
            .as_bool()
            .unwrap()
    );
    assert!(
        store.recall(work, recall("PostgreSQL")).await.unwrap()["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search
            .query(work, "PostgreSQL", None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store.remember(work, request).await.unwrap()["hidden"]
            .as_bool()
            .unwrap()
    );
    assert!(
        store.capture(work, original.clone()).await.unwrap()["hidden"]
            .as_bool()
            .unwrap()
    );
    let row =
        entity::source_events::Entity::find_by_id((work.bytes(), original.id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert!(row.body.is_none());
    assert!(
        entity::memory_revisions::Entity::find()
            .filter(entity::memory_revisions::Column::SpaceId.eq(work.bytes()))
            .filter(entity::memory_revisions::Column::MemoryId.eq(id.as_bytes().to_vec()))
            .all(&store.db)
            .await
            .unwrap()
            .iter()
            .all(|row| row.content.is_none() && row.change_reason.is_none())
    );
    assert!(
        store.forget(work, forgotten).await.unwrap()["hidden"]
            .as_bool()
            .unwrap()
    );

    let raw = event("独立原始事件，尚未提取为记忆");
    store.capture(work, raw.clone()).await.unwrap();
    let direct = GetMemory {
        memory_id: None,
        event_id: Some(raw.id),
        view: View::Current,
        cursor: None,
        max_chars: Some(1024),
    };
    assert_eq!(
        store.get(life, direct.clone()).await.unwrap_err().code,
        "not_found"
    );
    store
        .forget(
            work,
            Forget {
                memory_id: None,
                event_id: Some(raw.id),
                expected_revision: None,
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.get(work, direct).await.unwrap_err().code, "not_found");
}

async fn mcp(
    client: &reqwest::Client,
    url: &str,
    password: &str,
    method: &str,
    params: Value,
) -> Value {
    let response = client.post(url).bearer_auth(password).header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18").json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7().to_string(),"method":method,"params":params})).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "MCP HTTP 状态：{}",
        response.status()
    );
    response.json().await.unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn http_auth_and_mcp() {
    let (store, work, life) = fixture().await;
    let (_, mcp_key, hook_key) = support::grant(&store, work, false).await;
    let (_, life_mcp_key, _) = support::grant(&store, life, false).await;
    let work_key = store.space_key(work).await.unwrap();
    let life_key = store.space_key(life).await.unwrap();
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!("http://{address}/spaces/{}/mcp", work_key.as_str());
    let client = reqwest::Client::new();
    let envelope = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
    assert_eq!(
        client
            .post(&url)
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let malformed_marker = "不可回显的虚构请求字段";
    let malformed = client
        .post(format!(
            "http://{address}/spaces/{}/events",
            work_key.as_str()
        ))
        .bearer_auth(&hook_key)
        .json(&json!({"id":malformed_marker}))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 400);
    assert!(!malformed.text().await.unwrap().contains(malformed_marker));
    assert_eq!(
        client
            .post(&url)
            .header("x-chromie-password", "错误的虚构测试密码")
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .header("x-chromie-password", PASSWORD)
            .header("origin", "https://不受信任.example")
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let initialized = mcp(&client, &url, &mcp_key, "initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"chromie-test","version":"0"}})).await;
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    let tools = mcp(&client, &url, &mcp_key, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 7);
    let guide = mcp(
        &client,
        &url,
        &mcp_key,
        "tools/call",
        json!({"name":"get_setup_guide","arguments":{}}),
    )
    .await;
    assert_eq!(
        guide["result"]["structuredContent"]["space"],
        work_key.as_str()
    );
    assert!(!guide.to_string().contains(&mcp_key));
    assert!(!guide.to_string().contains(&hook_key));
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("需要接续任务")
    );
    let remembered = mcp(&client, &url, &mcp_key, "tools/call", json!({"name":"remember","arguments":{"content":"虚构 MCP 决定：采用 Qdrant。","request_id":Uuid::now_v7()}})).await;
    let result = &remembered["result"]["structuredContent"];
    let id = result["memory_id"]
        .as_str()
        .expect("实际 MCP 调用应返回记忆 ID");
    assert!(
        remembered["result"]["content"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let foreign = mcp(
        &client,
        &format!("http://{address}/spaces/{}/mcp", life_key.as_str()),
        &life_mcp_key,
        "tools/call",
        json!({"name":"get_memory","arguments":{"memory_id":id}}),
    )
    .await;
    assert_eq!(foreign["result"]["structuredContent"]["code"], "not_found");
    let source = event("HTTP 采集只保存原始证据");
    assert!(
        client
            .post(format!(
                "http://{address}/spaces/{}/events",
                work_key.as_str()
            ))
            .bearer_auth(&hook_key)
            .json(&source)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    store
        .admin_set_password(work.as_uuid(), "新的虚构测试密码-456".into())
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .header("x-chromie-password", PASSWORD)
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let changed = mcp(&client, &url, &mcp_key, "tools/list", json!({})).await;
    assert_eq!(changed["result"]["tools"].as_array().unwrap().len(), 7);
    let model = entity::spaces::Entity::find_by_id(work.bytes())
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert!(model.password_hash.starts_with("$argon2id$"));
    assert_ne!(model.password_hash, PASSWORD);
    server.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn pagination_recovery_and_project_scope() {
    let (store, work, _) = fixture().await;
    let body = "中文😀\n\"分段证据\"".repeat(180);
    let raw = event(&body);
    store.capture(work, raw.clone()).await.unwrap();
    let mut content = String::new();
    let mut cursor = None;
    for _ in 0..100 {
        let page = store
            .get(
                work,
                GetMemory {
                    memory_id: None,
                    event_id: Some(raw.id),
                    view: View::Current,
                    cursor,
                    max_chars: Some(1024),
                },
            )
            .await
            .unwrap();
        assert!(page.to_string().chars().count() + 128 <= 1024);
        content.push_str(page["body"].as_str().unwrap());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(content, body);
    let mut oversized = event("有效事件");
    oversized.body = "界".repeat(20001);
    assert!(store.capture(work, oversized).await.is_err());

    let original = format!("分页验证中文😀 {}", "引号\"换行\n".repeat(500));
    let id = memory_id(
        &store
            .remember(
                work,
                Remember {
                    content: original.clone(),
                    request_id: Uuid::now_v7(),
                    scope: Scope::Project {
                        project_key: "正确项目".into(),
                    },
                    source_refs: vec![],
                },
            )
            .await
            .unwrap(),
    );
    let mut current = String::new();
    let mut cursor = None;
    for _ in 0..100 {
        let mut input = get(id, View::Current);
        input.max_chars = Some(1024);
        input.cursor = cursor;
        let page = store.get(work, input).await.unwrap();
        assert!(page.to_string().chars().count() + 128 <= 1024);
        current.push_str(page["content"].as_str().unwrap());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(current, original);
    store.sync_due().await.unwrap();
    assert!(
        store
            .search
            .query(work, "分页验证", Some("错误项目"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search
            .query(work, "分页验证", Some("正确项目"))
            .await
            .unwrap()
            .iter()
            .any(|(hit, _)| *hit == id)
    );

    let disconnected = Store::new(
        store.db.clone(),
        Search::new("http://127.0.0.1:9", None).unwrap(),
    );
    let pending = memory_id(
        &disconnected
            .remember(
                work,
                Remember {
                    content: "恢复验证 recovery_marker".into(),
                    request_id: Uuid::now_v7(),
                    scope: Scope::Global,
                    source_refs: vec![],
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(disconnected.sync_due().await.unwrap(), 0);
    let fallback = disconnected
        .recall(work, recall("recovery_marker"))
        .await
        .unwrap();
    assert_eq!(fallback["index_available"], false);
    assert_eq!(fallback["partial"], true);
    assert!(
        fallback["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["memory_id"] == pending.to_string())
    );
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    let restarted = Store::new(store.db.clone(), store.search.clone());
    restarted.sync_due().await.unwrap();
    assert!(
        restarted
            .search
            .query(work, "recovery_marker", None)
            .await
            .unwrap()
            .iter()
            .any(|(hit, _)| *hit == pending)
    );
}

fn codex_probe(url: String, key: String) -> Value {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    let server_name = format!("chromie_probe_{}", Uuid::now_v7().simple());
    let configuration = format!(
        "mcp_servers = {{ {server_name} = {{ url = {url:?}, bearer_token_env_var = \"CHROMIE_PROBE_KEY\", startup_timeout_sec = 10 }} }}"
    );
    let mut child = Command::new("codex")
        .args([
            "-c",
            &configuration,
            "-c",
            "analytics.enabled=false",
            "app-server",
            "--stdio",
        ])
        .env("CHROMIE_PROBE_KEY", key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("需要本机 codex CLI");
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if sender.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let mut sequence = 0;
    let mut send = |method: &str, params: Value| -> Value {
        if method == "initialized" {
            writeln!(input, "{}", json!({"method":"initialized"})).unwrap();
            input.flush().unwrap();
            return Value::Null;
        }
        sequence += 1;
        writeln!(
            input,
            "{}",
            json!({"method":method,"id":sequence,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        loop {
            let value = receiver
                .recv_timeout(Duration::from_secs(20))
                .expect("Codex 客户端响应超时");
            if value["id"] == sequence {
                assert!(
                    value.get("error").is_none(),
                    "本机 Codex app-server 请求失败"
                );
                return value["result"].clone();
            }
        }
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        send(
            "initialize",
            json!({"clientInfo":{"name":"chromie-mvp-test","version":"0.1.0"},"capabilities":{"experimentalApi":true}}),
        );
        send("initialized", Value::Null);
        let tools = send("mcpServerStatus/list", json!({"serverName":server_name}));
        for tool in [
            "remember",
            "recall",
            "get_memory",
            "correct_memory",
            "forget_memory",
        ] {
            assert!(tools.to_string().contains(tool));
        }
        let thread = send(
            "thread/start",
            json!({"ephemeral":true,"cwd":env!("CARGO_MANIFEST_DIR")}),
        );
        let thread_id = thread["thread"]["id"].as_str().unwrap();
        let request_id = Uuid::now_v7();
        let content = "codex_probe_marker 本机客户端验证用的虚构记忆";
        let call = send(
            "mcpServer/tool/call",
            json!({"threadId":thread_id,"server":server_name,"tool":"remember","arguments":{"content":content,"request_id":request_id}}),
        );
        assert_eq!(call["isError"], false);
        let id = memory_id(&call["structuredContent"]);
        assert_eq!(call["structuredContent"]["revision"], 1);
        // 新建客户端会话读取服务中的记忆，避免把原会话上下文当成跨会话记忆。
        let reading = send(
            "thread/start",
            json!({"ephemeral":true,"cwd":env!("CARGO_MANIFEST_DIR")}),
        );
        let reading_id = reading["thread"]["id"].as_str().unwrap();
        let mut call_tool = |tool: &str, arguments: Value| {
            let result = send(
                "mcpServer/tool/call",
                json!({"threadId":reading_id,"server":server_name,"tool":tool,"arguments":arguments}),
            );
            assert_eq!(result["isError"], false, "原生客户端工具调用失败：{tool}");
            result["structuredContent"].clone()
        };
        let found = call_tool("recall", json!({"query":"codex_probe_marker"}));
        assert!(
            found["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["memory_id"] == id.to_string() && row["content"] == content })
        );
        let read = call_tool("get_memory", json!({"memory_id":id}));
        assert_eq!(read["content"], content);
        let corrected = "codex_probe_marker 本机客户端验证用的虚构记忆，已更正";
        let updated = call_tool(
            "correct_memory",
            json!({"memory_id":id,"expected_revision":1,"request_id":Uuid::now_v7(),"content":corrected,"reason":"验证更正链路"}),
        );
        assert_eq!(updated["revision"], 2);
        let found = call_tool("recall", json!({"query":"codex_probe_marker"}));
        assert!(found["memories"].as_array().unwrap().iter().any(|row| {
            row["memory_id"] == id.to_string()
                && row["content"] == corrected
                && row["revision"] == 2
        }));
        let forgotten = call_tool(
            "forget_memory",
            json!({"memory_id":id,"expected_revision":2,"request_id":Uuid::now_v7()}),
        );
        assert_eq!(forgotten["hidden"], true);
        let found = call_tool("recall", json!({"query":"codex_probe_marker"}));
        assert!(
            found["memories"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["memory_id"] != id.to_string())
        );
        let hidden = call_tool("get_memory", json!({"memory_id":id}));
        assert_eq!(hidden["hidden"], true);
        assert!(hidden.get("content").is_none());
        json!({"tools_discovered":5,"tools_called":5,"cross_session_recall":true,"forgotten":true,"model_turns_started":0})
    }));
    let _ = child.kill();
    let _ = child.wait();
    result.unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL、Qdrant 与本机 codex CLI"]
async fn native_codex_client() {
    let (store, work, _) = fixture().await;
    let (_, mcp_key, _) = support::grant(&store, work, false).await;
    let work_key = store.space_key(work).await.unwrap();
    let router = chromie::http::router(store, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/spaces/{}/mcp",
        listener.local_addr().unwrap(),
        work_key.as_str()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let report = tokio::task::spawn_blocking(move || codex_probe(url, mcp_key))
        .await
        .unwrap();
    assert_eq!(report["model_turns_started"], 0);
    server.abort();
}

async fn capture_command(
    directory: std::path::PathBuf,
    url: String,
    space: String,
    credentials: (Uuid, String),
    command: &'static str,
    input: Value,
    paused: bool,
) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_chromie"))
            .arg(command)
            .env("CHROMIE_QUEUE_DIR", directory)
            .env("CHROMIE_URL", url)
            .env("CHROMIE_SPACE", space.to_string())
            .env("CHROMIE_AGENT_ID", credentials.0.to_string())
            .env("CHROMIE_HOOK_KEY", credentials.1)
            .env_remove("CHROMIE_MCP_KEY")
            .env_remove("CHROMIE_CLIENT_CONFIG")
            .env("CHROMIE_CAPTURE", if paused { "off" } else { "on" })
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
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；见开发运行文档"]
async fn hook_capture_queue_and_pause() {
    let (store, work, life) = fixture().await;
    let (agent_id, _, hook_key) = support::grant(&store, work, false).await;
    let hook_credentials = (agent_id, hook_key);
    let work_key = store.space_key(work).await.unwrap();
    let life_key = store.space_key(life).await.unwrap();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local")
        .join(format!("capture-test-{}", Uuid::now_v7()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let message = json!({"hook_event_name":"UserPromptSubmit","session_id":"虚构hook会话","turn_id":"虚构轮次","prompt":"hook原始采集虚构消息，不能自动成为记忆"});
    let paused = capture_command(
        directory.clone(),
        url.clone(),
        work_key.as_str().to_owned(),
        hook_credentials.clone(),
        "capture",
        message.clone(),
        true,
    )
    .await;
    assert!(paused.status.success());
    assert!(!directory.exists(), "暂停必须在本地写入前生效");
    let offline = capture_command(
        directory.clone(),
        url.clone(),
        work_key.as_str().to_owned(),
        hook_credentials.clone(),
        "capture",
        message,
        false,
    )
    .await;
    assert!(
        offline.status.success(),
        "连接超时后应可靠暂存并正常退出 hook"
    );
    assert!(!String::from_utf8_lossy(&offline.stdout).contains("虚构消息"));
    let queued = std::fs::read_dir(&directory)
        .unwrap()
        .map(|v| v.unwrap().path())
        .filter(|p| p.extension().is_some_and(|v| v == "json"))
        .collect::<Vec<_>>();
    assert_eq!(queued.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&queued[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let record: Value = serde_json::from_slice(&std::fs::read(&queued[0]).unwrap()).unwrap();
    let event_id = Uuid::parse_str(record["event"]["id"].as_str().unwrap()).unwrap();
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let wrong = capture_command(
        directory.clone(),
        url.clone(),
        life_key.as_str().to_owned(),
        hook_credentials.clone(),
        "flush",
        Value::Null,
        false,
    )
    .await;
    assert!(wrong.status.success());
    assert!(queued[0].exists(), "切换空间不能把旧事件改投到新空间");
    let uploaded = capture_command(
        directory.clone(),
        url.clone(),
        work_key.as_str().to_owned(),
        hook_credentials.clone(),
        "flush",
        Value::Null,
        false,
    )
    .await;
    assert!(uploaded.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&uploaded.stdout).unwrap()["uploaded"],
        1
    );
    assert!(!queued[0].exists());
    let event =
        entity::source_events::Entity::find_by_id((work.bytes(), event_id.as_bytes().to_vec()))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(event.role, "user");
    assert!(
        store.recall(work, recall("虚构消息")).await.unwrap()["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let reply=capture_command(directory.clone(),url,work_key.as_str().to_owned(),
        hook_credentials.clone(),"capture",json!({"hook_event_name":"Stop","session_id":"虚构hook会话","turn_id":"虚构轮次","last_assistant_message":"虚构最终回复"}),false).await;
    assert!(reply.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&reply.stdout).unwrap(),
        json!({})
    );
    assert!(
        entity::source_events::Entity::find()
            .filter(entity::source_events::Column::SpaceId.eq(work.bytes()))
            .all(&store.db)
            .await
            .unwrap()
            .iter()
            .any(|event| event.role == "assistant")
    );
    server.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；通用 stdin、私有队列及 bin 上传验证"]
async fn generic_capture_uses_the_same_bin_queue_and_upload_without_client_generated_event_metadata()
 {
    let (store, work, _) = fixture().await;
    let (agent, _, hook) = support::grant(&store, work, false).await;
    let credentials = (agent, hook);
    let space = store.space_key(work).await.unwrap().as_str().to_owned();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local")
        .join(format!("generic-capture-test-{}", Uuid::now_v7()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let user = json!({"session_id":"generic-fictional-session","role":"user","body":"虚构通用用户消息",
        "turn_id":"fictional-turn","project_key":"generic-project"});
    let offline = capture_command(
        directory.clone(),
        url.clone(),
        space.clone(),
        credentials.clone(),
        "capture",
        user.clone(),
        false,
    )
    .await;
    assert!(offline.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&offline.stdout).unwrap(),
        json!({})
    );
    assert!(!String::from_utf8_lossy(&offline.stderr).contains("虚构通用用户消息"));
    let queued = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(queued.len(), 1);
    let record: Value = serde_json::from_slice(&std::fs::read(&queued[0]).unwrap()).unwrap();
    let event = Uuid::parse_str(record["event"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(event.get_version_num(), 7);
    assert!(
        time::OffsetDateTime::parse(
            record["event"]["occurred_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339
        )
        .is_ok()
    );
    assert_eq!(record["event"]["client_type"], "other");
    assert!(!record.to_string().contains(&credentials.1));
    let mut invalid = user.clone();
    invalid["role"] = json!("tool");
    let rejected = capture_command(
        directory.clone(),
        url.clone(),
        space.clone(),
        credentials.clone(),
        "capture",
        invalid,
        false,
    )
    .await;
    assert!(!rejected.status.success());
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains("虚构通用用户消息"));
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let flushed = capture_command(
        directory.clone(),
        url.clone(),
        space.clone(),
        credentials.clone(),
        "flush",
        Value::Null,
        false,
    )
    .await;
    assert!(flushed.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&flushed.stdout).unwrap()["uploaded"],
        1
    );
    let row = entity::source_events::Entity::find_by_id((work.bytes(), event.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.agent_id, Some(agent.as_bytes().to_vec()));
    assert_eq!(row.body.as_deref(), Some("虚构通用用户消息"));
    assert_eq!(row.project_key.as_deref(), Some("generic-project"));
    assert_eq!(row.turn_id.as_deref(), Some("fictional-turn"));
    let reply = capture_command(directory.clone(), url, space, credentials.clone(), "capture",
        json!({"session_id":"generic-fictional-session","role":"assistant","body":"虚构通用最终回复"}), false).await;
    assert!(reply.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&reply.stdout).unwrap(),
        json!({})
    );
    assert!(!String::from_utf8_lossy(&reply.stdout).contains(&credentials.1));
    let rows = entity::source_events::Entity::find()
        .filter(entity::source_events::Column::SpaceId.eq(work.bytes()))
        .all(&store.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.role == "assistant" && row.body.as_deref() == Some("虚构通用最终回复"))
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant；验证 Agent 短时并行 MCP 调用"]
async fn short_parallel_mcp_calls_accept_agent_credentials() {
    let (store, work, _) = fixture().await;
    let (_, mcp_key, _) = support::grant(&store, work, false).await;
    let slug = store.space_key(work).await.unwrap();
    let router = chromie::http::router(store, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/spaces/{}/mcp",
        listener.local_addr().unwrap(),
        slug.as_str()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(6));
    let client = reqwest::Client::new();
    let mut requests = Vec::new();
    for index in 0..6 {
        let client = client.clone();
        let barrier = barrier.clone();
        let url = url.clone();
        let mcp_key = mcp_key.clone();
        requests.push(tokio::spawn(async move {
            barrier.wait().await;
            client
                .post(url)
                .bearer_auth(&mcp_key)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-06-18")
                .json(&json!({"jsonrpc":"2.0","id":index,"method":"tools/list","params":{}}))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let mut statuses = Vec::new();
    for request in requests {
        statuses.push(request.await.unwrap());
    }
    server.abort();
    assert!(
        statuses.iter().all(|status| status.is_success()),
        "正常短时并行请求不应因验密槽已满而失败：{statuses:?}"
    );
}
