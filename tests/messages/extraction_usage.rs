use super::*;
use chromie::entity::{knowledge_jobs, memories, memory_revisions, recall_items, recall_runs};
use sea_orm::{ConnectionTrait, raw_sql};
use time::{Duration, PrimitiveDateTime};

async fn job(store: &Store, space: SpaceId, kind: &str, created_at: PrimitiveDateTime) -> Uuid {
    let id = Uuid::now_v7();
    knowledge_jobs::ActiveModel {
        space_id: Set(space.bytes()),
        id: Set(id.as_bytes().to_vec()),
        kind: Set(kind.into()),
        config_revision: Set(1),
        state: Set("done".into()),
        attempts: Set(1),
        run_after: Set(created_at),
        created_at: Set(created_at),
        updated_at: Set(now()),
        automatic: Set(1),
        ..Default::default()
    }
    .insert(&store.db)
    .await
    .unwrap();
    id
}

async fn memory(
    store: &Store,
    space: SpaceId,
    id: Uuid,
    job: Option<Uuid>,
    kind: &str,
    at: PrimitiveDateTime,
) {
    memories::ActiveModel {
        space_id: Set(space.bytes()),
        id: Set(id.as_bytes().to_vec()),
        current_revision: Set(1),
        created_at: Set(at),
        updated_at: Set(at),
        ..Default::default()
    }
    .insert(&store.db)
    .await
    .unwrap();
    memory_revisions::ActiveModel {
        space_id: Set(space.bytes()),
        memory_id: Set(id.as_bytes().to_vec()),
        revision: Set(1),
        memory_type: Set(kind.into()),
        content: Set(Some("仅测试统计，不返回这段虚构正文".into())),
        change_reason: Set(job.map(|id| format!("后台 LLM 整理：任务 {id}"))),
        created_at: Set(at),
        ..Default::default()
    }
    .insert(&store.db)
    .await
    .unwrap();
}

