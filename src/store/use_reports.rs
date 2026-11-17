//! 使用报告固定到原召回修订；只聚合来源 Agent 的观察，不把成功报告当作事实验证。
use super::*;
use crate::{
    entity::{recall_items, recall_runs},
    recall_usage::{MemoryUsage, UseOutcome},
};
use sea_orm::{FromQueryResult, JoinType, QueryOrder, RelationTrait};
use std::collections::HashMap;

#[derive(FromQueryResult)]
struct UseGroup {
    memory_id: Vec<u8>,
    revision: i32,
    use_outcome: String,
    count: i64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct UseEvidence {
    pub recall_id: Uuid,
    pub memory_id: Uuid,
    pub revision: i32,
    pub outcome: UseOutcome,
    pub reason: Option<String>,
    pub agent_id: Uuid,
    pub reported_at: String,
}
#[derive(FromQueryResult)]
struct EvidenceRow {
    recall_id: Vec<u8>,
    memory_id: Vec<u8>,
    revision: i32,
    use_outcome: String,
    use_reason: Option<String>,
    used_at: time::PrimitiveDateTime,
    agent_id: Vec<u8>,
}

impl Store {
    pub(super) async fn add_use_statistics<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        heads: &[(Uuid, i32)],
        result: &mut HashMap<Uuid, MemoryUsage>,
    ) -> Result<()> {
        let groups = recall_items::Entity::find()
            .join(
                JoinType::InnerJoin,
                recall_items::Relation::RecallRuns.def(),
            )
            .filter(recall_items::Column::SpaceId.eq(space.bytes()))
            .filter(recall_runs::Column::Channel.eq("agent"))
            .filter(
                recall_items::Column::MemoryId
                    .is_in(heads.iter().map(|(id, _)| id.as_bytes().to_vec())),
            )
            .filter(recall_items::Column::UseOutcome.is_not_null())
            .select_only()
            .column(recall_items::Column::MemoryId)
            .column(recall_items::Column::Revision)
            .column(recall_items::Column::UseOutcome)
            .column_as(recall_items::Column::MemoryId.count(), "count")
            .group_by(recall_items::Column::MemoryId)
            .group_by(recall_items::Column::Revision)
            .group_by(recall_items::Column::UseOutcome)
            .into_model::<UseGroup>()
            .all(db)
            .await?;
        for group in groups {
            let Some(usage) = result.get_mut(&uuid_from_bytes(&group.memory_id)?) else {
                continue;
            };
            if group.revision != usage.evaluated_revision {
                continue;
            }
            let count = u64::try_from(group.count).map_err(|_| Error::unavailable())?;
            usage.used_count += count;
            match UseOutcome::parse(&group.use_outcome)? {
                UseOutcome::Succeeded => usage.succeeded_count += count,
                UseOutcome::ContentOutdated => usage.outdated_count += count,
                UseOutcome::EnvironmentBlocked => usage.environment_blocked_count += count,
                UseOutcome::Unverified => usage.unverified_count += count,
            }
        }
        Ok(())
    }

    pub(super) async fn usage_evidence<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        heads: &[(Uuid, i32)],
    ) -> Result<Vec<UseEvidence>> {
        if heads.is_empty() {
            return Ok(vec![]);
        }
        // 每个目标保留最新问题，再留最新观察；成功高频上报不能挤掉触发本轮的依据。
        let mut rows = Vec::new();
        for (id, revision) in heads.iter().take(10) {
            let base = recall_items::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    recall_items::Relation::RecallRuns.def(),
                )
                .filter(recall_items::Column::SpaceId.eq(space.bytes()))
                .filter(recall_runs::Column::Channel.eq("agent"))
                .filter(recall_items::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .filter(recall_items::Column::Revision.eq(*revision))
                .filter(recall_items::Column::UseOutcome.is_not_null())
                .filter(recall_items::Column::UsedAt.is_not_null())
                .filter(recall_runs::Column::AgentId.is_not_null());
            for negative in [true, false] {
                let mut query = base.clone();
                if negative {
                    query = query.filter(
                        recall_items::Column::UseOutcome
                            .is_in(["content_outdated", "environment_blocked"]),
                    );
                }
                let row = query
                    .select_only()
                    .columns([
                        recall_items::Column::RecallId,
                        recall_items::Column::MemoryId,
                        recall_items::Column::Revision,
                        recall_items::Column::UseOutcome,
                        recall_items::Column::UseReason,
                        recall_items::Column::UsedAt,
                    ])
                    .column_as(
                        sea_orm::sea_query::Expr::col((
                            recall_runs::Entity,
                            recall_runs::Column::AgentId,
                        )),
                        "agent_id",
                    )
                    .order_by_desc(recall_items::Column::UsedAt)
                    .order_by_desc(recall_items::Column::RecallId)
                    .into_model::<EvidenceRow>()
                    .one(db)
                    .await?;
                if let Some(row) = row
                    && !rows.iter().any(|r: &EvidenceRow| {
                        r.recall_id == row.recall_id && r.memory_id == row.memory_id
                    })
                {
                    rows.push(row);
                }
            }
        }
        rows.into_iter()
            .map(|r| {
                Ok(UseEvidence {
                    recall_id: uuid_from_bytes(&r.recall_id)?,
                    memory_id: uuid_from_bytes(&r.memory_id)?,
                    revision: r.revision,
                    outcome: UseOutcome::parse(&r.use_outcome)?,
                    reason: r.use_reason,
                    agent_id: uuid_from_bytes(&r.agent_id)?,
                    reported_at: crate::types::timestamp(r.used_at)?,
                })
            })
            .collect()
    }

    pub(super) async fn check_usage_evidence<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        evidence: &[UseEvidence],
    ) -> Result<()> {
        for proof in evidence {
            let row = recall_items::Entity::find_by_id((
                space.bytes(),
                proof.recall_id.as_bytes().to_vec(),
                proof.memory_id.as_bytes().to_vec(),
            ))
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
            let run = recall_runs::Entity::find_by_id((
                space.bytes(),
                proof.recall_id.as_bytes().to_vec(),
            ))
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
            if row.revision != proof.revision
                || row.use_outcome.as_deref() != Some(proof.outcome.as_str())
                || row.use_reason != proof.reason
                || row
                    .used_at
                    .map(crate::types::timestamp)
                    .transpose()?
                    .as_deref()
                    != Some(proof.reported_at.as_str())
                || run.channel != "agent"
                || run.agent_id.as_deref() != Some(proof.agent_id.as_bytes().as_slice())
            {
                return Err(Error::conflict("knowledge_input_changed"));
            }
            if Self::head(db, space, proof.memory_id)
                .await?
                .forgotten_at
                .is_some()
            {
                return Err(Error::missing());
            }
        }
        Ok(())
    }
}
