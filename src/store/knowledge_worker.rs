//! 有限整理任务：领取租约后在事务外调用模型，发布时复核配置、输入和租约。
use super::knowledge_jobs::Snapshot;
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, source_events, text_model_settings},
    knowledge::{MemoryType, Relation},
    text_model::{JobKind, Output},
};
use sea_orm::{Condition, IntoActiveModel, QueryOrder};
use std::collections::{HashMap, HashSet};

/// 等待共享额度后重新确认发送资格；借用持久任务快照，不创建另一份长期正文副本。
pub(super) struct KnowledgeAdmission<'a> {
    pub(super) job: &'a jobs::Model,
    pub(super) lease: Uuid,
    pub(super) snapshot: &'a Snapshot,
}

impl Store {
    pub async fn sync_knowledge_due(&self) -> Result<usize> {
        let tx = self.db.begin().await?;
        Self::model_settings_locked(&tx).await?;
        let allow_automatic = Self::automatic_allowed(&tx).await?;
        let current = now();
        let candidate = jobs::Entity::find()
            .filter(if allow_automatic {
                Condition::all()
            } else {
                Condition::all().add(jobs::Column::Automatic.eq(0))
            })
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(jobs::Column::State.is_in(["pending", "retry"]))
                            .add(jobs::Column::RunAfter.lte(current)),
                    )
                    .add(
                        Condition::all()
                            .add(jobs::Column::State.eq("running"))
                            .add(jobs::Column::LeaseExpiresAt.lte(current)),
                    ),
            )
            .order_by_asc(jobs::Column::RunAfter)
            .lock_exclusive()
            .one(&tx)
            .await?;
        let Some(row) = candidate else {
            tx.commit().await?;
            return Ok(0);
        };
        if row.automatic != 0 {
            Self::count_automatic_attempt(&tx).await?;
        }
        let lease = Uuid::now_v7();
        let mut active = row.into_active_model();
        active.state = Set("running".into());
        active.lease_owner = Set(Some(lease.as_bytes().to_vec()));
        active.lease_expires_at = Set(Some(current + time::Duration::seconds(180)));
        active.attempts = Set(active
            .attempts
            .as_ref()
            .checked_add(1)
            .ok_or_else(Error::unavailable)?);
        active.updated_at = Set(current);
        let row = active.update(&tx).await?;
        tx.commit().await?;
        if let Err(error) = self.run_knowledge_job(&row, lease).await {
            self.fail_knowledge_job(&row, lease, error).await?;
        }
        Ok(1)
    }

    async fn run_knowledge_job(&self, row: &jobs::Model, lease: Uuid) -> Result<()> {
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|config| config.enabled != 0 && config.revision == row.config_revision)
            .ok_or_else(|| Error::conflict("knowledge_config_changed"))?;
        let snapshot: Snapshot =
            serde_json::from_value(row.payload.clone().ok_or_else(Error::missing)?)
                .map_err(|_| Error::unavailable())?;
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        Self::check_automatic(&self.db, &snapshot).await?;
        self.validate_snapshot(&self.db, space, &snapshot).await?;
        if snapshot.kind == JobKind::Extract {
            return self
                .run_extraction_stage(row, lease, &config, snapshot)
                .await;
        }
        // 自动批次、预算修订与工作标记仅供服务恢复使用，不是模型需要理解的资料。
        let model_input = json!({"kind":snapshot.kind,"memories":snapshot.memories,"events":snapshot.events,"usage":snapshot.usage,"conversation":snapshot.automatic.as_ref().and_then(|a|a.conversation.as_ref()).map(|c|json!({"current_title":c.title,"title_locked":c.title_locked,"new_event_ids":c.work_event_ids}))});
        let output = self
            .call_text_model_guarded(
                &self.text_connection(&config).await?,
                &model_input,
                Some(KnowledgeAdmission {
                    job: row,
                    lease,
                    snapshot: &snapshot,
                }),
            )
            .await?;
        let parsed: Output = serde_json::from_value(output).map_err(|_| {
            crate::text_model::error("model_output_schema", "模型整理结果结构无效，未写入记忆")
        })?;
        let tx = self.begin(space).await?;
        // 配置保存/停用同样持有这把锁；提交不能跨越一次配置切换。
        Self::model_settings_locked(&tx).await?;
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if config.enabled == 0 || config.revision != row.config_revision {
            return Err(Error::conflict("knowledge_config_changed"));
        }
        let current = jobs::Entity::find_by_id((row.space_id.clone(), row.id.clone()))
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if current.state != "running"
            || current.lease_owner.as_deref() != Some(lease.as_bytes().as_slice())
            || current
                .lease_expires_at
                .is_none_or(|expiry| expiry <= now())
        {
            return Ok(());
        }
        Self::check_automatic(&tx, &snapshot).await?;
        self.validate_snapshot(&tx, space, &snapshot).await?;
        let memories: HashMap<_, _> = snapshot
            .memories
            .iter()
            .map(|memory| (memory.id, memory))
            .collect();
        let events: HashMap<_, _> = snapshot
            .events
            .iter()
            .map(|event| (event.event_id, event))
            .collect();
        if parsed.notes.len() > 5 || parsed.conflicts.len() > 10 {
            return Err(crate::text_model::error(
                "model_evidence_invalid",
                "模型输出超过整理条数上限",
            ));
        }
        let mut ids = Vec::new();
        let proposals = Self::maintenance_proposals(&snapshot, parsed.revisions)?;
        let mut edited = HashSet::new();
        let mut titles = HashSet::new();
        let mut merged_topics = Vec::new();
        let mut topic_changes = Vec::new();
        for mut draft in parsed.notes {
            crate::types::bounded(&draft.content, 16000)?;
            if !titles.insert(crate::knowledge::title(&draft.content)) {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "模型生成重复主题，未写入记忆",
                ));
            }
            let mut projects = HashSet::new();
            let mut evidence = 0usize;
            if draft.memory_id.is_none()
                && let Some(existing) = snapshot.memories.iter().find(|m| {
                    m.memory_type == MemoryType::Topic
                        && crate::knowledge::title(&m.content)
                            == crate::knowledge::title(&draft.content)
                })
            {
                draft.memory_id = Some(existing.id);
                draft.expected_revision = Some(existing.revision);
            }
            let topic_title = crate::knowledge::title(&draft.content);
            let merged = Self::prepare_topic_merge(&snapshot, &mut draft, &mut edited)?;
            let previous = draft.memory_id.and_then(|id| memories.get(&id).copied());
            if let Some(previous) = previous {
                // 模型没有读过的分支原样保留，局部整理不能删掉主题的其他入口。
                for link in &previous.links {
                    if !memories.contains_key(&link.memory_id)
                        && !draft.links.iter().any(|new| {
                            new.memory_id == link.memory_id && new.relation == link.relation
                        })
                    {
                        draft.links.push(link.clone());
                    }
                }
            }
            for link in &draft.links {
                link.validate()?;
                let Some(target) = memories.get(&link.memory_id) else {
                    if let Some(previous) = snapshot.memories.iter().find(|m| {
                        (Some(m.id) == draft.memory_id || draft.merge_from.contains(&m.id))
                            && m.links.contains(link)
                    }) {
                        if link.relation == Relation::DerivedFrom {
                            evidence += 1;
                        }
                        if let Some(project) = &previous.project_key {
                            projects.insert(project.clone());
                        }
                        continue;
                    }
                    return Err(crate::text_model::error(
                        "model_evidence_invalid",
                        "模型引用了本次范围之外的记忆",
                    ));
                };
                if link.relation == Relation::DerivedFrom {
                    if link.revision != Some(target.revision) {
                        return Err(crate::text_model::error(
                            "model_evidence_invalid",
                            "模型依据修订不匹配",
                        ));
                    }
                    evidence += 1;
                    if let Some(project) = &target.project_key {
                        projects.insert(project.clone());
                    }
                }
            }
            for source in &draft.source_refs {
                let event = events.get(source).ok_or_else(|| {
                    crate::text_model::error(
                        "model_evidence_invalid",
                        if memories.contains_key(source) {
                            "模型把记忆 ID 填入了消息来源；请用 links 表达记忆关系"
                        } else {
                            "模型引用了本次范围之外的消息"
                        },
                    )
                })?;
                if let Some(project) = &event.project_key {
                    projects.insert(project.clone());
                }
            }
            if projects.len() > 1 {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "模型混合不同项目的事实，请缩小范围重试",
                ));
            }
            let project = projects.into_iter().next();
            let scope = project
                .as_ref()
                .map(|key| crate::types::Scope::Project {
                    project_key: key.clone(),
                })
                .unwrap_or_default();
            if draft.memory_type != MemoryType::Topic
                || evidence == 0
                || !draft.source_refs.is_empty()
            {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "主题整理必须提供实际记忆依据，不改写普通记忆",
                ));
            }
            let result = if let Some(id) = draft.memory_id {
                let previous = memories.get(&id).ok_or_else(|| {
                    crate::text_model::error("model_evidence_invalid", "模型试图修改范围之外的主题")
                })?;
                if previous.memory_type != MemoryType::Topic
                    || draft.expected_revision != Some(previous.revision)
                    || previous.project_key != project
                    || !edited.insert(id)
                {
                    return Err(crate::text_model::error(
                        "model_evidence_invalid",
                        "模型主题修订、范围或类型不匹配",
                    ));
                }
                let preserved = snapshot
                    .memories
                    .iter()
                    .filter(|m| m.id == id || draft.merge_from.contains(&m.id))
                    .flat_map(|m| m.links.clone())
                    .collect::<Vec<_>>();
                self.correct_with_preserved_links_tx(
                    &tx,
                    space,
                    crate::types::Correct {
                        memory_id: id,
                        expected_revision: previous.revision,
                        request_id: Uuid::now_v7(),
                        reason: "后台 LLM 核对依据后整理主题".into(),
                        content: Some(draft.content),
                        scope: None,
                        source_refs: None,
                        memory_type: Some(MemoryType::Topic),
                        links: Some(draft.links),
                    },
                    &preserved,
                )
                .await?
            } else {
                if draft.expected_revision.is_some() {
                    return Err(crate::text_model::error(
                        "model_evidence_invalid",
                        "新记忆不能声明已有修订",
                    ));
                }
                self.remember_tx(
                    &tx,
                    space,
                    crate::types::Remember {
                        content: draft.content,
                        request_id: Uuid::now_v7(),
                        scope,
                        source_refs: draft.source_refs,
                        memory_type: draft.memory_type,
                        links: draft.links,
                    },
                )
                .await?
            };
            let keeper: Uuid = serde_json::from_value(result["memory_id"].clone())
                .map_err(|_| Error::unavailable())?;
            for (id, revision, title) in merged {
                self.correct_tx(
                    &tx,
                    space,
                    crate::types::Correct {
                        memory_id: id,
                        expected_revision: revision,
                        request_id: Uuid::now_v7(),
                        reason: "后台整理合并重叠主题，保留旧入口和历史".into(),
                        content: Some(format!("主题已合并：{title}\n完整入口见关联主题。")),
                        scope: None,
                        source_refs: None,
                        memory_type: Some(MemoryType::TopicAlias),
                        links: Some(vec![crate::knowledge::Link {
                            memory_id: keeper,
                            relation: Relation::Reference,
                            revision: None,
                        }]),
                    },
                )
                .await?;
                merged_topics.push(json!({"from":id,"to":keeper,"title":title}));
            }
            topic_changes.push(json!({"memory_id":keeper,"title":topic_title,"action":if draft.memory_id.is_some(){"updated"}else{"created"}}));
            if draft.memory_id.is_none() {
                let created: Uuid = serde_json::from_value(result["memory_id"].clone())
                    .map_err(|_| Error::unavailable())?;
                Self::mark_generated(&tx, space, created, uuid_from_bytes(&row.id)?).await?;
            }
            ids.push(result["memory_id"].clone());
        }
        for conflict in &parsed.conflicts {
            crate::types::bounded(&conflict.reason, 2000)?;
            if conflict.memory_ids.is_empty()
                || conflict.memory_ids.len() > 10
                || conflict
                    .memory_ids
                    .iter()
                    .any(|id| !memories.contains_key(id))
            {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "冲突说明的引用无效",
                ));
            }
        }
        Self::publish_conversation_title(&tx, space, &snapshot, parsed.conversation_title).await?;
        Self::finish_organization(&tx, &current, "done", None).await?;
        let mut result = current.result.clone().unwrap_or_else(|| json!({}));
        if let Some(fields) = result.as_object_mut() {
            fields.remove("error");
        }
        result["memory_ids"] = json!(ids);
        result["conflicts"] = json!(parsed.conflicts);
        result["merged_topics"] = json!(merged_topics);
        result["topic_changes"] = json!(topic_changes);
        if snapshot.kind == JobKind::Maintain {
            result["pending_revisions"] = json!(proposals.len());
            result["decisions"] = json!(proposals);
        }
        let mut active = current.into_active_model();
        active.state = Set("done".into());
        active.payload = Set(None);
        active.result = Set(Some(result));
        active.lease_owner = Set(None);
        active.lease_expires_at = Set(None);
        active.last_error = Set(None);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn validate_knowledge_admission(
        &self,
        admission: &KnowledgeAdmission<'_>,
    ) -> Result<()> {
        let row = admission.job;
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        // 与暂停/Provider切换/遗忘使用相同锁序；提交是发送已放行的时点，HTTP不持这些锁。
        let tx = self.begin(space).await?;
        Self::model_settings_locked(&tx).await?;
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if config.enabled == 0 || config.revision != row.config_revision {
            return Err(Error::conflict("knowledge_config_changed"));
        }
        Self::check_automatic(&tx, admission.snapshot).await?;
        let current = jobs::Entity::find_by_id((row.space_id.clone(), row.id.clone()))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if current.state != "running"
            || current.lease_owner.as_deref() != Some(admission.lease.as_bytes().as_slice())
            || current.lease_expires_at.is_none_or(|until| until <= now())
        {
            return Err(Error::conflict("knowledge_lease_changed"));
        }
        self.validate_snapshot(&tx, space, admission.snapshot)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn validate_snapshot<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        snapshot: &Snapshot,
    ) -> Result<()> {
        Self::check_usage_evidence(db, space, &snapshot.usage).await?;
        for item in &snapshot.memories {
            let current = Self::head(db, space, item.id).await?;
            if current.forgotten_at.is_some() || current.current_revision != item.revision {
                return Err(Error::conflict("knowledge_input_changed"));
            }
        }
        for item in &snapshot.events {
            let current = source_events::Entity::find_by_id((
                space.bytes(),
                item.event_id.as_bytes().to_vec(),
            ))
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
            if current.forgotten_at.is_some() || current.body.as_deref() != Some(item.body.as_str())
            {
                return Err(Error::conflict("knowledge_input_changed"));
            }
        }
        Ok(())
    }

    async fn fail_knowledge_job(&self, row: &jobs::Model, lease: Uuid, error: Error) -> Result<()> {
        // 与遗忘、配置切换和发布保持锁序，避免任务行与事件进度互相等待。
        let tx = self.begin(SpaceId(uuid_from_bytes(&row.space_id)?)).await?;
        Self::model_settings_locked(&tx).await?;
        let current = jobs::Entity::find_by_id((row.space_id.clone(), row.id.clone()))
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if current.state != "running"
            || current.lease_owner.as_deref() != Some(lease.as_bytes().as_slice())
        {
            return Ok(());
        }
        let stale = matches!(
            error.code,
            "knowledge_config_changed"
                | "knowledge_input_changed"
                | "revision_conflict"
                | "dependency_revision_conflict"
                | "not_found"
                | "organization_settings_changed"
                | "knowledge_lease_changed"
        );
        let retry = matches!(
            error.code,
            "model_unreachable"
                | "model_server_error"
                | "model_rate_limited"
                | "model_busy"
                | "storage_error"
                | "unavailable"
        ) && current.attempts < 4;
        let delay = [10, 30, 120, 300]
            .get(current.attempts.saturating_sub(1) as usize)
            .copied()
            .unwrap_or(300);
        if stale || !retry {
            Self::finish_organization(
                &tx,
                &current,
                if stale { "pending" } else { "failed" },
                Some(error.code),
            )
            .await?;
        }
        // Error 仅携带服务定义的静态说明，不把模型响应、SQL 或输入资料放进结果。
        let mut result = current.result.clone().unwrap_or_else(|| json!({}));
        result["error"] = error.json();
        result["diagnostic"] = json!({
            "phase":if current.kind == "organize" {"organize"}else if current.kind=="maintain"{"maintain"} else if current.payload.as_ref().is_some_and(|p| !p["review"].is_null()) {"review"} else {"extract"},
            "snapshot_bytes":current.payload.as_ref().map(|p|p.to_string().len()),
            "work_items":Self::knowledge_work_items(&current),
            "code":error.code,"attempts":current.attempts,"automatic_retry":retry
        });
        let mut active = current.into_active_model();
        active.result = Set(Some(result));
        active.state = Set(if stale {
            "stale"
        } else if retry {
            "retry"
        } else {
            "failed"
        }
        .into());
        if stale || !retry {
            active.payload = Set(None);
        }
        active.last_error = Set(Some(error.code.into()));
        active.lease_owner = Set(None);
        active.lease_expires_at = Set(None);
        active.run_after = Set(now() + time::Duration::seconds(delay));
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) fn knowledge_work_items(row: &jobs::Model) -> usize {
        let payload = row.payload.as_ref();
        payload
            .and_then(|p| {
                p["automatic"]["conversation"]["work_event_ids"]
                    .as_array()
                    .or_else(|| p["automatic"]["memory_ids"].as_array())
            })
            .map(Vec::len)
            .unwrap_or(1)
    }

    pub(super) async fn smaller_knowledge_batch<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        job: Option<Vec<u8>>,
        configured: u32,
    ) -> Result<u64> {
        let Some(job) = job else {
            return Ok(u64::from(configured));
        };
        let rows = jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .filter(jobs::Column::Id.eq(job))
            .one(db)
            .await?;
        let size = rows
            .and_then(|r| r.result)
            .and_then(|r| r["diagnostic"]["work_items"].as_u64());
        Ok(size.map_or(u64::from(configured), |n| {
            (n / 2).max(1).min(u64::from(configured))
        }))
    }
}