async fn hit(
    store: &Store,
    space: SpaceId,
    id: Uuid,
    channel: &str,
    vote: Option<bool>,
    reason: Option<&str>,
    at: PrimitiveDateTime,
) {
    let run = Uuid::now_v7().as_bytes().to_vec();
    recall_runs::ActiveModel {
        space_id: Set(space.bytes()),
        id: Set(run.clone()),
        channel: Set(channel.into()),
        created_at: Set(at),
        ..Default::default()
    }
    .insert(&store.db)
    .await
    .unwrap();
    recall_items::ActiveModel {
        space_id: Set(space.bytes()),
        recall_id: Set(run),
        memory_id: Set(id.as_bytes().to_vec()),
        revision: Set(1),
        useful: Set(vote.map(i8::from)),
        evaluated_at: Set(vote.map(|_| at)),
        negative_reason: Set(reason.map(str::to_owned)),
        ..Default::default()
    }
    .insert(&store.db)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL"]
async fn only_new_live_extracted_notes_and_explicit_agent_feedback_count() {
    let (store, space, other) = fixture().await;
    assert_eq!(store.extraction_usage(space).await.unwrap()["generated"], 0);
    let now = now();
    let at = now - Duration::days(3);
    let extract = job(&store, space, "extract", now - Duration::days(10)).await;
    let organize = job(&store, space, "organize", at).await;
    let other_extract = job(&store, other, "extract", at).await;
    let [
        rated,
        recalled,
        unseen,
        topic,
        manual,
        generated_by_organize,
        forgotten,
        old,
        future,
        secret,
    ] = std::array::from_fn(|_| Uuid::now_v7());
    for (id, from, kind, when) in [
        (rated, Some(extract), "note", at),
        (recalled, Some(extract), "note", at),
        (unseen, Some(extract), "note", at),
        (topic, Some(extract), "topic", at),
        (manual, None, "note", at),
        (generated_by_organize, Some(organize), "note", at),
        (forgotten, Some(extract), "note", at),
        (old, Some(extract), "note", now - Duration::days(8)),
        (future, Some(extract), "note", now + Duration::days(1)),
        (secret, Some(extract), "note", at),
    ] {
        memory(&store, space, id, from, kind, when).await;
    }
    let sid = space.bytes();
    let mid = forgotten.as_bytes().to_vec();
    store
        .db
        .execute_raw(raw_sql!(
            MySql,
            "UPDATE memories SET forgotten_at={now} WHERE space_id={sid} AND id={mid}"
        ))
        .await
        .unwrap();
    let mid = secret.as_bytes().to_vec();
    store.db.execute_raw(raw_sql!(MySql, "INSERT INTO secrets (space_id,memory_id,format_version,payload_revision,key_id,nonce,ciphertext,created_at,updated_at) VALUES ({sid},{mid},1,1,'fictional',UNHEX('000000000000000000000000'),NULL,{at},{at})")).await.unwrap();
    // 手工新建后由后台修订仍属于手工新建；提炼新建后发生修订仍保留在原分母。
    for id in [manual, rated] {
        let mid = id.as_bytes().to_vec();
        let label = format!("后台 LLM 整理：任务 {extract}");
        store.db.execute_raw(raw_sql!(MySql,"INSERT INTO memory_revisions (space_id,memory_id,revision,memory_type,content,change_reason,created_at) VALUES ({sid},{mid},2,'note','已修订',{label},{now})")).await.unwrap();
        store
            .db
            .execute_raw(raw_sql!(
                MySql,
                "UPDATE memories SET current_revision=2 WHERE space_id={sid} AND id={mid}"
            ))
            .await
            .unwrap();
    }
    // 另一空间有完全相同的记忆 ID，同空间三元组才构成召回。
    memory(&store, other, unseen, Some(other_extract), "note", at).await;
    hit(
        &store,
        other,
        unseen,
        "agent",
        Some(false),
        Some("missing_detail"),
        now,
    )
    .await;
    hit(
        &store,
        space,
        unseen,
        "web",
        Some(false),
        Some("missing_detail"),
        now,
    )
    .await;
    hit(&store, space, unseen, "internal", Some(true), None, now).await;
    hit(
        &store,
        space,
        unseen,
        "agent",
        Some(true),
        None,
        at - Duration::minutes(1),
    )
    .await;
    hit(
        &store,
        space,
        unseen,
        "agent",
        Some(true),
        None,
        now + Duration::days(1),
    )
    .await;
    hit(&store, space, recalled, "agent", None, None, now).await;
    for (vote, reason) in [
        (true, None),
        (false, Some("scope_mismatch")),
        (false, Some("scope_mismatch")),
        (false, None),
        (false, Some("wrong_result_type")),
        (false, Some("missing_detail")),
    ] {
        hit(&store, space, rated, "agent", Some(vote), reason, now).await;
    }
    for id in [manual, generated_by_organize, forgotten, old, secret, topic] {
        hit(
            &store,
            space,
            id,
            "agent",
            Some(false),
            Some("missing_detail"),
            now,
        )
        .await;
    }
    let mut stats = store.extraction_usage(space).await.unwrap();
    let parse = |v: Value| {
        time::OffsetDateTime::parse(
            v.as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    };
    let start = parse(
        stats
            .as_object_mut()
            .unwrap()
            .remove("window_start")
            .unwrap(),
    );
    let end = parse(stats.as_object_mut().unwrap().remove("window_end").unwrap());
    assert_eq!(end - start, Duration::days(7));
    assert_eq!(
        stats,
        json!({"generated":3,"recalled":2,"evaluated":1,"not_recalled":1,
        "negative_reasons":{"scope_mismatch":2,"wrong_result_type":1,"missing_detail":1,"unspecified":1}})
    );
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL"]
async fn compaction_preserves_create_provenance_and_recovers_verified_legacy_notes() {
    use chromie::entity::write_receipts;
    use chromie::types::Correct;
    use sha2::{Digest, Sha256};

    let (store, space, _) = fixture().await;
    let at = now() - Duration::days(1);
    let origin = job(&store, space, "extract", at).await;
    let id = Uuid::now_v7();
    memory(&store, space, id, Some(origin), "note", at).await;
    let marker = format!("后台 LLM 整理：任务 {origin}");
    for revision in 1..=6 {
        store
            .correct(
                space,
                Correct {
                    memory_id: id,
                    expected_revision: revision,
                    request_id: Uuid::now_v7(),
                    content: Some(format!("修订 {} 的虚构正文", revision + 1)),
                    reason: "后台 LLM 自由文本不能作为持久来源".into(),
                    scope: None,
                    source_refs: None,
                    memory_type: None,
                    links: None,
                },
            )
            .await
            .unwrap();
    }
    let sid = space.bytes();
    let mid = id.as_bytes().to_vec();
    let jid = origin.as_bytes().to_vec();
    let first = memory_revisions::Entity::find_by_id((sid.clone(), mid.clone(), 1))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    let second = memory_revisions::Entity::find_by_id((sid.clone(), mid.clone(), 2))
        .one(&store.db)
        .await
        .unwrap()
        .unwrap();
    assert!(first.content.is_none() && second.content.is_none());
    assert_eq!(first.change_reason.as_deref(), Some(marker.as_str()));
    let legacy_marker = "后台 LLM 整理（历史正文已清理）";
    assert_eq!(
        second.change_reason.as_deref(),
        Some(legacy_marker),
        "自由文本已清理"
    );
    assert_eq!(store.extraction_usage(space).await.unwrap()["generated"], 1);
    store.db.execute_raw(raw_sql!(MySql,"UPDATE memory_revisions SET change_reason={legacy_marker} WHERE space_id={sid} AND memory_id={mid} AND revision=1")).await.unwrap();
    assert_eq!(
        store.extraction_usage(space).await.unwrap()["generated"],
        0,
        "无法核实的旧记录不作推断"
    );
    let decision =
        json!({"decisions":[{"action":"create","published_memory_id":id,"reason":"虚构证据"}]});
    store
        .db
        .execute_raw(raw_sql!(
            MySql,
            "UPDATE knowledge_jobs SET result={decision} WHERE space_id={sid} AND id={jid}"
        ))
        .await
        .unwrap();
    assert_eq!(
        store.extraction_usage(space).await.unwrap()["generated"],
        1,
        "旧创建决策仍保留"
    );
    store
        .db
        .execute_raw(raw_sql!(
            MySql,
            "UPDATE knowledge_jobs SET result=NULL WHERE space_id={sid} AND id={jid}"
        ))
        .await
        .unwrap();
    write_receipts::ActiveModel {
        space_id: Set(space.bytes()),
        operation: Set("background_revision".into()),
        request_id: Set(Uuid::now_v7().as_bytes().to_vec()),
        input_hash: Set(Sha256::digest(serde_json::to_vec(&(id, 1, origin)).unwrap()).to_vec()),
        target_id: Set(mid),
        revision: Set(Some(1)),
        created_at: Set(at),
        agent_id: Set(None),
    }
    .insert(&store.db)
    .await
    .unwrap();
    assert_eq!(
        store.extraction_usage(space).await.unwrap()["generated"],
        1,
        "详情已清理时仍可核对首版回执"
    );
}
