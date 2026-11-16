mod support;
use chromie::{
    knowledge::{Link, MemoryType, Navigation, Neighbors, Relation},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Forget, GetMemory, Remember, Scope, SpaceId},
};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use serde_json::{Value, json};
use uuid::Uuid;

async fn fixture() -> (Store, SpaceId, SpaceId) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new("http://127.0.0.1:16334", None).unwrap(),
    ))
    .await;
    async fn space(store: &Store) -> SpaceId {
        let suffix: String = Uuid::now_v7()
            .simple()
            .to_string()
            .chars()
            .rev()
            .take(16)
            .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
            .collect();
        let key = SpaceKey::parse(&format!("graph-{suffix}")).unwrap();
        store
            .create_space(
                key.clone(),
                format!("虚构关系验收-{suffix}"),
                "fictional-graph-password".into(),
            )
            .await
            .unwrap();
        store.resolve_space(&key).await.unwrap()
    }
    let a = space(&store).await;
    let b = space(&store).await;
    (store, a, b)
}
fn remember(content: &str, kind: MemoryType, links: Vec<Link>) -> Remember {
    Remember {
        content: content.into(),
        request_id: Uuid::now_v7(),
        scope: Scope::Global,
        source_refs: vec![],
        memory_type: kind,
        links,
    }
}
fn id(value: &Value) -> Uuid {
    Uuid::parse_str(value["memory_id"].as_str().unwrap()).unwrap()
}
fn reference(id: Uuid) -> Link {
    Link {
        memory_id: id,
        relation: Relation::Reference,
        revision: None,
    }
}
fn derived(id: Uuid, revision: i32) -> Link {
    Link {
        memory_id: id,
        relation: Relation::DerivedFrom,
        revision: Some(revision),
    }
}
fn correct(id: Uuid, revision: i32, links: Option<Vec<Link>>) -> Correct {
    Correct {
        memory_id: id,
        expected_revision: revision,
        request_id: Uuid::now_v7(),
        reason: "虚构核对".into(),
        content: None,
        scope: None,
        source_refs: None,
        memory_type: None,
        links,
    }
}
async fn read(store: &Store, space: SpaceId, memory: Uuid) -> Value {
    store
        .get(
            space,
            serde_json::from_value::<GetMemory>(json!({"memory_id":memory,"max_chars":8000}))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn neighbors(store: &Store, space: SpaceId, memory: Uuid, incoming: bool) -> Value {
    store
        .memory_neighbors(
            space,
            Neighbors {
                memory_id: memory,
                incoming,
                cursor: None,
                project_key: None,
                max_chars: Some(8000),
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL"]
async fn relations_are_revisioned_idempotent_scoped_and_topics_are_discoverable() {
    let (store, space, other) = fixture().await;
    let note = id(&store
        .remember(
            space,
            remember("虚构数据库的检查流程", MemoryType::Note, vec![]),
        )
        .await
        .unwrap());
    let mut request = remember(
        "数据库运维\n按需阅读检查流程。",
        MemoryType::Topic,
        vec![reference(note)],
    );
    let first = store.remember(space, request.clone()).await.unwrap();
    let topic = id(&first);
    assert_eq!(
        store.remember(space, request.clone()).await.unwrap()["memory_id"],
        first["memory_id"]
    );
    assert_eq!(
        store
            .navigation(space, Navigation::default())
            .await
            .unwrap()["memories"][0]["memory_id"],
        topic.to_string()
    );
    assert_eq!(
        neighbors(&store, space, note, true).await["memories"][0]["memory_id"],
        topic.to_string()
    );
    let mut change = correct(topic, 1, None);
    change.content = Some("数据库运维\n更新入口介绍，保留原引用。".into());
    store.correct(space, change).await.unwrap();
    assert_eq!(
        neighbors(&store, space, topic, false).await["memories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        neighbors(&store, space, note, true).await["memories"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "旧修订不重复计入反向引用"
    );
    assert_eq!(read(&store, space, topic).await["memory_type"], "topic");
    request.request_id = Uuid::now_v7();
    assert!(
        store.remember(other, request).await.is_err(),
        "目标 ID 不能跨空间引用"
    );
    assert!(
        store
            .memory_neighbors(
                other,
                Neighbors {
                    memory_id: topic,
                    incoming: false,
                    cursor: None,
                    project_key: None,
                    max_chars: None
                }
            )
            .await
            .is_err()
    );
    store
        .correct(space, correct(topic, 2, Some(vec![])))
        .await
        .unwrap();
    assert!(
        neighbors(&store, space, note, true).await["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL"]
async fn derived_evidence_detects_changes_rejects_cycles_and_forgetting_purges_dependent_text() {
    let (store, space, _) = fixture().await;
    let note = id(&store
        .remember(
            space,
            remember("虚构环境的当前约定", MemoryType::Note, vec![]),
        )
        .await
        .unwrap());
    let summary = id(&store
        .remember(
            space,
            remember("虚构环境差异总结", MemoryType::Note, vec![derived(note, 1)]),
        )
        .await
        .unwrap());
    let topic = id(&store
        .remember(
            space,
            remember("虚构主题", MemoryType::Topic, vec![reference(note)]),
        )
        .await
        .unwrap());
    assert_eq!(
        read(&store, space, summary).await["knowledge"]["needs_review"],
        false
    );
    assert!(
        store
            .correct(space, correct(note, 1, Some(vec![derived(summary, 1)])))
            .await
            .is_err(),
        "派生环会使核对与遗忘无法收敛"
    );
    let mut change = correct(note, 1, None);
    change.content = Some("虚构环境约定已变更".into());
    store.correct(space, change).await.unwrap();
    assert_eq!(
        read(&store, space, summary).await["knowledge"]["needs_review"],
        true
    );
    assert!(
        store
            .remember(
                space,
                remember(
                    "未经重新核对的摘要",
                    MemoryType::Note,
                    vec![derived(note, 1)]
                )
            )
            .await
            .is_err()
    );
    store
        .correct(space, correct(summary, 1, Some(vec![derived(note, 2)])))
        .await
        .unwrap();
    assert_eq!(
        read(&store, space, summary).await["knowledge"]["needs_review"],
        false
    );
    store
        .forget(
            space,
            Forget {
                memory_id: Some(note),
                event_id: None,
                expected_revision: Some(2),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        read(&store, space, summary).await["hidden"],
        true,
        "被遗忘的信息不能残留在派生正文中"
    );
    assert_ne!(
        read(&store, space, topic).await["hidden"],
        true,
        "普通导航引用不扩大遗忘范围"
    );
    assert!(
        neighbors(&store, space, topic, false).await["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL"]
async fn navigation_respects_scope_and_small_budget_cursors_do_not_lose_topics() {
    let (store, space, _) = fixture().await;
    for index in 0..9 {
        let mut request = remember(
            &format!("虚构主题 {index} · 足够长的入口名称用于分页预算验证"),
            MemoryType::Topic,
            vec![],
        );
        request.scope = Scope::Project {
            project_key: if index == 0 { "other" } else { "main" }.into(),
        };
        store.remember(space, request).await.unwrap();
    }
    let mut cursor = None;
    let mut ids = std::collections::HashSet::new();
    loop {
        let page = store
            .navigation(
                space,
                Navigation {
                    project_key: Some("main".into()),
                    cursor,
                    max_chars: Some(1024),
                },
            )
            .await
            .unwrap();
        assert!(page.to_string().chars().count() <= 1024);
        for item in page["memories"].as_array().unwrap() {
            assert!(ids.insert(item["memory_id"].as_str().unwrap().to_owned()));
        }
        cursor = page["next_cursor"]
            .as_str()
            .map(|v| Uuid::parse_str(v).unwrap());
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 8);
}
