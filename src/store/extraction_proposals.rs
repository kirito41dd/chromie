//! 发布后的修订方案只由 Web 明确确认；空间锁与旧修订号共同阻止过期方案覆盖新内容。
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
        if row.kind != "extract" || row.state != "done" {
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
        if !user_evidence {
            return Err(Error::invalid("修订缺少用户证据"));
        }
        let head = Self::head(&tx, space, replacement.memory_id).await?;
        let old = Self::revision(&tx, &head).await?;
        if old.memory_type != MemoryType::Note.as_str()
            || crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
                .one(&tx)
                .await?
                .is_some()
        {
            return Err(Error::invalid("此方案只能修订普通记忆"));
        }
        let mut refs = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(head.id.clone()))
            .filter(memory_sources::Column::Revision.eq(head.current_revision))
            .all(&tx)
            .await?
            .into_iter()
            .map(|s| uuid_from_bytes(&s.event_id))
            .collect::<Result<Vec<_>>>()?;
        for id in source_refs {
            if !refs.contains(&id) {
                refs.push(id);
            }
        }
        // 来源没有上限扩张：超限时保留旧修订及方案，交由人工整理，而不是静默丢弃旧依据。
        if refs.len() > 16 {
            return Err(Error::invalid("修订关联来源超过16条，请先人工整理"));
        }
        let reason = decision["reason"]
            .as_str()
            .unwrap_or("确认对话提炼的修订方案")
            .to_owned();
        let receipt = self
            .correct_tx(
                &tx,
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
            .await?;
        decision["applied"] = json!(true);
        decision["receipt"] = receipt.clone();
        result["pending_revisions"] = json!(
            result["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["action"] == "propose_revision" && d["applied"] != true)
                .count()
        );
        let mut active = row.into_active_model();
        active.result = Set(Some(result));
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
