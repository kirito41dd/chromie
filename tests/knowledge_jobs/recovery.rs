//! 生产巡检回归：晚入库的后台记忆不能冒充人工新决定，辅助来源不能沉淀为事实。
use super::*;
use chromie::entity::{extraction_items, memory_revisions, source_events, write_receipts};
use sea_orm::{ColumnTrait, QueryFilter, sea_query::Expr};

pub(super) async fn make_job_due(f: &Fixture, id: Uuid) {
    knowledge_jobs::Entity::update_many()
        .col_expr(
            knowledge_jobs::Column::RunAfter,
            Expr::value(chromie::types::now() - time::Duration::seconds(1)),
        )
        .filter(knowledge_jobs::Column::SpaceId.eq(f.space.bytes()))
        .filter(knowledge_jobs::Column::Id.eq(id.as_bytes().to_vec()))
        .exec(&f.store.db)
        .await
        .unwrap();
}

async fn extract(f: &Fixture, event: Uuid, target: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: target.into_iter().collect(),
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    finish_stage(f).await;
    id
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；验证生产旧后台来源与人工修订的区别"]
async fn legacy_backend_creation_can_be_updated_but_manual_revision_stays_protected() {
    let f = fixture().await;
    save(&f, "fictional-legacy-recovery").await;
    let old = captured(
        &f,
        "legacy-created",
        "用户最初确认每周四检查。",
        chromie::types::now() - time::Duration::days(3),
    )
    .await;
    let origin = extract(&f, old, None).await;
    let target: Uuid =
        serde_json::from_value(job(&f, origin).await.result.unwrap()["memory_ids"][0].clone())
            .unwrap();
    // 只移除新版本内部标记，模拟已完成任务详情被压缩的真实旧后台首版。
    write_receipts::Entity::delete_many()
        .filter(write_receipts::Column::SpaceId.eq(f.space.bytes()))
        .filter(write_receipts::Column::Operation.eq("background_revision"))
        .filter(write_receipts::Column::TargetId.eq(target.as_bytes().to_vec()))
        .exec(&f.store.db)
        .await
        .unwrap();
    let mut row = job(&f, origin).await.into_active_model();
    row.result = Set(Some(json!({"detail_expired":true,"memory_count":1})));
    row.update(&f.store.db).await.unwrap();
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let outdated = captured(
        &f,
        "legacy-created",
        "用户更早说过周五检查。",
        chromie::types::now() - time::Duration::days(4),
    )
    .await;
    let outdated_job = extract(&f, outdated, Some(target)).await;
    assert_eq!(
        job(&f, outdated_job).await.result.unwrap()["decisions"][0]["rejection_code"],
        "newer_source",
        "认可后台来源只取消入库时间误判，不能取消真实证据的先后顺序"
    );
    let later = captured(
        &f,
        "legacy-created",
        "用户确认今后每周五检查数据库。",
        chromie::types::now() - time::Duration::days(2),
    )
    .await;
    let updated = extract(&f, later, Some(target)).await;
    assert_eq!(
        job(&f, updated).await.result.unwrap()["decisions"][0]["resolution"],
        "automatic"
    );
    let current = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(current["revision"], 2);
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: target,
                expected_revision: 2,
                request_id: Uuid::now_v7(),
                reason: format!("后台 LLM 整理：任务 {origin}"),
                content: Some("用户今天明确改为每周一检查。".into()),
                scope: None,
                source_refs: None,
                memory_type: None,
                links: None,
            },
        )
        .await
        .unwrap();
    let rejected = extract(&f, later, Some(target)).await;
    let result = job(&f, rejected).await.result.unwrap();
    assert_eq!(
        result["decisions"][0]["rejection_code"],
        "newer_manual_revision"
    );
    assert_eq!(
        result["decisions"][0]["revision_target"]["memory_id"],
        json!(target)
    );
    assert_eq!(result["decisions"][0]["action"], "defer");
    assert_eq!(
        f.store
            .get(
                f.space,
                serde_json::from_value(json!({"memory_id":target})).unwrap()
            )
            .await
            .unwrap()["revision"],
        3
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；仅有首版说明不能伪造后台来源"]
async fn legacy_marker_requires_matching_backend_job_and_creation_receipt() {
    let f = fixture().await;
    save(&f, "fictional-forged-origin").await;
    let event = captured(
        &f,
        "forged-origin",
        "用户确认今后每周五检查数据库。",
        chromie::types::now() - time::Duration::days(1),
    )
    .await;
    let target = memory(&f).await;
    memory_revisions::Entity::update_many()
        .col_expr(
            memory_revisions::Column::ChangeReason,
            Expr::value(format!("后台 LLM 整理：任务 {}", Uuid::now_v7())),
        )
        .filter(memory_revisions::Column::SpaceId.eq(f.space.bytes()))
        .filter(memory_revisions::Column::MemoryId.eq(target.as_bytes().to_vec()))
        .exec(&f.store.db)
        .await
        .unwrap();
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let id = extract(&f, event, Some(target)).await;
    assert_eq!(
        job(&f, id).await.result.unwrap()["decisions"][0]["rejection_code"],
        "newer_manual_revision"
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；辅助会话保留原文但自动／手动／在途发布均不提炼"]
async fn auxiliary_sessions_are_excluded_without_deleting_capture_or_crossing_agent_scope() {
    let f = fixture().await;
    save(&f, "fictional-auxiliary-filter").await;
    let mut ids = vec![];
    for (i, prefix) in [
        "Your task is to generate a very short title for a conversation based on the user's first message.",
        "This is an app-generated request for a suggested next user message.",
    ].iter().enumerate() {
        let session = format!("auxiliary-{i}");
        let answer = captured(&f, &session, "虚构助手回答", chromie::types::now()).await;
        let mut row = source_events::Entity::find_by_id((f.space.bytes(), answer.as_bytes().to_vec())).one(&f.store.db).await.unwrap().unwrap().into_active_model();
        row.role = Set("assistant".into()); row.update(&f.store.db).await.unwrap();
        let queued = Uuid::now_v7();
        f.store.enqueue_knowledge(f.space, Queue { request_id:queued, kind:JobKind::Extract, memory_ids:vec![], event_ids:vec![answer] }).await.unwrap();
        // Stop 先到，原始辅助 UserPromptSubmit 后到：在途快照也不能趁机发布。
        let prompt = captured(&f, &session, &format!("{prefix} 虚构内部任务"), chromie::types::now()).await;
        finish_stage(&f).await;
        assert_eq!(job(&f, queued).await.state, "stale");
        assert!(f.store.enqueue_knowledge(f.space, Queue { request_id:Uuid::now_v7(), kind:JobKind::Extract, memory_ids:vec![], event_ids:vec![answer] }).await.is_err());
        let info = conversation_info(&f, &session).await;
        assert_eq!(info["extraction"]["state"], "auxiliary");
        assert_eq!(info["extraction"]["pending"], 0);
        assert_eq!(info["extraction"]["total"], 2);
        ids.extend([prompt, answer]);
    }
    // 同 session 的另一 Agent 不继承辅助分类。
    let (agent, _, _) = support::grant(&f.store, f.space, false).await;
    let normal = captured(
        &f,
        "auxiliary-0",
        "用户明确确认虚构业务操作。",
        chromie::types::now(),
    )
    .await;
    let mut row = source_events::Entity::find_by_id((f.space.bytes(), normal.as_bytes().to_vec()))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    row.agent_id = Set(Some(agent.as_bytes().to_vec()));
    row.update(&f.store.db).await.unwrap();
    extraction_config(&f, true, false, 100).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 1);
    finish_stage(&f).await;
    assert_eq!(extraction_state(&f, normal).await.as_deref(), Some("done"));
    for id in ids {
        assert!(
            source_events::Entity::find_by_id((f.space.bytes(), id.as_bytes().to_vec()))
                .one(&f.store.db)
                .await
                .unwrap()
                .unwrap()
                .body
                .is_some()
        );
        assert!(
            extraction_state(&f, id).await.is_none(),
            "原文仍可浏览，不创建消耗预算的提炼任务"
        );
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；短暂截断可恢复，持续截断有界且旧阻断只恢复一次"]
async fn truncated_output_retries_once_and_legacy_block_recovers_without_infinite_requeue() {
    let f = fixture().await;
    save(&f, "fictional-output-recovery").await;
    let event = captured(
        &f,
        "truncated-recovery",
        "虚构检查已确认。",
        chromie::types::now(),
    )
    .await;
    extraction_config(&f, true, false, 100).await;
    f.model.mode.store(19, Ordering::SeqCst);
    f.store.schedule_extraction().await.unwrap();
    let row = knowledge_jobs::Entity::find()
        .filter(knowledge_jobs::Column::State.eq("pending"))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    let id = Uuid::from_slice(&row.id).unwrap();
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "retry");
    assert_eq!(extraction_state(&f, event).await.as_deref(), Some("queued"));
    f.model.mode.store(5, Ordering::SeqCst);
    make_job_due(&f, id).await;
    finish_stage(&f).await;
    assert_eq!(job(&f, id).await.state, "done");
    // 模拟旧版本的一次失败阻断；任务小元数据仍在，超过一天后可恢复一次。
    let mut old = job(&f, id).await.into_active_model();
    old.state = Set("failed".into());
    old.last_error = Set(Some("model_output_truncated".into()));
    old.updated_at = Set(chromie::types::now() - time::Duration::days(2));
    old.result = Set(Some(json!({"diagnostic":{"automatic_retry":false}})));
    old.update(&f.store.db).await.unwrap();
    extraction_items::Entity::update_many()
        .col_expr(extraction_items::Column::State, Expr::value("blocked"))
        .col_expr(
            extraction_items::Column::RunAfter,
            Expr::value(chromie::types::now() - time::Duration::days(1)),
        )
        .filter(extraction_items::Column::EventId.eq(event.as_bytes().to_vec()))
        .exec(&f.store.db)
        .await
        .unwrap();
    f.model.mode.store(19, Ordering::SeqCst);
    organization_due(&f).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 1);
    let item = extraction_items::Entity::find_by_id((f.space.bytes(), event.as_bytes().to_vec()))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    let retry_id = Uuid::from_slice(&item.job_id.unwrap()).unwrap();
    finish_stage(&f).await;
    make_job_due(&f, retry_id).await;
    finish_stage(&f).await;
    assert_eq!(job(&f, retry_id).await.attempts, 2);
    assert_eq!(
        extraction_state(&f, event).await.as_deref(),
        Some("blocked")
    );
    organization_due(&f).await;
    assert_eq!(f.store.schedule_extraction().await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；核对阶段的重试不受生成阶段领取次数挤占"]
async fn review_phase_has_its_own_output_retry_limit() {
    let f = fixture().await;
    save(&f, "fictional-review-output-recovery").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    let event = captured(
        &f,
        "review-output-recovery",
        "用户确认今后每周五检查数据库。",
        chromie::types::now(),
    )
    .await;
    f.model.mode.store(29, Ordering::SeqCst);
    let id = extract(&f, event, Some(target)).await;
    let failed = job(&f, id).await;
    assert_eq!(failed.state, "retry");
    assert_eq!(failed.attempts, 2, "第一次领取生成，第二次领取核对");
    assert_eq!(failed.result.unwrap()["diagnostic"]["phase_attempts"], 1);
    f.model.mode.store(7, Ordering::SeqCst);
    make_job_due(&f, id).await;
    finish_stage(&f).await;
    let done = job(&f, id).await;
    assert_eq!(done.state, "done");
    assert_eq!(done.attempts, 3);
    assert_eq!(
        done.result.unwrap()["decisions"][0]["resolution"],
        "automatic"
    );
    {
        let requests = f.model.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(_, _, r)| r["kind"] == "extract")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(_, _, r)| r["kind"] == "review")
                .count(),
            2
        );
    }
    // 持续在核对阶段截断：只重试核对一次，既不重生成，也不无限重试。
    f.model.mode.store(29, Ordering::SeqCst);
    let again = extract(&f, event, Some(target)).await;
    assert_eq!(job(&f, again).await.state, "retry");
    make_job_due(&f, again).await;
    finish_stage(&f).await;
    let stopped = job(&f, again).await;
    assert_eq!(stopped.state, "failed");
    assert_eq!(stopped.attempts, 3);
    assert_eq!(
        stopped.result.unwrap()["diagnostic"]["automatic_retry"],
        false
    );
}
