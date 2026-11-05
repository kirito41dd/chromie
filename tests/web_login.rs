use chromie::{
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Event, Forget, Recall, Remember, Scope, now, timestamp},
    web_session::WebSessions,
};
use cookie::{Cookie, SameSite};
use migration::MigratorTrait;
use p256::{
    SecretKey,
    elliptic_curve::Generate,
    pkcs8::{EncodePrivateKey, LineEnding},
};
use sea_orm::{ConnectOptions, Database};
use serde_json::Value;
use uuid::Uuid;

const PASSWORD: &str = "仅用于登录测试的虚构密码-123";

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

async fn fixture() -> (Store, SpaceKey, SpaceKey, WebSessions) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    );
    let work = key("web");
    let life = key("other");
    store
        .create_space(
            work.clone(),
            format!("虚构登录空间-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    store
        .create_space(
            life.clone(),
            format!("另一虚构登录空间-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let pem = SecretKey::try_generate()
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    let sessions = WebSessions::from_pem(&pem).unwrap();
    (store, work, life, sessions)
}

async fn server(
    store: Store,
    sessions: WebSessions,
    origin: Option<String>,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = chromie::http::router_with_sessions(store, origin, sessions)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, handle)
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 和 Qdrant"]
async fn login_cookie_is_persistent_space_scoped_and_revoked_by_password_change() {
    let (store, work, life, sessions) = fixture().await;
    let work_space = store.resolve_space(&work).await.unwrap();
    let content = format!(
        "{}【完整正文末尾，不能被详情截掉】",
        "虚构跨设备操作记忆。".repeat(80)
    );
    store
        .remember(
            work_space,
            Remember {
                content: content.clone(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    let (base, task) = server(store.clone(), sessions.clone(), None).await;
    let prefix = format!("{base}/spaces/{}", work.as_str());
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{prefix}/dashboard-data"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let wrong = client
        .post(format!("{prefix}/login"))
        .header("origin", &base)
        .header("x-chromie-password", "错误的虚构测试密码")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    assert!(wrong.headers().get("set-cookie").is_none());
    assert_eq!(wrong.headers()["cache-control"], "no-store");
    let logged = client
        .post(format!("{prefix}/login"))
        .header("origin", &base)
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(logged.status(), 204);
    assert_eq!(logged.headers()["cache-control"], "no-store");
    let cookie =
        Cookie::parse(logged.headers()["set-cookie"].to_str().unwrap().to_owned()).unwrap();
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Strict));
    assert_eq!(
        cookie.path().unwrap(),
        format!("/spaces/{}/", work.as_str())
    );
    assert!(cookie.domain().is_none());
    assert_ne!(cookie.secure(), Some(true), "仅限回环 HTTP 调试");
    assert_eq!(cookie.max_age().unwrap().whole_days(), 30);
    assert_eq!(
        jsonwebtoken::decode_header(cookie.value()).unwrap().alg,
        jsonwebtoken::Algorithm::ES256
    );
    assert!(
        logged.text().await.unwrap().is_empty(),
        "JWT 不进入响应正文"
    );
    let credential = format!("{}={}", cookie.name(), cookie.value());
    let response = client
        .get(format!("{prefix}/dashboard-data"))
        .header("cookie", &credential)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let snapshot: Value = response.json().await.unwrap();
    assert_eq!(snapshot["space_id"], work.as_str());
    assert_eq!(snapshot["memory_count"], 1);
    assert_eq!(snapshot["memories"][0]["content"], content);
    assert_eq!(
        snapshot["memories"][0]["preview"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        360
    );
    assert!(
        !snapshot["memories"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("完整正文末尾")
    );
    assert_eq!(
        client
            .get(format!("{base}/spaces/{}/dashboard-data", life.as_str()))
            .header("cookie", &credential)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for endpoint in ["mcp", "events"] {
        assert_eq!(
            client
                .post(format!("{prefix}/{endpoint}"))
                .header("cookie", &credential)
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "看板 Cookie 不能认证 MCP 或采集"
        );
    }
    assert_eq!(
        client
            .get(format!("{prefix}/dashboard-data"))
            .header("cookie", format!("{credential}; {credential}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let logout = client
        .post(format!("{prefix}/logout"))
        .header("cookie", &credential)
        .header("origin", &base)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 204);
    let removed =
        Cookie::parse(logout.headers()["set-cookie"].to_str().unwrap().to_owned()).unwrap();
    assert_eq!(removed.max_age().unwrap().whole_seconds(), 0);
    assert_eq!(removed.path(), cookie.path());
    assert_eq!(
        client
            .get(format!("{prefix}/dashboard-data"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    task.abort();
    let _ = task.await;
    let (restarted, task) = server(store.clone(), sessions, None).await;
    let data = format!("{restarted}/spaces/{}/dashboard-data", work.as_str());
    assert_eq!(
        client
            .get(&data)
            .header("cookie", &credential)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "同一密钥重建路由后，未到期 token 仍有效；退出只清除浏览器 Cookie"
    );
    store
        .admin_set_password(work_space.as_uuid(), "已更新的虚构测试密码-456".into())
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&data)
            .header("cookie", &credential)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let login = format!("{restarted}/spaces/{}/login", work.as_str());
    assert_eq!(
        client
            .post(&login)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&login)
            .header("x-chromie-password", "已更新的虚构测试密码-456")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 和 Qdrant"]
async fn web_session_rejects_cross_site_origins_and_uses_secure_cookies_for_https() {
    let (store, work, _, sessions) = fixture().await;
    let (base, task) = server(store.clone(), sessions.clone(), None).await;
    let login = format!("{base}/spaces/{}/login", work.as_str());
    let logout = format!("{base}/spaces/{}/logout", work.as_str());
    let client = reqwest::Client::new();
    for endpoint in [&login, &logout] {
        assert_eq!(
            client
                .post(endpoint)
                .header("origin", "https://untrusted.example")
                .header("x-chromie-password", PASSWORD)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .post(endpoint)
                .header("origin", &base)
                .header("sec-fetch-site", "cross-site")
                .header("x-chromie-password", PASSWORD)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(
        client
            .post(&login)
            .header("host", "rebound.example")
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    task.abort();
    let (base, task) = server(
        store.clone(),
        sessions.clone(),
        Some("https://chromie.example.com".into()),
    )
    .await;
    let response = client
        .post(format!("{base}/spaces/{}/login", work.as_str()))
        .header("origin", "https://chromie.example.com")
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let cookie = Cookie::parse(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    assert_eq!(cookie.secure(), Some(true));
    assert!(
        chromie::http::router_with_sessions(
            store,
            Some("ftp://chromie.example.com".into()),
            sessions
        )
        .await
        .is_err()
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 和 Qdrant"]
async fn vpn_private_http_origin_has_working_cookie_and_keeps_origin_isolation() {
    let (store, work, _, sessions) = fixture().await;
    let origin = "http://192.0.2.10:8732";
    let (base, task) = server(store.clone(), sessions.clone(), Some(origin.into())).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/spaces/{}/login", work.as_str()))
        .header("origin", origin)
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let cookie = Cookie::parse(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    assert_ne!(cookie.secure(), Some(true));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Strict));
    let token = format!("{}={}", cookie.name(), cookie.value());
    for (request_origin, expected) in [(origin, 200), ("http://192.0.2.11:8732", 401)] {
        let response = client
            .get(format!("{base}/spaces/{}/dashboard-data", work.as_str()))
            .header("origin", request_origin)
            .header("cookie", &token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert!(
        chromie::http::router_with_sessions(store, Some("ftp://8.8.8.8:8732".into()), sessions)
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 和 Qdrant"]
async fn web_recall_reuses_domain_search_and_enforces_space_and_cookie_boundaries() {
    let (store, work, life, sessions) = fixture().await;
    let space = store.resolve_space(&work).await.unwrap();
    let other = store.resolve_space(&life).await.unwrap();
    let event_id = Uuid::now_v7();
    store
        .capture(
            space,
            Event {
                id: event_id,
                client_type: "web-recall-test".into(),
                session_id: Uuid::now_v7().to_string(),
                turn_id: None,
                role: "user".into(),
                body: "rawevidenceonly 虚构原始消息，不应直接召回".into(),
                project_key: None,
                occurred_at: timestamp(now()).unwrap(),
            },
        )
        .await
        .unwrap();
    let written = store
        .remember(
            space,
            Remember {
                content: "webrecall alpha 当前空间的虚构记忆".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![event_id],
            },
        )
        .await
        .unwrap();
    let id = Uuid::parse_str(written["memory_id"].as_str().unwrap()).unwrap();
    store
        .remember(
            other,
            Remember {
                content: "webrecall 另一个空间的虚构记忆".into(),
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    store.sync_due().await.unwrap();
    let (base, task) = server(store.clone(), sessions.clone(), None).await;
    let prefix = format!("{base}/spaces/{}", work.as_str());
    let endpoint = format!("{prefix}/recall");
    let client = reqwest::Client::new();
    let input = serde_json::json!({"query":"webrecall","max_chars":8000});
    for page in ["dashboard", "recall"] {
        let response = client.get(format!("{prefix}/{page}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "GET 页面提供静态登录壳");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("text/html")
        );
        let html = response.text().await.unwrap();
        assert!(html.contains("id=\"canvas\""));
        assert!(!html.contains("当前空间的虚构记忆"));
    }
    let denied = client.post(&endpoint).json(&input).send().await.unwrap();
    assert_eq!(denied.status(), 401);
    assert_eq!(denied.headers()["cache-control"], "no-store");
    let login = client
        .post(format!("{prefix}/login"))
        .header("origin", &base)
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 204);
    let cookie = Cookie::parse(login.headers()["set-cookie"].to_str().unwrap().to_owned()).unwrap();
    let credential = format!("{}={}", cookie.name(), cookie.value());
    let response = client
        .post(&endpoint)
        .header("cookie", &credential)
        .header("origin", &base)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let actual: Value = response.json().await.unwrap();
    let mut expected = store
        .recall(
            space,
            Recall {
                query: "webrecall".into(),
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap();
    assert!(
        expected.get("debug").is_none(),
        "普通 MCP 结果不附加 Web 诊断"
    );
    let mut without_debug = actual.clone();
    let debug = without_debug
        .as_object_mut()
        .unwrap()
        .remove("debug")
        .unwrap();
    let web_recall_id = without_debug
        .as_object_mut()
        .unwrap()
        .remove("recall_id")
        .unwrap();
    let internal_recall_id = expected
        .as_object_mut()
        .unwrap()
        .remove("recall_id")
        .unwrap();
    assert!(web_recall_id.is_string());
    assert!(internal_recall_id.is_string());
    assert_ne!(web_recall_id, internal_recall_id, "不同调用使用独立召回 ID");
    for hit in debug["hits"].as_array().unwrap() {
        let usage = &hit["usage"];
        assert_eq!(usage["web_recall_count"], 1);
        assert_eq!(usage["agent_recall_count"], 0, "Web 调试不混入 Agent 热度");
    }
    assert_eq!(without_debug, expected, "Web 与 MCP 复用相同的领域结果");
    assert_eq!(debug["qdrant"]["tokenizer"], "multilingual");
    assert_eq!(debug["qdrant"]["tokens_available"], false);
    assert_eq!(debug["qdrant"]["candidates"], 1);
    assert_eq!(debug["qdrant"]["validated"], 1);
    assert_eq!(debug["qdrant"]["discarded"], 0);
    assert_eq!(debug["hits"][0]["memory_id"], id.to_string());
    assert_eq!(debug["hits"][0]["path"], "qdrant_bm25");
    assert!(debug["hits"][0]["bm25_score"].as_f64().unwrap() > 0.0);
    assert_eq!(debug["hits"][0]["indexed_revision"], 1);
    assert_eq!(debug["hits"][0]["score_matches_revision"], true);
    assert!(debug["timing"]["qdrant_server_ms"].as_f64().unwrap() >= 0.0);
    assert!(
        debug["timing"]["total_ms"].as_f64().unwrap()
            >= debug["timing"]["index_call_ms"].as_f64().unwrap()
    );
    assert_eq!(debug["selection"]["returned"], 1);
    assert!(!debug.to_string().contains("虚构记忆"), "诊断不复制正文");
    assert_eq!(actual["memories"].as_array().unwrap().len(), 1);
    assert_eq!(actual["memories"][0]["memory_id"], id.to_string());
    assert_eq!(
        actual["memories"][0]["source_refs"][0],
        event_id.to_string()
    );
    assert!(!actual.to_string().contains("rawevidenceonly"));
    assert!(actual.to_string().chars().count() <= 8000);
    let mut bounded: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&serde_json::json!({"query":"webrecall","max_chars":1024}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bounded_debug = bounded.as_object_mut().unwrap().remove("debug").unwrap();
    assert!(bounded.to_string().chars().count() <= 1024);
    let ids = bounded["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["memory_id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(bounded_debug["selection"]["returned"], ids.len());
    assert!(
        bounded_debug["hits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| ids.contains(&hit["memory_id"]))
    );
    let raw: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&serde_json::json!({"query":"rawevidenceonly"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(raw["memories"].as_array().unwrap().is_empty());
    assert_eq!(
        client
            .post(format!("{base}/spaces/{}/recall", life.as_str()))
            .header("cookie", &credential)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&endpoint)
            .header("cookie", &credential)
            .header("origin", "https://untrusted.example")
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for invalid in [
        serde_json::json!({"query":" "}),
        serde_json::json!({"query":"中".repeat(86)}),
        serde_json::json!({"query":"webrecall","space_id":life.as_str()}),
        serde_json::json!({"query":"webrecall","max_chars":8001}),
        serde_json::json!({"query":"x".repeat(5000)}),
    ] {
        let response = client
            .post(&endpoint)
            .header("cookie", &credential)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    store
        .correct(
            space,
            Correct {
                memory_id: id,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "测试更正".into(),
                content: Some("updatedrecall beta 更正后的虚构记忆".into()),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    let old: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(old["memories"].as_array().unwrap().is_empty());
    assert_eq!(
        old["debug"]["qdrant"]["discarded"], 1,
        "更正后丢弃旧索引修订"
    );
    assert!(old["debug"]["hits"].as_array().unwrap().is_empty());
    let updated_input = serde_json::json!({"query":"updatedrecall","max_chars":8000});
    let updated: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&updated_input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["memories"][0]["revision"], 2);
    assert_eq!(updated["debug"]["hits"][0]["path"], "pending_keyword");
    assert!(updated["debug"]["hits"][0]["bm25_score"].is_null());
    assert_eq!(
        updated["debug"]["fallback"]["terms"],
        serde_json::json!(["updatedrecall"])
    );
    assert!(
        updated["memories"][0]["content"]
            .as_str()
            .unwrap()
            .contains("更正后")
    );

    let offline = Store::new(
        store.db.clone(),
        Search::new("http://127.0.0.1:9", None).unwrap(),
    );
    let (degraded, degraded_task) = server(offline, sessions, None).await;
    let fallback: Value = client
        .post(format!("{degraded}/spaces/{}/recall", work.as_str()))
        .header("cookie", &credential)
        .json(&updated_input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fallback["index_available"], false);
    assert_eq!(fallback["partial"], true);
    assert_eq!(fallback["memories"][0]["memory_id"], id.to_string());
    assert_eq!(fallback["debug"]["fallback"]["mode"], "recent_keyword");
    assert_eq!(fallback["debug"]["hits"][0]["path"], "recent_keyword");
    assert!(fallback["debug"]["hits"][0]["bm25_score"].is_null());
    assert!(fallback["debug"]["timing"]["qdrant_server_ms"].is_null());
    assert_eq!(fallback["debug"]["selection"]["returned"], 1);
    degraded_task.abort();

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
    let forgotten: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&updated_input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(forgotten["memories"].as_array().unwrap().is_empty());
    assert!(forgotten["debug"]["hits"].as_array().unwrap().is_empty());
    let mut budget_ids = Vec::new();
    for index in 0..4 {
        let saved = store
            .remember(
                space,
                Remember {
                    content: format!(
                        "debugbudget {index} {}",
                        "仅用于预算测试的虚构长正文".repeat(120)
                    ),
                    request_id: Uuid::now_v7(),
                    scope: Scope::Global,
                    source_refs: vec![],
                },
            )
            .await
            .unwrap();
        budget_ids.push(saved["memory_id"].clone());
    }
    let mut limited: Value = client
        .post(&endpoint)
        .header("cookie", &credential)
        .json(&serde_json::json!({"query":"debugbudget","max_chars":1024}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let diagnostic = limited.as_object_mut().unwrap().remove("debug").unwrap();
    assert_eq!(limited["truncated"], true);
    assert!(limited.to_string().chars().count() <= 1024);
    let visible = limited["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["memory_id"].clone())
        .collect::<Vec<_>>();
    assert!(!visible.is_empty() && visible.len() < budget_ids.len());
    assert_eq!(diagnostic["hits"].as_array().unwrap().len(), visible.len());
    for hidden_id in budget_ids.iter().filter(|id| !visible.contains(id)) {
        assert!(
            !diagnostic.to_string().contains(hidden_id.as_str().unwrap()),
            "诊断不泄漏预算裁去的 ID"
        );
    }
    store
        .admin_set_password(space.as_uuid(), "更换后的虚构测试密码-456".into())
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&endpoint)
            .header("cookie", &credential)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    task.abort();
}
