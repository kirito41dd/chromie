use chromie::{
    entity::source_events,
    messages::{MessagesQuery, SessionsQuery},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{SpaceId, now},
    web_session::WebSessions,
};
use migration::MigratorTrait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, QueryFilter, Set,
};
use serde_json::{Value, json};
use uuid::Uuid;
#[allow(dead_code)]
mod support;
const PASSWORD: &str = "fictional-messages-test-password";

async fn fixture() -> (Store, SpaceId, SpaceId) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new("http://127.0.0.1:9", None).unwrap(),
    ))
    .await;
    let mut spaces = Vec::new();
    for _ in 0..2 {
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
                format!("虚构消息测试-{suffix}"),
                PASSWORD.into(),
            )
            .await
            .unwrap();
        spaces.push(store.resolve_space(&slug).await.unwrap());
    }
    (store, spaces[0], spaces[1])
}
async fn insert(
    store: &Store,
    space: SpaceId,
    agent: Option<Uuid>,
    session: &str,
    body: &str,
    offset: i64,
    received: time::PrimitiveDateTime,
) -> Uuid {
    let id = Uuid::now_v7();
    source_events::ActiveModel {
        space_id: Set(space.bytes()),
        id: Set(id.as_bytes().to_vec()),
        agent_id: Set(agent.map(|id| id.as_bytes().to_vec())),
        client_type: Set("codex".into()),
        session_id: Set(session.into()),
        turn_id: Set(Some(format!("turn-{offset}"))),
        role: Set(if offset % 2 == 0 { "user" } else { "assistant" }.into()),
        body: Set(Some(body.into())),
        project_key: Set(Some("fictional-project".into())),
        occurred_at: Set(received + time::Duration::seconds(offset)),
        received_at: Set(received),
        forgotten_at: Set(None),
    }
    .insert(&store.db)
    .await
    .unwrap();
    id
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn conversations_isolate_space_and_agent_and_paginate_a_stable_snapshot() {
    let (mut store, space, other) = fixture().await;
    let (a, _, _) = support::grant(&store, space, false).await;
    let (b, _, _) = support::grant(&store, space, false).await;
    let at = now() - time::Duration::seconds(200);
    for index in 0..32 {
        insert(
            &store,
            space,
            Some(a),
            "shared-session",
            &format!("虚构消息-{index}"),
            index,
            at,
        )
        .await;
    }
    insert(
        &store,
        space,
        Some(b),
        "shared-session",
        "另一个 Agent 的消息",
        0,
        at,
    )
    .await;
    insert(
        &store,
        other,
        None,
        "shared-session",
        "其他空间不可泄露",
        0,
        at,
    )
    .await;
    let hidden = insert(&store, space, None, "hidden-session", "已遗忘正文", 0, at).await;
    source_events::Entity::update_many()
        .col_expr(
            source_events::Column::ForgottenAt,
            sea_orm::sea_query::Expr::value(now()),
        )
        .filter(source_events::Column::SpaceId.eq(space.bytes()))
        .filter(source_events::Column::Id.eq(hidden.as_bytes().to_vec()))
        .exec(&store.db)
        .await
        .unwrap();
    let first = store
        .message_sessions(space, SessionsQuery::default())
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(
        first
            .items
            .iter()
            .find(|c| c.conversation.agent_id == Some(a))
            .unwrap()
            .message_count,
        32
    );
    let filtered = store
        .message_sessions(
            space,
            SessionsQuery {
                agent_id: Some(b),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert!(filtered.items[0].preview.contains("另一个"));
    let page = store
        .conversation_messages(
            space,
            MessagesQuery {
                agent_id: Some(a),
                session_id: "shared-session".into(),
                page: 0,
                as_of: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 30);
    assert!(page.has_more);
    assert_eq!(page.items.first().unwrap().preview, "虚构消息-2");
    assert_eq!(page.items.last().unwrap().preview, "虚构消息-31");
    insert(
        &store,
        space,
        Some(a),
        "shared-session",
        "快照之后的新消息",
        100,
        now(),
    )
    .await;
    let earlier = store
        .conversation_messages(
            space,
            MessagesQuery {
                agent_id: Some(a),
                session_id: "shared-session".into(),
                page: 1,
                as_of: Some(page.as_of),
            },
        )
        .await
        .unwrap();
    assert_eq!(earlier.items.len(), 2);
    assert!(!earlier.has_more);
    assert_eq!(earlier.items[0].preview, "虚构消息-0");
    assert_eq!(earlier.items[1].preview, "虚构消息-1");
    assert!(store.captured_message(space, hidden).await.is_err());
    let long = "中文长消息".repeat(1000);
    let full = insert(&store, space, Some(a), "long", &long, 0, at).await;
    let detail = store.captured_message(space, full).await.unwrap();
    assert_eq!(detail["body"], long);
    assert_eq!(detail["truncated"], false);
    assert!(store.captured_message(other, full).await.is_err());
    let rows = store
        .conversation_messages(
            space,
            MessagesQuery {
                agent_id: Some(a),
                session_id: "long".into(),
                page: 0,
                as_of: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.items[0].preview.chars().count(), 360);
    assert!(rows.items[0].truncated);
    for index in 0..25 {
        insert(
            &store,
            space,
            Some(b),
            &format!("paged-{index:02}"),
            "虚构分页会话",
            0,
            at,
        )
        .await;
    }
    let selects = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let query_count = selects.clone();
    store.db.set_metric_callback(move |_| {
        query_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    let start = std::time::Instant::now();
    let first_page = store
        .message_sessions(
            space,
            SessionsQuery {
                agent_id: Some(b),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eprintln!(
        "分页 20 个会话：{} 次 SQL，耗时 {:?}",
        selects.load(std::sync::atomic::Ordering::Relaxed),
        start.elapsed()
    );
    assert!(
        (1..=3).contains(&selects.load(std::sync::atomic::Ordering::Relaxed)),
        "读取一页摘要的查询次数应与会话数无关"
    );
    assert_eq!(first_page.items.len(), 20);
    assert!(first_page.has_more);
    insert(
        &store,
        space,
        Some(b),
        "after-snapshot",
        "快照之后的新会话",
        0,
        now(),
    )
    .await;
    let second_page = store
        .message_sessions(
            space,
            SessionsQuery {
                agent_id: Some(b),
                page: 1,
                as_of: Some(first_page.as_of),
            },
        )
        .await
        .unwrap();
    assert_eq!(second_page.items.len(), 6);
    assert!(!second_page.has_more);
    for item in &second_page.items {
        assert!(
            !first_page
                .items
                .iter()
                .any(|first| first.conversation.session_id == item.conversation.session_id)
        );
        assert_ne!(item.conversation.session_id, "after-snapshot");
    }
    assert!(
        store
            .message_sessions(
                space,
                SessionsQuery {
                    page: 501,
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn session_summaries_keep_snapshot_previews_and_current_extraction_separate() {
    use chromie::{entity::extraction_items, messages::Conversation, types::timestamp};
    let (store, space, _) = fixture().await;
    let (agent, _, _) = support::grant(&store, space, false).await;
    let at = now() - time::Duration::seconds(10);
    let session = "shared-summary";
    let first = insert(&store, space, Some(agent), session, "最早用户消息", 0, at).await;
    let long = "这是带中文的很长预览".repeat(32);
    insert(&store, space, Some(agent), session, &long, 1, at).await;
    insert(&store, space, None, session, "历史助手独立会话", 1, at).await;
    insert(
        &store,
        space,
        Some(agent),
        "Shared-Summary",
        "大小写不同的会话",
        0,
        at,
    )
    .await;
    let fallback = store
        .message_sessions(space, SessionsQuery::default())
        .await
        .unwrap();
    let named = fallback
        .items
        .iter()
        .find(|row| {
            row.conversation.agent_id == Some(agent) && row.conversation.session_id == session
        })
        .unwrap();
    assert_eq!(named.title, "最早用户消息");
    let legacy = fallback
        .items
        .iter()
        .find(|row| row.conversation.agent_id.is_none())
        .unwrap();
    assert_eq!(legacy.title, "未命名会话");
    assert_eq!(
        legacy.origin,
        json!({"agent_id":null,"agent_name":"历史记录"})
    );
    store
        .set_conversation_title(
            space,
            Conversation {
                agent_id: Some(agent),
                session_id: session.into(),
            },
            0,
            Some("固定标题".into()),
        )
        .await
        .unwrap();
    extraction_items::ActiveModel {
        space_id: Set(space.bytes()),
        event_id: Set(first.as_bytes().to_vec()),
        config_revision: Set(1),
        state: Set("done".into()),
        job_id: Set(None),
        run_after: Set(now()),
        last_error: Set(None),
        updated_at: Set(now()),
    }
    .insert(&store.db)
    .await
    .unwrap();
    let later = at + time::Duration::seconds(2);
    insert(
        &store,
        space,
        Some(agent),
        session,
        "快照外的新消息",
        0,
        later,
    )
    .await;
    insert(
        &store,
        space,
        None,
        session,
        "This is an app-generated request for a suggested next user message. example",
        0,
        later,
    )
    .await;
    let page = store
        .message_sessions(
            space,
            SessionsQuery {
                as_of: Some(timestamp(at + time::Duration::seconds(1)).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    for row in page.items {
        if row.conversation.session_id == "Shared-Summary" {
            assert_eq!(row.title, "大小写不同的会话");
            assert_eq!(row.extraction, json!({"total":1,"completed":0,"pending":1}));
            continue;
        }
        let detail = store
            .conversation_messages(
                space,
                MessagesQuery {
                    agent_id: row.conversation.agent_id,
                    session_id: session.into(),
                    page: 0,
                    as_of: Some(page.as_of.clone()),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            row.extraction,
            detail.conversation_info.unwrap()["extraction"]
        );
        if row.conversation.agent_id == Some(agent) {
            assert_eq!(row.message_count, 2);
            assert_eq!(row.title, "固定标题");
            assert_eq!(row.preview, long.chars().take(120).collect::<String>());
            assert_eq!(
                row.extraction,
                json!({"total":3, "completed":1, "pending":2})
            );
        } else {
            assert_eq!(row.message_count, 1);
            assert_eq!(row.title, "未命名会话");
            assert_eq!(row.preview, "历史助手独立会话");
            assert_eq!(
                row.extraction,
                json!({"total":2,"completed":0,"pending":0,"excluded":2,"state":"auxiliary"})
            );
        }
    }
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn web_browsing_requires_current_space_cookie_and_same_origin() {
    let (store, space, other) = fixture().await;
    let (a, mcp, hook) = support::grant(&store, space, false).await;
    let id = insert(
        &store,
        space,
        Some(a),
        "http-session",
        "虚构 HTTP 消息",
        0,
        now(),
    )
    .await;
    let slug = store.space_key(space).await.unwrap().as_str().to_owned();
    let other_slug = store.space_key(other).await.unwrap().as_str().to_owned();
    let router = chromie::http::router_with_sessions(
        store,
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let prefix = format!("{base}/spaces/{slug}/messages");
    let response = client
        .post(format!("{base}/spaces/{slug}/login"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap();
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let usage_url = format!("{base}/spaces/{slug}/knowledge/extraction-usage");
    assert_eq!(client.get(&usage_url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(format!(
                "{base}/spaces/{other_slug}/knowledge/extraction-usage"
            ))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .get(&usage_url)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["generated"], 0);
    for response in [
        client
            .post(format!("{prefix}/sessions"))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{prefix}/sessions"))
            .header("x-chromie-password", PASSWORD)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{prefix}/sessions"))
            .bearer_auth(&mcp)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{prefix}/sessions"))
            .bearer_auth(&hook)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{base}/spaces/{other_slug}/messages/sessions"))
            .header("cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{prefix}/sessions"))
            .header("cookie", &cookie)
            .header("origin", "https://untrusted.example")
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), 401);
    }
    let response = client
        .post(format!("{prefix}/sessions"))
        .header("cookie", &cookie)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let sessions: Value = response.json().await.unwrap();
    assert_eq!(sessions["items"][0]["session_id"], "http-session");
    let response = client
        .post(format!("{prefix}/conversation"))
        .header("cookie", &cookie)
        .json(&json!({"agent_id":a,"session_id":"http-session","page":0,"as_of":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["items"][0]["event_id"],
        id.to_string()
    );
    assert_eq!(
        client
            .get(format!("{prefix}/events/{id}"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{prefix}/sessions"))
            .header("cookie", &cookie)
            .json(&json!({"page":501}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    task.abort();
}

#[path = "messages/extraction_usage.rs"]
mod extraction_usage;
