//! 每阶段单独领取/计预算。候选与相关修订先持久化，再核对发布，避免两次HTTP跨过同一租约。
use super::knowledge_jobs::{ReviewSnapshot, Snapshot};
use super::knowledge_worker::KnowledgeAdmission;
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, text_model_settings},
    extraction::{Action, Candidate, Category, Decision, Generated, Reviewed},
};
use sea_orm::IntoActiveModel;
use std::collections::{BTreeMap, HashMap, HashSet};

impl Store {
    pub(super) async fn extraction_commit(
        &self,
        row: &jobs::Model,
        lease: Uuid,
        snapshot: &Snapshot,
    ) -> Result<(DatabaseTransaction, jobs::Model)> {
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        let tx = self.begin(space).await?;
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
            || current.lease_expires_at.is_none_or(|t| t <= now())
        {
            return Err(Error::conflict("knowledge_lease_changed"));
        }
        Self::check_automatic(&tx, snapshot).await?;
        self.validate_snapshot(&tx, space, snapshot).await?;
        Ok((tx, current))
    }
    pub(super) async fn run_extraction_stage(
        &self,
        row: &jobs::Model,
        lease: Uuid,
        config: &text_model_settings::Model,
        mut snapshot: Snapshot,
    ) -> Result<()> {
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        let admission = KnowledgeAdmission {
            job: row,
            lease,
            snapshot: &snapshot,
        };
        let conversation=snapshot.automatic.as_ref().and_then(|a|a.conversation.as_ref()).map(|c|json!({"current_title":c.title,"title_locked":c.title_locked,"new_event_ids":c.work_event_ids}));
        if let Some(review) = &snapshot.review {
            let input = json!({"kind":"review","candidates":review.candidates,"precheck":review.rejected,"events":snapshot.events,"memories":snapshot.memories,"retrieval_partial":review.retrieval_partial,"conversation":conversation});
            let value = self
                .call_text_model_guarded(
                    &self.text_connection(config).await?,
                    &input,
                    Some(admission),
                )
                .await?;
            let reviewed: Reviewed =
                serde_json::from_value(value).map_err(|_| model_error("核对结果结构无效"))?;
            return self
                .publish_extraction(row, lease, &snapshot, reviewed.decisions)
                .await;
        }
        let input = json!({"kind":"extract","events":snapshot.events,"memories":[],"conversation":conversation});
        let value = self
            .call_text_model_guarded(
                &self.text_connection(config).await?,
                &input,
                Some(admission),
            )
            .await?;
        let mut generated: Generated =
            serde_json::from_value(value).map_err(|_| model_error("候选结果结构无效"))?;
        if generated.candidates.len() > 5 {
            return Err(model_error("候选超过5条"));
        }
        let mut rejected = BTreeMap::new();
        for (index, candidate) in generated.candidates.iter_mut().enumerate() {
            crate::types::bounded(&candidate.content, 16000)?;
            crate::types::bounded(&candidate.search_query, 256)?;
            if candidate.evidence.len() > 4 || candidate.source_refs.len() > 16 {
                // 单条草稿违规不丢整批消息；只保留有界展示资料，precheck禁止其写入。
                rejected.insert(index, "来源或引文超过数量上限，暂缓此候选".into());
                candidate.evidence.truncate(4);
                candidate.source_refs.truncate(16);
            } else if let Some(reason) = Self::candidate_issue(&snapshot, candidate) {
                rejected.insert(index, reason.to_owned());
            }
        }
        snapshot.review = Some(ReviewSnapshot {
            candidates: generated.candidates,
            conversation_title: generated.conversation_title,
            retrieval_partial: false,
            rejected,
        });
        if snapshot
            .review
            .as_ref()
            .is_some_and(|r| r.candidates.is_empty())
        {
            return self.publish_extraction(row, lease, &snapshot, vec![]).await;
        }
        let admission = KnowledgeAdmission {
            job: row,
            lease,
            snapshot: &snapshot,
        };
        let review = snapshot.review.as_ref().ok_or_else(Error::unavailable)?;
        let (memories, partial) = self
            .extraction_related(space, &review.candidates, &admission, &snapshot)
            .await?;
        snapshot.memories = memories;
        if let Some(review) = snapshot.review.as_mut() {
            review.retrieval_partial = partial;
        }
        let payload = serde_json::to_value(&snapshot).map_err(|_| Error::unavailable())?;
        if payload.to_string().len() > 256 * 1024 {
            return Err(model_error("核对输入超过256KiB"));
        }
        let (tx, current) = self.extraction_commit(row, lease, &snapshot).await?;
        let mut result = current.result.clone().unwrap_or_else(|| json!({}));
        result["phase"] = json!("review");
        result["candidate_count"] =
            json!(snapshot.review.as_ref().map_or(0, |r| r.candidates.len()));
        let mut active = current.into_active_model();
        active.payload = Set(Some(payload));
        active.result = Set(Some(result));
        active.state = Set("pending".into());
        active.run_after = Set(now());
        active.lease_owner = Set(None);
        active.lease_expires_at = Set(None);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// 引文只能逐字摘自已读来源；偏好必须有用户证据，不能把助手提议提升为用户选择。
    fn candidate_issue(snapshot: &Snapshot, candidate: &Candidate) -> Option<&'static str> {
        if candidate.source_refs.is_empty() || candidate.evidence.is_empty() {
            return Some("缺少可核对来源");
        }
        let events = snapshot
            .events
            .iter()
            .map(|e| (e.event_id, e))
            .collect::<HashMap<_, _>>();
        if candidate
            .source_refs
            .iter()
            .any(|id| !events.contains_key(id))
        {
            return Some("来源不在本批范围");
        }
        if let Some(c) = snapshot
            .automatic
            .as_ref()
            .and_then(|a| a.conversation.as_ref())
            && !candidate
                .source_refs
                .iter()
                .any(|id| c.work_event_ids.contains(id))
        {
            return Some("没有本批新增消息作为依据");
        }
        if candidate.evidence.iter().any(|e| {
            e.quote.trim().is_empty()
                || e.quote.len() > 1000
                || !candidate.source_refs.contains(&e.event_id)
                || events
                    .get(&e.event_id)
                    .is_none_or(|source| !source.body.contains(e.quote.as_str()))
        }) {
            return Some("引文与实际消息不匹配");
        }
        if candidate.category == Category::Preference
            && !candidate
                .evidence
                .iter()
                .any(|e| events.get(&e.event_id).is_some_and(|s| s.role == "user"))
        {
            return Some("偏好缺少用户原话依据");
        }
        None
    }
    pub(super) async fn publish_extraction(
        &self,
        row: &jobs::Model,
        lease: Uuid,
        snapshot: &Snapshot,
        mut decisions: Vec<Decision>,
    ) -> Result<()> {
        let review = snapshot.review.as_ref().ok_or_else(Error::unavailable)?;
        if decisions.len() != review.candidates.len() {
            return Err(model_error("核对未覆盖全部候选"));
        }
        let mut seen = HashSet::new();
        let mut targets = HashMap::new();
        for d in &decisions {
            if d.candidate_index >= review.candidates.len() || !seen.insert(d.candidate_index) {
                return Err(model_error("核对候选编号无效"));
            }
            crate::types::bounded(&d.reason, 1000)?;
            if let Some(replacement) = &d.replacement {
                *targets.entry(replacement.memory_id).or_insert(0usize) += 1;
            }
        }
        decisions.sort_by_key(|d| d.candidate_index);
        let (tx, current) = self.extraction_commit(row, lease, snapshot).await?;
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        let mut ids = vec![];
        let mut records = vec![];
        for mut d in decisions {
            let candidate = &review.candidates[d.candidate_index];
            let content = d.content.as_deref().unwrap_or(&candidate.content);
            crate::types::bounded(content, 16000)?;
            if let Some(issue) = &d.quality_issue {
                crate::types::bounded(issue, 1000)?;
                d.action = Action::Defer;
                d.reason = issue.clone();
                d.replacement = None;
            }
            if crate::extraction::incomplete_method(content)
                || d.replacement
                    .as_ref()
                    .is_some_and(|r| crate::extraction::incomplete_method(&r.content))
            {
                d.action = Action::Defer;
                d.reason = "操作链接或路径含省略，无法独立使用；须从真实来源补齐".into();
                d.replacement = None;
            }
            if let Some(reason) = review.rejected.get(&d.candidate_index) {
                d.action = Action::Defer;
                d.reason = reason.clone();
                d.replacement = None;
            }
            let mut source_refs = candidate.source_refs.clone();
            let mut evidence = candidate.evidence.clone();
            if let Some(proof) = &d.explicit_change
                && !d
                    .evidence
                    .iter()
                    .chain(evidence.iter())
                    .any(|e| e.event_id == proof.event_id && e.quote == proof.quote)
            {
                d.evidence.push(proof.clone());
            }
            if d.evidence.len() > 4 {
                return Err(model_error("补充证据数量超限"));
            }
            for proof in &d.evidence {
                let valid = !proof.quote.trim().is_empty()
                    && proof.quote.len() <= 1000
                    && snapshot.events.iter().any(|e| {
                        e.event_id == proof.event_id && e.body.contains(proof.quote.as_str())
                    });
                if !valid {
                    d.action = Action::Defer;
                    d.reason = "核对阶段补充引文与实际消息不匹配".into();
                    d.replacement = None;
                    break;
                }
                if !source_refs.contains(&proof.event_id) {
                    source_refs.push(proof.event_id);
                }
                evidence.push(proof.clone());
            }
            if source_refs.len() > 16 {
                return Err(model_error("补充来源后数量超限"));
            }
            let projects = source_refs
                .iter()
                .filter_map(|id| {
                    snapshot
                        .events
                        .iter()
                        .find(|e| e.event_id == *id)
                        .and_then(|e| e.project_key.clone())
                })
                .collect::<HashSet<_>>();
            if projects.len() > 1 {
                d.action = Action::Defer;
                d.reason = "候选混合了不同项目的证据".into();
                d.replacement = None;
            }
            let project = projects.into_iter().next();
            let user_evidence = evidence.iter().any(|e| {
                snapshot
                    .events
                    .iter()
                    .any(|s| s.event_id == e.event_id && s.role == "user")
            });
            if d.action == Action::ProposeRevision && !user_evidence {
                d.action = Action::Defer;
                d.reason = "修订缺少用户明确变更的证据".into();
                d.replacement = None;
            }
            if d.action != Action::ProposeRevision && d.replacement.is_some() {
                return Err(model_error("非修订动作不能携带替换正文"));
            }
            let mut old = None;
            let mut created = None;
            if d.action == Action::Create {
                let duplicate = snapshot
                    .memories
                    .iter()
                    .any(|m| normalize(&m.content) == normalize(content));
                if duplicate {
                    d.action = Action::Skip;
                    d.reason = "已有相同正文，无需重复写入".into();
                } else {
                    let receipt = self
                        .remember_tx(
                            &tx,
                            space,
                            crate::types::Remember {
                                request_id: Uuid::now_v7(),
                                content: content.to_owned(),
                                scope: project
                                    .clone()
                                    .map(|project_key| crate::types::Scope::Project { project_key })
                                    .unwrap_or_default(),
                                source_refs: source_refs.clone(),
                                memory_type: crate::knowledge::MemoryType::Note,
                                links: vec![],
                            },
                        )
                        .await?;
                    let id: Uuid = serde_json::from_value(receipt["memory_id"].clone())
                        .map_err(|_| Error::unavailable())?;
                    Self::mark_generated(&tx, space, id, uuid_from_bytes(&row.id)?).await?;
                    ids.push(id);
                    created = Some(id);
                }
            }
            let mut receipt = None;
            if d.action == Action::ProposeRevision {
                let replacement = d
                    .replacement
                    .as_ref()
                    .ok_or_else(|| model_error("修订缺少完整替换方案"))?;
                crate::types::bounded(&replacement.content, 16000)?;
                let target = snapshot
                    .memories
                    .iter()
                    .find(|m| {
                        m.id == replacement.memory_id
                            && m.revision == replacement.expected_revision
                            && m.memory_type == crate::knowledge::MemoryType::Note
                    })
                    .ok_or_else(|| model_error("修订目标不在已读相关记忆中"))?;
                if target.project_key != project {
                    d.action = Action::Defer;
                    d.reason = "修订目标与消息的项目范围不一致".into();
                    d.replacement = None;
                } else {
                    old = Some(target.content.clone());
                    if normalize(&target.content) == normalize(&replacement.content) {
                        d.action = Action::Skip;
                        d.reason = "当前正文已包含相同更正，无需重复增加修订".into();
                        d.replacement = None;
                    }
                }
            }
            if d.action == Action::ProposeRevision
                && let Some(proof) = &d.explicit_change
                && let Some(replacement) = &d.replacement
                && targets.get(&replacement.memory_id) == Some(&1)
                && Self::explicit_change_is_current(
                    &tx,
                    space,
                    snapshot,
                    replacement,
                    proof,
                    &source_refs,
                )
                .await?
            {
                receipt = Some(
                    self.apply_replacement_tx(
                        &tx,
                        space,
                        replacement.clone(),
                        &source_refs,
                        format!("后台按用户明确更正自动修订：{}", d.reason),
                    )
                    .await?,
                );
                let version: i32 = serde_json::from_value(
                    receipt.as_ref().ok_or_else(Error::unavailable)?["revision"].clone(),
                )
                .map_err(|_| Error::unavailable())?;
                Self::mark_automatic_revision(
                    &tx,
                    space,
                    replacement.memory_id,
                    version,
                    uuid_from_bytes(&row.id)?,
                )
                .await?;
            }
            let evidence_details = evidence.iter().map(|proof| {
                let source = snapshot.events.iter().find(|s| s.event_id == proof.event_id);
                json!({"event_id":proof.event_id,"quote":proof.quote,"role":source.map(|s|s.role.as_str()),"occurred_at":source.map(|s|s.occurred_at.as_str())})
            }).collect::<Vec<_>>();
            records.push(json!({"evidence_details":evidence_details,"candidate_index":d.candidate_index,"action":d.action,"reason":d.reason,"content":content,"category":candidate.category,"source_refs":source_refs,"evidence":evidence,"replacement":d.replacement,"previous_content":old,"published_memory_id":created,"applied":receipt.is_some(),"resolution":if receipt.is_some(){Some("automatic")}else{None},"receipt":receipt}));
        }
        // 无主要事项时保留原名/消息摘要，不让命名阻塞消息消费。
        if let Some(title) = &review.conversation_title {
            Self::publish_conversation_title(&tx, space, snapshot, Some(title.clone())).await?;
        }
        Self::finish_organization(&tx, &current, "done", None).await?;
        let mut result = current.result.clone().unwrap_or_else(|| json!({}));
        result["phase"] = json!("complete");
        result["memory_ids"] = json!(ids);
        result["decisions"] = json!(records);
        result["pending_revisions"] = json!(
            records
                .iter()
                .filter(|r| r["action"] == "propose_revision" && r["applied"] != true)
                .count()
        );
        result["retrieval_partial"] = json!(review.retrieval_partial);
        if let Some(map) = result.as_object_mut() {
            map.remove("error");
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
}
fn model_error(message: &'static str) -> Error {
    crate::text_model::error("model_evidence_invalid", message)
}
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<String>()
}
