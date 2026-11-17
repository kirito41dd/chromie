//! 自动更正与人工确认共用修订事务；过期方案不允许覆盖新内容，来源和历史不丢失。
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, memory_sources, source_events},
    extraction::{Apply, Evidence, Replacement},
    knowledge::MemoryType,
    types::Correct,
};
use sea_orm::IntoActiveModel;

impl Store {
    pub async fn knowledge_review(&self, space: SpaceId, job_id: Uuid) -> Result<Value> {
        let row = jobs::Entity::find_by_id((space.bytes(), job_id.as_bytes().to_vec()))
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        let mut view = Self::job_view(&row);
        let mut result = row.result.unwrap_or_else(|| json!({}));
        if let Some(fields) = result.as_object_mut() {
            fields.remove("request_hash");
        }
        view["result"] = result;
        Ok(view)
    }

    pub async fn apply_extraction_revision(&self, space: SpaceId, input: Apply) -> Result<Value> {
        let tx = self.begin(space).await?;
        let row = jobs::Entity::find_by_id((space.bytes(), input.job_id.as_bytes().to_vec()))
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if !["extract", "maintain"].contains(&row.kind.as_str()) || row.state != "done" {
            return Err(Error::invalid("此任务没有可确认的修订"));
        }
        let mut result = row.result.clone().ok_or_else(Error::missing)?;
        let decision = result["decisions"]
            .as_array_mut()
            .and_then(|items| {
                items
                    .iter_mut()
                    .find(|d| d["candidate_index"].as_u64() == Some(input.candidate_index as u64))
            })
            .filter(|d| d["action"] == "propose_revision")
            .ok_or_else(Error::missing)?;
        // 同一方案重复确认只返回原回执；不再次生成修订。
        if decision["applied"] == true {
            return Ok(decision["receipt"].clone());
        }
        if decision["resolution"] == "superseded" {
            return Err(Error::conflict("proposal_superseded"));
        }
        let replacement: Replacement = serde_json::from_value(decision["replacement"].clone())
            .map_err(|_| Error::unavailable())?;
        let evidence: Vec<Evidence> = serde_json::from_value(decision["evidence"].clone())
            .map_err(|_| Error::unavailable())?;
        let source_refs: Vec<Uuid> = serde_json::from_value(decision["source_refs"].clone())
            .map_err(|_| Error::unavailable())?;
        let mut user_evidence = false;
        for proof in evidence {
            let source = source_events::Entity::find_by_id((
                space.bytes(),
                proof.event_id.as_bytes().to_vec(),
            ))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
            if source.forgotten_at.is_some()
                || source
                    .body
                    .as_ref()
                    .is_none_or(|body| !body.contains(proof.quote.as_str()))
            {
                return Err(Error::conflict("knowledge_input_changed"));
            }
            user_evidence |= source.role == "user";
        }
        if row.kind == "maintain" {
            let proof: Vec<super::use_reports::UseEvidence> =
                serde_json::from_value(decision["usage_evidence"].clone())
                    .map_err(|_| Error::unavailable())?;
            if proof.is_empty()
                || proof.len() > 10
                || proof.iter().any(|e| {
                    e.memory_id != replacement.memory_id
                        || e.revision != replacement.expected_revision
                        || !matches!(
                            e.outcome,
                            crate::recall_usage::UseOutcome::ContentOutdated
                                | crate::recall_usage::UseOutcome::EnvironmentBlocked
                        )
                })
            {
                return Err(Error::invalid("维护修订缺少对应版本的实际问题报告"));
            }
            Self::check_usage_evidence(&tx, space, &proof).await?;
        } else if !user_evidence {
            return Err(Error::invalid("修订缺少用户证据"));
        }
        let reason = decision["reason"]
            .as_str()
            .unwrap_or("确认对话提炼的修订方案")
            .to_owned();
        let receipt = self
            .apply_replacement_tx(&tx, space, replacement, &source_refs, reason)
            .await?;
        // correct 已将同一任务内的其他旧方案作废；重读后补回执，不能用旧快照复活它们。
        let mut result =
            jobs::Entity::find_by_id((space.bytes(), input.job_id.as_bytes().to_vec()))
                .one(&tx)
                .await?
                .and_then(|r| r.result)
                .ok_or_else(Error::missing)?;
        let decision = result["decisions"]
            .as_array_mut()
            .and_then(|items| {
                items
                    .iter_mut()
                    .find(|d| d["candidate_index"].as_u64() == Some(input.candidate_index as u64))
            })
            .ok_or_else(Error::missing)?;
        decision["applied"] = json!(true);
        decision["resolution"] = json!("confirmed");
        decision["receipt"] = receipt.clone();
        result["pending_revisions"] = json!(Self::pending_proposal_count(&result));
        let mut active = row.into_active_model();
        active.result = Set(Some(result));
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// 调用方持空间锁并校验来源证据；此处统一维护旧来源、关系、修订和幂等回执。
    pub(super) async fn apply_replacement_tx(
        &self,
        tx: &DatabaseTransaction,
        space: SpaceId,
        replacement: Replacement,
        source_refs: &[Uuid],
        reason: String,
    ) -> Result<Value> {
        let head = Self::head(tx, space, replacement.memory_id).await?;
        let old = Self::revision(tx, &head).await?;
        if old.memory_type != MemoryType::Note.as_str()
            || crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
                .one(tx)
                .await?
                .is_some()
        {
            return Err(Error::invalid("此方案只能修订普通记忆"));
        }
        let mut refs = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(head.id.clone()))
            .filter(memory_sources::Column::Revision.eq(head.current_revision))
            .all(tx)
            .await?
            .into_iter()
            .map(|s| uuid_from_bytes(&s.event_id))
            .collect::<Result<Vec<_>>>()?;
        for id in source_refs {
            if !refs.contains(id) {
                refs.push(*id);
            }
        }
        // 来源没有上限扩张：超限时保留旧修订及方案，交由人工整理，而不是静默丢弃旧依据。
        if refs.len() > 16 {
            return Err(Error::invalid("修订关联来源超过16条，请先人工整理"));
        }
        self.correct_tx(
            tx,
            space,
            Correct {
                memory_id: replacement.memory_id,
                expected_revision: replacement.expected_revision,
                request_id: Uuid::now_v7(),
                reason,
                content: Some(replacement.content),
                scope: None,
                source_refs: Some(refs.into_iter().map(|id| id.to_string()).collect()),
                memory_type: None,
                links: None,
            },
        )
        .await
    }

