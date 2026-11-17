//! AI 自主发布、旧建议复核与有限历史的集成边界；只使用隔离库中的虚构资料。
use super::*;
use chromie::entity::memory_revisions;
use sea_orm::{ColumnTrait, QueryFilter, QueryOrder};

async fn legacy_suggestion(f: &Fixture, target: Uuid, event: Uuid, quote: &str) -> Uuid {
    let id = Uuid::now_v7();
    knowledge_jobs::ActiveModel {
        space_id:Set(f.space.bytes()), id:Set(id.as_bytes().to_vec()),kind:Set("extract".into()),
        config_revision:Set(1),state:Set("done".into()),payload:Set(None),
        result:Set(Some(json!({"pending_revisions":1,"decisions":[{
            "candidate_index":0,"action":"propose_revision","applied":false,
            "category":"fact","reason":"虚构历史建议","content":"虚构数据库检查已调整为周五。",
            "replacement":{"memory_id":target,"expected_revision":1,"content":"旧建议不是直接批准的正文"},
            "source_refs":[event],"evidence":[{"event_id":event,"quote":quote}]
        }]}))),lease_owner:Set(None),lease_expires_at:Set(None),attempts:Set(2),run_after:Set(chromie::types::now()),
        created_at:Set(chromie::types::now()),updated_at:Set(chromie::types::now()),last_error:Set(None),automatic:Set(1),
    }.insert(&f.store.db).await.unwrap();
    id
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；旧建议重新读取当前资料，自动发布后其余旧方案收敛"]
async fn autonomous_rechecks_pending_suggestions_and_supersedes_duplicates() {
    let f = fixture().await;
    save(&f, "fictional-autonomous").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let quote = "用户确认今后每周五检查数据库。";
    let event = captured(&f, "autonomous-backlog", quote, chromie::types::now()).await;
    let older = legacy_suggestion(&f, target, event, quote).await;
    let newer = legacy_suggestion(&f, target, event, quote).await;
    extraction_config(&f, true, false, 100).await;
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 1);
    let staged = knowledge_jobs::Entity::find()
        .filter(knowledge_jobs::Column::SpaceId.eq(f.space.bytes()))
        .filter(knowledge_jobs::Column::State.eq("pending"))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        staged.payload.as_ref().unwrap()["memories"][0]["revision"],
        1
    );
    assert!(
        staged.payload.as_ref().unwrap()["review"].is_object(),
        "旧建议直接进入核对阶段，仍用真实来源校验"
    );
    f.store.sync_knowledge_due().await.unwrap();
    let done = job(&f, Uuid::from_slice(&staged.id).unwrap()).await;
    assert_eq!(done.state, "done", "{:?}", done.last_error);
    assert_eq!(
        done.result.as_ref().unwrap()["decisions"][0]["resolution"],
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
    assert!(current["content"].as_str().unwrap().contains("每周五"));
    for id in [older, newer] {
        assert_eq!(
            f.store.knowledge_review(f.space, id).await.unwrap()["result"]["pending_revisions"],
            0
        );
    }
    assert_eq!(
        f.store.knowledge_review(f.space, newer).await.unwrap()["result"]["decisions"][0]["resolution"],
        "rechecked"
    );
    assert_eq!(
        f.store.knowledge_review(f.space, older).await.unwrap()["result"]["decisions"][0]["resolution"],
        "superseded"
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；旧建议的错误引文只形成暂缓，不改当前记忆或继续要求审批"]
async fn autonomous_recheck_defers_invalid_old_evidence_without_requeueing_it() {
    let f = fixture().await;
    save(&f, "fictional-bad-legacy-proof").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let event = captured(
        &f,
        "bad-legacy",
        "与更改检查日期无关的虚构原话",
        chromie::types::now(),
    )
    .await;
    let old = legacy_suggestion(&f, target, event, "不存在于原始消息的更正").await;
    extraction_config(&f, true, false, 100).await;
    f.store.schedule_automatic().await.unwrap();
    f.store.sync_knowledge_due().await.unwrap();
    let row = knowledge_jobs::Entity::find()
        .filter(knowledge_jobs::Column::SpaceId.eq(f.space.bytes()))
        .filter(knowledge_jobs::Column::Id.ne(old.as_bytes().to_vec()))
        .order_by_desc(knowledge_jobs::Column::CreatedAt)
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.result.as_ref().unwrap()["decisions"][0]["action"],
        "defer"
    );
    let head = chromie::entity::memories::Entity::find_by_id((
        f.space.bytes(),
        target.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(head.current_revision, 1);
    assert_eq!(
        f.store.knowledge_review(f.space, old).await.unwrap()["result"]["pending_revisions"],
        0
    );
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；暂停或重设任务后，旧建议可重新调度，不遗留复核中状态"]
async fn paused_recheck_releases_parent_and_resumes_with_fresh_snapshot() {
    let f = fixture().await;
    save(&f, "fictional-recheck-pause").await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(7, Ordering::SeqCst);
    let quote = "用户确认今后每周五检查数据库。";
    let event = captured(&f, "recheck-pause", quote, chromie::types::now()).await;
    let old = legacy_suggestion(&f, target, event, quote).await;
    extraction_config(&f, true, false, 100).await;
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 1);
    assert_eq!(
        job(&f, old).await.result.unwrap()["decisions"][0]["resolution"],
        "rechecking"
    );
    extraction_config(&f, false, false, 100).await;
    let parent = job(&f, old).await.result.unwrap();
    assert_eq!(parent["pending_revisions"], 1);
    assert!(parent["decisions"][0]["resolution"].is_null());
    assert!(parent["decisions"][0].get("review_job_id").is_none());
    assert_eq!(f.store.sync_knowledge_due().await.unwrap(), 0);
    extraction_config(&f, true, false, 100).await;
    assert_eq!(f.store.schedule_automatic().await.unwrap(), 1);
    f.store.sync_knowledge_due().await.unwrap();
    assert_eq!(
        job(&f, old).await.result.unwrap()["decisions"][0]["resolution"],
        "rechecked"
    );
    let head = chromie::entity::memories::Entity::find_by_id((
        f.space.bytes(),
        target.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(head.current_revision, 2);
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；标题历史实际限制为十条，当前身份与递增修订不变"]
async fn title_history_keeps_ten_names_and_current_revision() {
    let f = fixture().await;
    let session = "fictional-long-conversation";
    captured(&f, session, "虚构长期会话", chromie::types::now()).await;
    let key = chromie::messages::Conversation {
        agent_id: None,
        session_id: session.into(),
    };
    for n in 1..=13 {
        let info = conversation_info(&f, session).await;
        f.store
            .set_conversation_title(
                f.space,
                key.clone(),
                info["revision"].as_i64().unwrap(),
                Some(format!("虚构标题{n}")),
            )
            .await
            .unwrap();
    }
    let info = conversation_info(&f, session).await;
    assert_eq!(info["title"], "虚构标题13");
    assert_eq!(info["revision"], 13);
    let rows = chromie::entity::conversation_titles::Entity::find()
        .filter(chromie::entity::conversation_titles::Column::SpaceId.eq(f.space.bytes()))
        .order_by_asc(chromie::entity::conversation_titles::Column::Revision)
        .all(&f.store.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0].revision, 4);
    // 模拟升级前已存在的标题，不再调用命名入口，周期维护也应回收存量。
    for revision in 1..=3 {
        chromie::entity::conversation_titles::ActiveModel {
            space_id: Set(f.space.bytes()),
            conversation_key: Set(rows[0].conversation_key.clone()),
            revision: Set(revision),
            title: Set(format!("虚构旧标题{revision}")),
            source: Set("automatic".into()),
            created_at: Set(chromie::types::now()),
        }
        .insert(&f.store.db)
        .await
        .unwrap();
    }
    assert!(f.store.compact_history().await.unwrap() >= 3);
    let retained = chromie::entity::conversation_titles::Entity::find()
        .filter(chromie::entity::conversation_titles::Column::SpaceId.eq(f.space.bytes()))
        .all(&f.store.db)
        .await
        .unwrap();
    assert_eq!(retained.len(), 10);
    assert_eq!(
        conversation_info(&f, session).await,
        info,
        "清理标题历史不修改当前元数据"
    );
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；新增用户决定必须有用户引文，不能将助手建议升级为用户决定"]
async fn decision_creation_requires_user_evidence() {
    let f = fixture().await;
    save(&f, "fictional-decision-proof").await;
    f.model.mode.store(27, Ordering::SeqCst);
    for role in ["assistant", "user"] {
        let event = captured(
            &f,
            "decision-proof",
            "虚构用户决定每周四检查数据库。",
            chromie::types::now(),
        )
        .await;
        let mut row = chromie::entity::source_events::Entity::find_by_id((
            f.space.bytes(),
            event.as_bytes().to_vec(),
        ))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
        row.role = Set(role.into());
        row.update(&f.store.db).await.unwrap();
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
        finish_stage(&f).await;
        let result = job(&f, id).await.result.unwrap();
        assert_eq!(
            result["decisions"][0]["action"],
            if role == "user" { "create" } else { "defer" }
        );
        assert_eq!(
            result["memory_ids"].as_array().unwrap().len(),
            usize::from(role == "user")
        );
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；未被引文支持的新上下文不能让旧决定推翻手动修订"]
async fn uncited_new_context_cannot_advance_old_decision_evidence() {
    let f = fixture().await;
    save(&f, "fictional-cited-time").await;
    let old = captured(
        &f,
        "cited-time",
        "用户确认今后每周五检查数据库。",
        chromie::types::now() - time::Duration::days(1),
    )
    .await;
    let target = memory(&f).await;
    *f.model.target.lock().unwrap() = Some(target);
    f.model.mode.store(28, Ordering::SeqCst);
    let unrelated = captured(
        &f,
        "cited-time",
        "你好，今天还在处理另一个虚构任务。",
        chromie::types::now(),
    )
    .await;
    for (events, expected) in [
        (vec![old, unrelated], "defer"),
        (
            vec![
                captured(
                    &f,
                    "cited-time",
                    "用户现在确认改为每周五检查。",
                    chromie::types::now(),
                )
                .await,
            ],
            "propose_revision",
        ),
    ] {
        let id = Uuid::now_v7();
        f.store
            .enqueue_knowledge(
                f.space,
                Queue {
                    request_id: id,
                    kind: JobKind::Extract,
                    memory_ids: vec![],
                    event_ids: events,
                },
            )
            .await
            .unwrap();
        finish_stage(&f).await;
        let result = job(&f, id).await.result.unwrap();
        assert_eq!(result["decisions"][0]["action"], expected);
        let head = chromie::entity::memories::Entity::find_by_id((
            f.space.bytes(),
            target.as_bytes().to_vec(),
        ))
        .one(&f.store.db)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            head.current_revision,
            if expected == "defer" { 1 } else { 2 }
        );
    }
}

async fn advance(f: &Fixture, id: Uuid, from: i32, to: i32) {
    for revision in from..to {
        f.store
            .correct(
                f.space,
                Correct {
                    memory_id: id,
                    expected_revision: revision,
                    request_id: Uuid::now_v7(),
                    content: Some(format!("虚构记忆正文修订 {}", revision + 1)),
                    scope: None,
                    source_refs: None,
                    memory_type: None,
                    links: None,
                    reason: "虚构历史容量验证".into(),
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；保留近期正文和被引用依据，压缩后修订小元数据不丢失"]
async fn history_retains_five_bodies_and_pinned_basis_then_releases_unreferenced_body() {
    let f = fixture().await;
    save(&f, "fictional-retention-origin").await;
    let event = captured(
        &f,
        "retention-origin",
        "虚构用户偏好：每周四检查数据库。",
        chromie::types::now(),
    )
    .await;
    let task = Uuid::now_v7();
    f.store
        .enqueue_knowledge(
            f.space,
            Queue {
                request_id: task,
                kind: JobKind::Extract,
                memory_ids: vec![],
                event_ids: vec![event],
            },
        )
        .await
        .unwrap();
    finish_stage(&f).await;
    let target = Uuid::parse_str(
        job(&f, task).await.result.unwrap()["memory_ids"][0]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let recall = report_problem(&f, target).await;
    let topic = f
        .store
        .remember(
            f.space,
            Remember {
                request_id: Uuid::now_v7(),
                content: "虚构主题依据".into(),
                scope: Scope::Global,
                source_refs: vec![],
                memory_type: MemoryType::Topic,
                links: vec![chromie::knowledge::Link {
                    memory_id: target,
                    relation: chromie::knowledge::Relation::DerivedFrom,
                    revision: Some(1),
                }],
            },
        )
        .await
        .unwrap();
    let topic = Uuid::parse_str(topic["memory_id"].as_str().unwrap()).unwrap();
    advance(&f, target, 1, 9).await;
    let rows = memory_revisions::Entity::find()
        .filter(memory_revisions::Column::SpaceId.eq(f.space.bytes()))
        .filter(memory_revisions::Column::MemoryId.eq(target.as_bytes().to_vec()))
        .order_by_asc(memory_revisions::Column::Revision)
        .all(&f.store.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 9, "小元数据继续承接统计和回执");
    for row in rows {
        assert_eq!(
            row.content.is_some(),
            row.revision == 1 || row.revision >= 5,
            "修订 {}",
            row.revision
        );
    }
    f.store
        .correct(
            f.space,
            Correct {
                memory_id: topic,
                expected_revision: 1,
                request_id: Uuid::now_v7(),
                content: None,
                scope: None,
                source_refs: None,
                memory_type: None,
                links: Some(vec![]),
                reason: "虚构解除派生关系".into(),
            },
        )
        .await
        .unwrap();
    // 最近历史也可能带有派生边；源历史超出保留窗口后，才释放其旧依据。
    advance(&f, topic, 2, 6).await;
    advance(&f, target, 9, 10).await;
    let old =
        memory_revisions::Entity::find_by_id((f.space.bytes(), target.as_bytes().to_vec(), 1))
            .one(&f.store.db)
            .await
            .unwrap()
            .unwrap();
    assert!(old.content.is_none());
    let retained = chromie::entity::recall_items::Entity::find_by_id((
        f.space.bytes(),
        recall.as_bytes().to_vec(),
        target.as_bytes().to_vec(),
    ))
    .one(&f.store.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        retained.revision, 1,
        "历史正文清理不破坏旧修订的召回和使用统计"
    );
    let history = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target,"view":"history","max_chars":8000}))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        history["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["content_available"] == false)
    );
    let current = f
        .store
        .get(
            f.space,
            serde_json::from_value(json!({"memory_id":target})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        current["created_by"]["origin_kind"], "llm",
        "第一版正文清理后保留创建来源"
    );
    assert_eq!(current["revision"], 10);
    assert!(current["content"].is_string());
}

#[tokio::test]
#[ignore = "需要隔离 MySQL；终态详情受数量和期限限制，在途与旧建议不清理"]
async fn job_details_are_bounded_without_erasing_work_or_idempotency_metadata() {
    let f = fixture().await;
    let mut ids = vec![];
    for n in 0..108 {
        let id = Uuid::now_v7();
        let old = n >= 105;
        let state = if n == 106 { "pending" } else { "done" };
        let created = chromie::types::now() + time::Duration::seconds(n)
            - if old {
                time::Duration::days(31)
            } else {
                time::Duration::ZERO
            };
        let mut result = json!({"request_hash":"fictional-retention-hash","memory_ids":[],"decisions":[{"action":"skip","content":"虚构详细结果".repeat(100)}],"pending_revisions":0});
        if n == 107 {
            result["pending_revisions"] = json!(1);
        }
        knowledge_jobs::ActiveModel {
            space_id: Set(f.space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            kind: Set("extract".into()),
            config_revision: Set(1),
            state: Set(state.into()),
            payload: Set(Some(json!({"fixture":true}))),
            result: Set(Some(result)),
            lease_owner: Set(None),
            lease_expires_at: Set(None),
            attempts: Set(1),
            run_after: Set(created),
            created_at: Set(created),
            updated_at: Set(created),
            last_error: Set(None),
            automatic: Set(1),
        }
        .insert(&f.store.db)
        .await
        .unwrap();
        ids.push(id);
    }
    assert_eq!(f.store.compact_history().await.unwrap(), 6);
    for n in [0, 4, 105] {
        let row = job(&f, ids[n]).await;
        let result = row.result.unwrap();
        assert_eq!(result["detail_expired"], true);
        assert_eq!(result["request_hash"], "fictional-retention-hash");
        assert!(row.payload.is_none());
    }
    for n in [5, 104, 106, 107] {
        assert!(job(&f, ids[n]).await.result.unwrap()["detail_expired"].is_null());
    }
    assert_eq!(
        f.store.compact_history().await.unwrap(),
        0,
        "重复清理不改动已压缩结果"
    );
}
