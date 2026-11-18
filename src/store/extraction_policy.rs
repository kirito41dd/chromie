//! AI 自主修订的发布护栏：真实来源、来源时序及人工新决定仍由服务校验。
use super::knowledge_jobs::Snapshot;
use super::*;
use crate::{
    entity::{knowledge_jobs, memory_sources, source_events},
    extraction::{Category, Evidence, Replacement},
};

/// 发布拒绝保留稳定原因码；目标及原始证据留在任务结果中供排障，不生成审批。
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RevisionBlock {
    MissingEvidence,
    NewerManualRevision,
    NewerSource,
    SourceUnavailable,
    SourceCapacity,
    AmbiguousTarget,
    IncompleteMethod,
}
impl RevisionBlock {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::MissingEvidence => "没有可用于修订的真实引文",
            Self::NewerManualRevision => "引文早于人工或未核实来源的当前修订，保留当前记忆",
            Self::NewerSource => "当前记忆已有更新的来源，旧引文不能覆盖",
            Self::SourceUnavailable => "当前记忆的来源已不可用，暂不自动修订",
            Self::SourceCapacity => "合并后来源超过容量，暂不自动修订",
            Self::AmbiguousTarget => "同一批次对同一记忆给出多个方案，暂不自动修订",
            Self::IncompleteMethod => "方法缺少可执行细节，暂不自动修订",
        }
    }
}

impl Store {
    pub(super) async fn autonomous_change_is_current(
        tx: &DatabaseTransaction,
        space: SpaceId,
        snapshot: &Snapshot,
        replacement: &Replacement,
        source_refs: &[Uuid],
        evidence: &[Evidence],
        category: Category,
    ) -> Result<Option<RevisionBlock>> {
        // 候选和核对阶段已逐字校验引文；在其实际来源中取最新依据，不能用处理时间代替。
        let Some(event) = snapshot
            .events
            .iter()
            .filter(|e| evidence.iter().any(|proof| proof.event_id == e.event_id))
            .filter(|e| {
                !matches!(category, Category::Preference | Category::Decision) || e.role == "user"
            })
            .max_by_key(|e| (&e.occurred_at, e.event_id))
        else {
            return Ok(Some(RevisionBlock::MissingEvidence));
        };
        let basis =
            source_events::Entity::find_by_id((space.bytes(), event.event_id.as_bytes().to_vec()))
                .one(tx)
                .await?
                .ok_or_else(Error::missing)?;
        let previous = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(replacement.memory_id.as_bytes().to_vec()))
            .filter(memory_sources::Column::Revision.eq(replacement.expected_revision))
            .all(tx)
            .await?;
        let head = Self::head(tx, space, replacement.memory_id).await?;
        let revision = Self::revision(tx, &head).await?;
        let automatic = write_receipts::Entity::find()
            .filter(write_receipts::Column::SpaceId.eq(space.bytes()))
            .filter(write_receipts::Column::Operation.eq("background_revision"))
            .filter(write_receipts::Column::TargetId.eq(replacement.memory_id.as_bytes().to_vec()))
            .filter(write_receipts::Column::Revision.eq(replacement.expected_revision))
            .one(tx)
            .await?
            .is_some()
            || Self::verified_legacy_generation(tx, space, &revision).await?;
        if (!automatic || previous.is_empty()) && basis.occurred_at < revision.created_at {
            return Ok(Some(RevisionBlock::NewerManualRevision));
        }
        let mut refs = source_refs.to_vec();
        for old in previous {
            let id = uuid_from_bytes(&old.event_id)?;
            if !refs.contains(&id) {
                refs.push(id);
            }
            let source = source_events::Entity::find_by_id((space.bytes(), old.event_id))
                .one(tx)
                .await?
                .ok_or_else(Error::missing)?;
            if source.forgotten_at.is_some() || source.body.is_none() {
                return Ok(Some(RevisionBlock::SourceUnavailable));
            }
            if source.occurred_at > basis.occurred_at {
                return Ok(Some(RevisionBlock::NewerSource));
            }
        }
        Ok((refs.len() > 16).then_some(RevisionBlock::SourceCapacity))
    }

    /// 旧首版由 mark_generated 写入不可经公开创建参数设置的标记。仍须交叉验证
    /// 同空间后台任务及无 Agent 的创建回执；调用方可填写的更正理由不构成证明。
    /// 任务详情可能已压缩，任务小元数据与写回执始终保留，因此不依赖旧正文快照。
    async fn verified_legacy_generation(
        tx: &DatabaseTransaction,
        space: SpaceId,
        revision: &memory_revisions::Model,
    ) -> Result<bool> {
        if revision.revision != 1 || revision.agent_id.is_some() || revision.memory_type != "note" {
            return Ok(false);
        }
        let Some(job_id) = revision
            .change_reason
            .as_deref()
            .and_then(|s| s.strip_prefix("后台 LLM 整理：任务 "))
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(false);
        };
        let Some(job) =
            knowledge_jobs::Entity::find_by_id((space.bytes(), job_id.as_bytes().to_vec()))
                .one(tx)
                .await?
                .filter(|j| {
                    j.state == "done"
                        && matches!(j.kind.as_str(), "extract" | "organize" | "maintain")
                        && j.created_at <= revision.created_at
                        && j.updated_at >= revision.created_at
                })
        else {
            return Ok(false);
        };
        let verified = write_receipts::Entity::find()
            .filter(write_receipts::Column::SpaceId.eq(space.bytes()))
            .filter(write_receipts::Column::Operation.eq("remember"))
            .filter(write_receipts::Column::TargetId.eq(revision.memory_id.clone()))
            .filter(write_receipts::Column::Revision.eq(1))
            .filter(write_receipts::Column::AgentId.is_null())
            .filter(write_receipts::Column::CreatedAt.gte(revision.created_at))
            .filter(write_receipts::Column::CreatedAt.lte(job.updated_at))
            .one(tx)
            .await?
            .is_some();
        if verified {
            // 核实一次后持久保存内部来源，后续不依赖人类可读理由。
            Self::mark_automatic_revision(
                tx,
                space,
                uuid_from_bytes(&revision.memory_id)?,
                1,
                job_id,
            )
            .await?;
        }
        Ok(verified)
    }
}