    pub(super) fn pending_proposal_count(result: &Value) -> usize {
        result["decisions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| {
                d["action"] == "propose_revision"
                    && d["applied"] != true
                    && d["resolution"] != "superseded"
            })
            .count()
    }

    /// 只扫描仍有待确认结果的任务；JSON 路径固定，目标 ID 通过绑定参数传递。
    async fn proposals_for<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<Vec<jobs::Model>> {
        use sea_orm::sea_query::Expr;
        Ok(jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .filter(jobs::Column::State.eq("done"))
            .filter(Expr::cust("JSON_EXTRACT(result, '$.pending_revisions') > 0"))
            .filter(Expr::cust_with_values("JSON_CONTAINS(JSON_EXTRACT(result, '$.decisions[*].replacement.memory_id'), JSON_QUOTE(?))", [id.to_string()]))
            .all(db).await?)
    }

    pub(super) async fn pending_revisions<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<usize> {
        let mut count = 0;
        for row in Self::proposals_for(db, space, id).await? {
            let result = row.result.unwrap_or_default();
            count += result["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| {
                    d["replacement"]["memory_id"] == id.to_string()
                        && d["replacement"]["expected_revision"] == revision
                        && d["action"] == "propose_revision"
                        && d["applied"] != true
                        && d["resolution"] != "superseded"
                })
                .count();
        }
        Ok(count)
    }

    /// 任何成功更正都会使基于更旧修订的草案失效；不会自动把旧草案套到新正文上。
    pub(super) async fn supersede_proposals(
        tx: &DatabaseTransaction,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        for row in Self::proposals_for(tx, space, id).await? {
            let mut result = row.result.clone().unwrap_or_default();
            if let Some(decisions) = result["decisions"].as_array_mut() {
                for d in decisions {
                    if d["replacement"]["memory_id"] == id.to_string()
                        && d["replacement"]["expected_revision"]
                            .as_i64()
                            .is_some_and(|r| r < i64::from(revision))
                        && d["applied"] != true
                    {
                        d["resolution"] = json!("superseded");
                    }
                }
            }
            result["pending_revisions"] = json!(Self::pending_proposal_count(&result));
            let mut active = row.into_active_model();
            active.result = Set(Some(result));
            active.updated_at = Set(now());
            active.update(tx).await?;
        }
        Ok(())
    }
}
