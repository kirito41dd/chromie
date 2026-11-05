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
    let (store, space, other) = fixture().await;
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
