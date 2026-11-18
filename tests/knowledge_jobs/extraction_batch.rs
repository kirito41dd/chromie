//! 在隔离的 MySQL 和 Qdrant 空间中检查两阶段提炼产生的实际核对快照。
use super::*;
use chromie::entity::{memories, memory_links, memory_revisions, secrets};
use std::collections::BTreeMap;

fn fixed_id(n: u128) -> Uuid {
    // 排除项的 ID 排在有效候选之前；错误纳入它们会改变断言中的前四名。
    Uuid::from_u128(if n >= 505 {
        n
    } else {
        0x40000000000040008000000000000000 + n
    })
}

#[tokio::test]
#[ignore = "需要隔离的 MySQL 和 Qdrant"]
async fn related_notes_keep_current_links_and_index_hits_outside_recent_window() {
    check_related_window(true).await;
}

#[tokio::test]
#[ignore = "需要隔离的 MySQL 和 Qdrant"]
async fn exactly_five_hundred_current_notes_keep_complete_coverage() {
    check_related_window(false).await;
}

async fn check_related_window(over_window: bool) {
    let mut f = fixture().await;
    f.store.search = Search::new(
        "http://127.0.0.1:16334",
        std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
    )
    .unwrap();
    save(&f, "fictional-extraction-window").await;
    let now = chromie::types::now();
    let other = SpaceKey::parse("fictional-neighbor").unwrap();
    f.store
        .create_space(
            other.clone(),
            "虚构相邻空间".into(),
            "fictional-local-password".into(),
        )
        .await
        .unwrap();
    let other = f.store.resolve_space(&other).await.unwrap();
    let mut heads = Vec::new();
    let mut revisions = Vec::new();
    // 1 是超过扫描窗口的索引命中；后四条是遗忘、主题、Secret 和相邻空间的同词干扰。
    for n in 1u16..=508 {
        let id = fixed_id(n.into()).as_bytes().to_vec();
        let owner = if n == 508 { other } else { f.space };
        let version = if n == 501 { 2 } else { 1 };
        let timestamp = now + time::Duration::seconds(i64::from(if n == 5 { 4 } else { n }));
        heads.push(memories::ActiveModel {
            space_id: Set(owner.bytes()),
            id: Set(id.clone()),
            current_revision: Set(version),
            created_at: Set(now),
            updated_at: Set(timestamp),
            forgotten_at: Set((n == 505 || !over_window && (100..=103).contains(&n)).then_some(now)),
            agent_id: Set(None),
        });
        for revision in 1..=version {
            let content = if revision != version {
                "旧修订不得发往模型"
            } else if n == 1 || n == 5 || n >= 501 {
                "数据库 检查"
            } else {
                "虚构旅行路线"
            };
            revisions.push(memory_revisions::ActiveModel {
                space_id: Set(owner.bytes()),
                memory_id: Set(id.clone()),
                revision: Set(revision),
                memory_type: Set(if n == 506 { "topic" } else { "note" }.into()),
                content: Set(Some(format!("{content} {n}"))),
                project_key: Set(None),
                created_at: Set(now),
                change_reason: Set(None),
                agent_id: Set(None),
            });
        }
    }
    memories::Entity::insert_many(heads)
        .exec(&f.store.db)
        .await
        .unwrap();
    memory_revisions::Entity::insert_many(revisions)
        .exec(&f.store.db)
        .await
        .unwrap();
    secrets::ActiveModel {
        space_id: Set(f.space.bytes()),
        memory_id: Set(fixed_id(507).as_bytes().to_vec()),
        format_version: Set(1),
        payload_revision: Set(1),
        key_id: Set("fictional".into()),
        nonce: Set(vec![0; 12]),
        ciphertext: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        contract: Set(None),
    }
    .insert(&f.store.db)
    .await
    .unwrap();
    for (revision, target) in [(1, 2), (2, 5), (2, 4)] {
        memory_links::ActiveModel {
            space_id: Set(f.space.bytes()),
            memory_id: Set(fixed_id(501).as_bytes().to_vec()),
            revision: Set(revision),
            target_id: Set(fixed_id(target).as_bytes().to_vec()),
            relation: Set("reference".into()),
            target_revision: Set(None),
        }
        .insert(&f.store.db)
        .await
        .unwrap();
    }
    f.store
        .search
        .upsert(f.space, fixed_id(1), 1, "数据库 检查 1", None)
        .await
        .unwrap();
    let event = captured(&f, "fictional-window", "用户确认每周四检查数据库。", now).await;
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();

    let statements = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let recorded = statements.clone();
    f.store.db.set_metric_callback(move |info| {
        *recorded
            .lock()
            .unwrap()
            .entry(info.statement.sql.clone())
            .or_default() += 1;
    });
    f.store.sync_knowledge_due().await.unwrap();
    let counts = statements.lock().unwrap().clone();
    let staged = job(&f, id).await;
    let payload = staged.payload.as_ref().unwrap();
    let related = payload["memories"].as_array().unwrap();
    assert_eq!(staged.state, "pending", "{:?}", staged.last_error);
    assert_eq!(payload["review"]["retrieval_partial"], over_window);
    let ids = related
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    // 4 与 5 的更新时间相同。窗口截断时按 ID 留下 4；未截断则同词的 5 入围。
    let expected = if over_window {
        [1, 501, 502, 503]
    } else {
        [1, 5, 501, 502]
    }
    .map(|n| fixed_id(n).to_string());
    assert_eq!(ids, expected);
    let second_revision = &related[if over_window { 1 } else { 2 }];
    assert_eq!(second_revision["revision"], 2);
    assert_eq!(second_revision["content"], "数据库 检查 501");
    let expected_links = [4, 5].map(|n| json!({"memory_id":fixed_id(n),"relation":"reference"}));
    assert_eq!(second_revision["links"], json!(expected_links));

    let total: usize = counts.values().sum();
    let links: usize = counts
        .iter()
        .filter(|(sql, _)| sql.contains("FROM `memory_links`"))
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(links, 1, "同一批入围记忆应一次补齐对应修订的关系");
    let bodies: usize = counts
        .iter()
        .filter(|(sql, _)| {
            sql.contains("LEFT JOIN `secrets`") && sql.contains("FROM `memory_revisions`")
        })
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(
        bodies,
        if over_window { 2 } else { 1 },
        "最近窗口一次读完，仅窗口外命中需要补读正文"
    );
    eprintln!(
        "核对快照={}，检索范围受限={}，第一阶段 SQL={}，其中关系 SELECT={}",
        json!(related),
        payload["review"]["retrieval_partial"],
        total,
        links
    );
}
