mod support;

use chromie::{
    agent::UpdateAgent,
    entity::{recall_items, recall_runs},
    recall_usage::MemoryOrder,
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::{Correct, Forget, Recall, Remember, Scope, SecretCreate, SpaceId},
    web_session::WebSessions,
};
use migration::MigratorTrait;
use sea_orm::{ActiveModelTrait, ConnectOptions, Database, EntityTrait, IntoActiveModel, Set};
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "fictional-usage-password-only";

fn key() -> SpaceKey {
    let suffix: String = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .rev()
        .take(20)
        .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
        .collect();
    SpaceKey::parse(&format!("usage-{suffix}")).unwrap()
}

async fn fixture() -> (
    Store,
    String,
    SpaceKey,
    SpaceId,
    String,
    Uuid,
    tokio::task::JoinHandle<()>,
) {
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
    let slug = key();
    store
        .create_space(
            slug.clone(),
            format!("虚构召回统计验收-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let space = store.resolve_space(&slug).await.unwrap();
    let (agent, token, _) = support::grant(&store, space, false).await;
    let router = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (store, base, slug, space, token, agent, server)
}

async fn memory(store: &Store, space: SpaceId, content: String) -> Uuid {
    let value = store
        .remember(
            space,
            Remember {
                content,
                request_id: Uuid::now_v7(),
                scope: Scope::Global,
                source_refs: vec![],
            },
        )
        .await
        .unwrap();
    Uuid::parse_str(value["memory_id"].as_str().unwrap()).unwrap()
}

async fn rpc(base: &str, slug: &SpaceKey, token: &str, name: &str, args: Value) -> Value {
    let response = reqwest::Client::new().post(format!("{base}/spaces/{}/mcp", slug.as_str()))
        .bearer_auth(token).header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":"tools/call","params":{"name":name,"arguments":args}}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    value["result"].clone()
}

async fn recall(base: &str, slug: &SpaceKey, token: &str, query: &str, budget: usize) -> Value {
    let result = rpc(
        base,
        slug,
        token,
        "recall",
        json!({"query":query,"max_chars":budget}),
    )
    .await;
    assert_eq!(result["isError"], false);
    let value = result["structuredContent"].clone();
    assert!(value["recall_id"].is_string(), "统计写入必须实际成功");
    assert!(value.to_string().chars().count() <= budget);
    value
}

fn evaluation(run: &Value, evaluated: &[Uuid], useful: &[Uuid]) -> Value {
    json!({"recall_id":run["recall_id"],"evaluated_memory_ids":evaluated,"useful_memory_ids":useful})
}

async fn stats(store: &Store, space: SpaceId, id: Uuid) -> Value {
    let snapshot = store.dashboard(space).await.unwrap();
    let row = snapshot
        .memories
        .into_iter()
        .find(|m| m.memory_id == id.to_string())
        .unwrap();
    assert!(
        row.usage.is_some(),
        "聚合查询必须实际成功，不能静默丢失统计"
    );
    serde_json::to_value(row.usage.unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn feedback_is_owned_idempotent_atomic_revision_scoped_and_separate_from_web() {
    let (store, base, slug, space, token, agent, task) = fixture().await;
    let marker = format!("usage-{}", Uuid::now_v7());
    let a = memory(&store, space, format!("{marker}-A 虚构记忆甲")).await;
    let b = memory(&store, space, format!("{marker}-B 虚构记忆乙")).await;
    let c = memory(&store, space, format!("{marker}-C 虚构记忆丙")).await;
    let run = recall(&base, &slug, &token, &marker, 8000).await;
    assert_eq!(run["memories"].as_array().unwrap().len(), 3);
    let initial = stats(&store, space, a).await;
    assert_eq!(initial["agent_recall_count"], 1);
    assert_eq!(initial["evaluated_count"], 0);
    let feedback = json!({"recalls":[evaluation(&run, &[a,b], &[a])]});
    let (first, concurrent) = tokio::join!(
        rpc(&base, &slug, &token, "feedback_recall", feedback.clone()),
        rpc(&base, &slug, &token, "feedback_recall", feedback.clone()),
    );
    assert_eq!(first["isError"], false);
    assert_eq!(concurrent["isError"], false);
    assert_eq!(
        first["structuredContent"]["recorded"].as_u64().unwrap()
            + concurrent["structuredContent"]["recorded"]
                .as_u64()
                .unwrap(),
        2
    );
    assert_eq!(stats(&store, space, a).await["useful_count"], 1);
    assert_eq!(stats(&store, space, b).await["evaluated_count"], 1);
    assert_eq!(stats(&store, space, b).await["useful_count"], 0);
    assert_eq!(
        stats(&store, space, c).await["evaluated_count"],
        0,
        "未评价不能当作负面评价"
    );
    let (_, other_token, _) = support::grant(&store, space, false).await;
    let denied = rpc(
        &base,
        &slug,
        &other_token,
        "feedback_recall",
        feedback.clone(),
    )
    .await;
    assert_eq!(denied["structuredContent"]["code"], "not_found");

    let run2 = recall(&base, &slug, &token, &marker, 8000).await;
    let conflict = rpc(
        &base,
        &slug,
        &token,
        "feedback_recall",
        json!({"recalls":[
            evaluation(&run2, &[b], &[b]), evaluation(&run, &[a], &[])
        ]}),
    )
    .await;
    assert_eq!(
        conflict["structuredContent"]["code"],
        "recall_feedback_conflict"
    );
    let run2_id = Uuid::parse_str(run2["recall_id"].as_str().unwrap()).unwrap();
    let item = recall_items::Entity::find_by_id((
        space.bytes_for_test(),
        run2_id.as_bytes().to_vec(),
        b.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert!(item.useful.is_none(), "冲突之前的写入也必须回滚");

    let client = reqwest::Client::new();
    let prefix = format!("{base}/spaces/{}", slug.as_str());
    let login = client
        .post(format!("{prefix}/login"))
        .header("origin", &base)
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
    let web: Value = client
        .post(format!("{prefix}/recall"))
        .header("origin", &base)
        .header("cookie", &cookie)
        .json(&json!({"query":marker,"max_chars":8000}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(web["recall_id"].is_string());
    let after_web = stats(&store, space, a).await;
    assert_eq!(after_web["agent_recall_count"], 2);
    assert_eq!(after_web["web_recall_count"], 1);
    let denied = rpc(
        &base,
        &slug,
        &token,
        "feedback_recall",
        json!({"recalls":[evaluation(&web, &[a], &[a])]}),
    )
    .await;
    assert_eq!(denied["structuredContent"]["code"], "not_found");
    let invalid = client
        .get(format!("{prefix}/dashboard-data?order=invalid"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);

    store
        .correct(
            space,
            Correct {
                memory_id: a,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构修订验证".into(),
                content: Some(format!("{marker}-A 更正后的虚构记忆")),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    let current = stats(&store, space, a).await;
    assert_eq!(current["agent_recall_count"], 2);
    assert_eq!(current["evaluated_revision"], 2);
    assert_eq!(current["evaluated_count"], 0);
    assert_eq!(current["useful_count"], 0);
    let run3 = recall(&base, &slug, &token, &format!("{marker}-A"), 8000).await;
    assert_eq!(
        rpc(
            &base,
            &slug,
            &token,
            "feedback_recall",
            json!({"recalls":[evaluation(&run3,&[a],&[a])]})
        )
        .await["isError"],
        false
    );
    assert_eq!(stats(&store, space, a).await["useful_count"], 1);
    store
        .forget(
            space,
            Forget {
                memory_id: Some(a),
                event_id: None,
                expected_revision: Some(2),
                request_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rpc(
            &base,
            &slug,
            &token,
            "feedback_recall",
            json!({"recalls":[evaluation(&run3,&[a],&[a])]})
        )
        .await["structuredContent"]["code"],
        "not_found"
    );
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: Some(false),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    let response = client
        .post(format!("{prefix}/mcp"))
        .bearer_auth(token)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    task.abort();
}

// SpaceId 的字节仅由公开 UUID 转换，测试不构造可伪造的领域身份。
trait SpaceBytes {
    fn bytes_for_test(self) -> Vec<u8>;
}
impl SpaceBytes for SpaceId {
    fn bytes_for_test(self) -> Vec<u8> {
        self.as_uuid().as_bytes().to_vec()
    }
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn budget_filtering_and_unreturned_or_foreign_ids_do_not_create_false_counts() {
    let (store, base, slug, space, token, _, task) = fixture().await;
    let marker = format!("budget-{}", Uuid::now_v7());
    let mut ids = Vec::new();
    for n in 0..8 {
        ids.push(
            memory(
                &store,
                space,
                format!("{marker} 虚构正文 {n} {}", "长内容仅用于测试。".repeat(150)),
            )
            .await,
        );
    }
    let run = recall(&base, &slug, &token, &marker, 1024).await;
    let returned = run["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| Uuid::parse_str(m["memory_id"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(returned.len() < ids.len());
    assert_eq!(run["truncated"], true);
    for id in &ids {
        assert_eq!(
            stats(&store, space, *id).await["agent_recall_count"],
            u64::from(returned.contains(id))
        );
    }
    let omitted = ids.iter().find(|id| !returned.contains(id)).unwrap();
    assert_eq!(
        rpc(
            &base,
            &slug,
            &token,
            "feedback_recall",
            json!({"recalls":[evaluation(&run,&[*omitted],&[*omitted])]})
        )
        .await["structuredContent"]["code"],
        "not_found"
    );

    let other = key();
    store
        .create_space(
            other.clone(),
            format!("另一个虚构空间-{}", Uuid::now_v7()),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let other_space = store.resolve_space(&other).await.unwrap();
    let foreign = memory(&store, other_space, "仅存在其他空间的虚构记忆".into()).await;
    let (_, foreign_token, _) = support::grant(&store, other_space, false).await;
    let other_router = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_base = format!("http://{}", listener.local_addr().unwrap());
    let other_task =
        tokio::spawn(async move { axum::serve(listener, other_router).await.unwrap() });
    assert_eq!(
        rpc(
            &other_base,
            &other,
            &foreign_token,
            "feedback_recall",
            json!({"recalls":[evaluation(&run,&[foreign],&[foreign])]})
        )
        .await["structuredContent"]["code"],
        "not_found"
    );
    assert_eq!(
        rpc(
            &base,
            &slug,
            &token,
            "feedback_recall",
            json!({"recalls":[evaluation(&run,&[foreign],&[foreign])]})
        )
        .await["structuredContent"]["code"],
        "not_found"
    );
    let foreign_insert = recall_items::ActiveModel {
        space_id: Set(space.bytes_for_test()),
        recall_id: Set(Uuid::parse_str(run["recall_id"].as_str().unwrap())
            .unwrap()
            .as_bytes()
            .to_vec()),
        memory_id: Set(foreign.as_bytes().to_vec()),
        revision: Set(1),
        useful: Set(None),
        evaluated_at: Set(None),
    }
    .insert(&store.db)
    .await;
    assert!(foreign_insert.is_err(), "数据库外键也必须阻止跨空间关联");
    let empty = recall(&base, &slug, &token, "no-such-fictional-usage-marker", 3000).await;
    assert!(empty["memories"].as_array().unwrap().is_empty());
    task.abort();
    other_task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL 与 Qdrant"]
async fn popular_and_useful_orders_use_agent_activity_current_revision_and_thirty_days() {
    let (store, base, slug, space, token, _, task) = fixture().await;
    let marker = Uuid::now_v7().to_string();
    let a = memory(&store, space, format!("{marker}-A 虚构热门记忆")).await;
    let b = memory(&store, space, format!("{marker}-B 虚构有用记忆")).await;
    let old = memory(&store, space, format!("{marker}-OLD 仅旧活动")).await;
    for _ in 0..3 {
        recall(&base, &slug, &token, &format!("{marker}-A"), 3000).await;
    }
    for _ in 0..2 {
        let run = recall(&base, &slug, &token, &format!("{marker}-B"), 3000).await;
        assert_eq!(
            rpc(
                &base,
                &slug,
                &token,
                "feedback_recall",
                json!({"recalls":[evaluation(&run,&[b],&[b])]})
            )
            .await["isError"],
            false
        );
    }
    let run = recall(&base, &slug, &token, &format!("{marker}-OLD"), 3000).await;
    let id = Uuid::parse_str(run["recall_id"].as_str().unwrap()).unwrap();
    let mut row = recall_runs::Entity::find_by_id((space.bytes_for_test(), id.as_bytes().to_vec()))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    row.created_at = Set(chromie::types::now() - time::Duration::days(31));
    row.update(&store.db).await.unwrap();
    assert_eq!(stats(&store, space, old).await["agent_recall_count"], 1);
    let popular = store
        .dashboard_ordered(space, MemoryOrder::Popular)
        .await
        .unwrap();
    assert_eq!(
        popular
            .memories
            .iter()
            .map(|m| m.memory_id.clone())
            .collect::<Vec<_>>(),
        vec![a.to_string(), b.to_string()]
    );
    let useful = store
        .dashboard_ordered(space, MemoryOrder::Useful)
        .await
        .unwrap();
    assert_eq!(useful.memories.len(), 1);
    assert_eq!(useful.memories[0].memory_id, b.to_string());
    store
        .correct(
            space,
            Correct {
                memory_id: b,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                reason: "虚构修订".into(),
                content: Some(format!("{marker}-B 新修订")),
                scope: None,
                source_refs: None,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .dashboard_ordered(space, MemoryOrder::Useful)
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要一次性空隔离 MySQL 与 Qdrant"]
async fn version_eight_upgrade_preserves_memories_credentials_and_secret_ciphertext() {
    let url = std::env::var("CHROMIE_USAGE_UPGRADE_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let manager = migration::SchemaManager::new(&db);
    assert!(
        !manager.has_table("spaces").await.unwrap(),
        "升级用例必须使用空库"
    );
    migration::Migrator::up(&db, Some(8)).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    ))
    .await;
    let slug = key();
    store
        .create_space(slug.clone(), "仅用于升级的虚构空间".into(), PASSWORD.into())
        .await
        .unwrap();
    let space = store.resolve_space(&slug).await.unwrap();
    // 旧版夹具只写旧表，不能用新版业务写入路径假装运行旧版本。
    let note = Uuid::now_v7();
    let secret = Uuid::now_v7();
    let current = chromie::types::now();
    for (id, content) in [
        (note, "usage-upgrade-marker 虚构历史记忆"),
        (secret, "usage-upgrade-marker 虚构凭据"),
    ] {
        chromie::entity::memories::ActiveModel {
            space_id: Set(space.bytes_for_test()),
            id: Set(id.as_bytes().to_vec()),
            current_revision: Set(1),
            created_at: Set(current),
            updated_at: Set(current),
            forgotten_at: Set(None),
            agent_id: Set(None),
        }
        .insert(&store.db)
        .await
        .unwrap();
        chromie::entity::memory_revisions::ActiveModel {
            space_id: Set(space.bytes_for_test()),
            memory_id: Set(id.as_bytes().to_vec()),
            revision: Set(1),
            content: Set(Some(content.into())),
            project_key: Set(None),
            change_reason: Set(None),
            created_at: Set(current),
            agent_id: Set(None),
        }
        .insert(&store.db)
        .await
        .unwrap();
        chromie::entity::outbox_tasks::ActiveModel {
            space_id: Set(space.bytes_for_test()),
            memory_id: Set(id.as_bytes().to_vec()),
            target_revision: Set(1),
            state: Set("pending".into()),
            attempts: Set(0),
            run_after: Set(current),
            updated_at: Set(current),
        }
        .insert(&store.db)
        .await
        .unwrap();
    }
    let (agent, _, _) = support::grant(&store, space, false).await;
    // 固定虚构历史密文仅验证字节保留；真实加解密由 Secret 流程测试覆盖。
    chromie::entity::secrets::ActiveModel {
        space_id: Set(space.bytes_for_test()),
        memory_id: Set(secret.as_bytes().to_vec()),
        format_version: Set(1),
        payload_revision: Set(1),
        key_id: Set("fictional-upgrade-key".into()),
        nonce: Set(vec![3; 12]),
        ciphertext: Set(Some(vec![7; 64])),
        created_at: Set(current),
        updated_at: Set(current),
        contract: Set(None),
    }
    .insert(&store.db)
    .await
    .unwrap();
    let space_before = chromie::entity::spaces::Entity::find_by_id(space.bytes_for_test())
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    let secret_before = chromie::entity::secrets::Entity::find_by_id((
        space.bytes_for_test(),
        secret.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    let agent_before = chromie::entity::agent_connections::Entity::find_by_id((
        space.bytes_for_test(),
        agent.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    let request = || Recall {
        query: "usage-upgrade-marker".into(),
        project_key: None,
        max_chars: Some(8000),
    };
    let untracked = store.recall(space, request()).await.unwrap();
    assert_eq!(untracked["usage_available"], false);
    assert_eq!(
        untracked["memories"].as_array().unwrap().len(),
        2,
        "统计表缺失也不能阻断召回"
    );
    migration::Migrator::up(&store.db, None).await.unwrap();
    migration::Migrator::up(&store.db, None).await.unwrap();
    assert_eq!(
        migration::Migrator::get_applied_migrations(&store.db)
            .await
            .unwrap()
            .len(),
        15
    );
    store.initialize_model_admin(None).await.unwrap();
    assert_eq!(
        store.embedding_status(space).await.unwrap()["state"],
        "disabled",
        "升级不自动外发历史资料"
    );
    assert!(
        space_before
            == chromie::entity::spaces::Entity::find_by_id(space.bytes_for_test())
                .one(&store.db)
                .await
                .unwrap()
                .unwrap()
    );
    assert!(
        secret_before
            == chromie::entity::secrets::Entity::find_by_id((
                space.bytes_for_test(),
                secret.as_bytes().to_vec()
            ))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap()
    );
    assert!(
        agent_before
            == chromie::entity::agent_connections::Entity::find_by_id((
                space.bytes_for_test(),
                agent.as_bytes().to_vec()
            ))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap()
    );
    let tracked = store.recall(space, request()).await.unwrap();
    assert!(tracked["recall_id"].is_string());
    assert!(
        !tracked
            .to_string()
            .contains("fictional-private-payload-never-in-usage")
    );
    let record = recall_items::Entity::find_by_id((
        space.bytes_for_test(),
        Uuid::parse_str(tracked["recall_id"].as_str().unwrap())
            .unwrap()
            .as_bytes()
            .to_vec(),
        note.as_bytes().to_vec(),
    ))
    .one(&store.db)
    .await
    .unwrap()
    .unwrap();
    assert!(record.useful.is_none());
}
