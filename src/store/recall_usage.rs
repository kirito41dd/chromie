use super::*;
use crate::{
    agent::KeyKind,
    entity::{recall_items, recall_runs},
    recall_usage::{FeedbackRecall, MemoryOrder, MemoryUsage},
    types::timestamp,
};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{FromQueryResult, IntoActiveModel, JoinType, QueryOrder, RelationTrait};
use std::collections::{HashMap, HashSet};
use time::PrimitiveDateTime;

// 聚合查询的投影，不是持久化表的第二套模型。
#[derive(FromQueryResult)]
struct UsageGroup {
    memory_id: Vec<u8>,
    revision: i32,
    channel: String,
    returned: i64,
    evaluated: i64,
    useful: i64,
    last_recalled_at: Option<PrimitiveDateTime>,
}

impl Store {
    pub(super) async fn record_recall(
        &self,
        space: SpaceId,
        result: &Value,
        web: bool,
    ) -> Result<Uuid> {
        let cards = result
            .get("memories")
            .and_then(Value::as_array)
            .ok_or_else(Error::unavailable)?;
        if cards.len() > 10 {
            return Err(Error::unavailable());
        }
        let channel = if web {
            "web"
        } else if self.actor.is_some() {
            "agent"
        } else {
            "internal"
        };
        if !web
            && self
                .actor
                .as_ref()
                .is_some_and(|actor| !matches!(actor.kind, KeyKind::Mcp))
        {
            return Err(Error::agent_unauthorized());
        }
        let tx = self.begin(space).await?;
        let id = Uuid::now_v7();
        recall_runs::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            agent_id: Set(if web { None } else { self.actor_bytes() }),
            channel: Set(channel.into()),
            created_at: Set(now()),
        }
        .insert(&tx)
        .await?;
        let mut seen = HashSet::new();
        for card in cards {
            let memory = card
                .get("memory_id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(Error::unavailable)?;
            let revision = card
                .get("revision")
                .and_then(Value::as_i64)
                .and_then(|revision| i32::try_from(revision).ok())
                .filter(|revision| *revision > 0)
                .ok_or_else(Error::unavailable)?;
            if !seen.insert(memory) {
                return Err(Error::unavailable());
            }
            recall_items::ActiveModel {
                space_id: Set(space.bytes()),
                recall_id: Set(id.as_bytes().to_vec()),
                memory_id: Set(memory.as_bytes().to_vec()),
                revision: Set(revision),
                useful: Set(None),
                evaluated_at: Set(None),
            }
            .insert(&tx)
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    /// 一次召回的一条记忆只有一次明确评价，相同重放不增计，冲突整批回滚。
    pub async fn feedback_recall(&self, space: SpaceId, input: FeedbackRecall) -> Result<Value> {
        input.validate()?;
        let actor = self
            .actor
            .as_ref()
            .filter(|actor| matches!(actor.kind, KeyKind::Mcp))
            .ok_or_else(Error::agent_unauthorized)?;
        let tx = self.begin(space).await?;
        let mut recorded = 0_u32;
        let mut replayed = 0_u32;
        for evaluation in input.recalls {
            let run = recall_runs::Entity::find_by_id((
                space.bytes(),
                evaluation.recall_id.as_bytes().to_vec(),
            ))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
            if run.channel != "agent"
                || run.agent_id.as_deref() != Some(actor.id.as_bytes().as_slice())
            {
                return Err(Error::missing());
            }
            let useful: HashSet<_> = evaluation.useful_memory_ids.into_iter().collect();
            for id in evaluation.evaluated_memory_ids {
                let head = Self::head(&tx, space, id).await?;
                if head.forgotten_at.is_some() {
                    return Err(Error::missing());
                }
                let row = recall_items::Entity::find_by_id((
                    space.bytes(),
                    run.id.clone(),
                    id.as_bytes().to_vec(),
                ))
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
                let judgement = i8::from(useful.contains(&id));
                if let Some(previous) = row.useful {
                    if previous != judgement {
                        return Err(Error::conflict("recall_feedback_conflict"));
                    }
                    replayed += 1;
                } else {
                    let mut active = row.into_active_model();
                    active.useful = Set(Some(judgement));
                    active.evaluated_at = Set(Some(now()));
                    active.update(&tx).await?;
                    recorded += 1;
                }
            }
        }
        tx.commit().await?;
        Ok(json!({"recorded":recorded,"replayed":replayed}))
    }

    /// 一次聚合取当前页面全部记忆，避免逐张卡片重复统计查询。
    pub(crate) async fn memory_usage<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        heads: &[(Uuid, i32)],
    ) -> Result<HashMap<Uuid, MemoryUsage>> {
        let mut result: HashMap<_, _> = heads
            .iter()
            .map(|(id, revision)| {
                (
                    *id,
                    MemoryUsage {
                        evaluated_revision: *revision,
                        ..Default::default()
                    },
                )
            })
            .collect();
        if heads.is_empty() {
            return Ok(result);
        }
        let positive = Expr::case(
            Expr::col((recall_items::Entity, recall_items::Column::Useful)).eq(1),
            1,
        )
        .finally(Expr::value(None::<i32>));
        let rows = recall_items::Entity::find()
            .join(
                JoinType::InnerJoin,
                recall_items::Relation::RecallRuns.def(),
            )
            .filter(recall_items::Column::SpaceId.eq(space.bytes()))
            .filter(
                recall_items::Column::MemoryId
                    .is_in(heads.iter().map(|(id, _)| id.as_bytes().to_vec())),
            )
            .select_only()
            .column(recall_items::Column::MemoryId)
            .column(recall_items::Column::Revision)
            .column_as(
                Expr::col((recall_runs::Entity, recall_runs::Column::Channel)),
                "channel",
            )
            .column_as(recall_items::Column::MemoryId.count(), "returned")
            .column_as(recall_items::Column::Useful.count(), "evaluated")
            .column_as(Expr::expr(positive).count(), "useful")
            .column_as(
                Expr::col((recall_runs::Entity, recall_runs::Column::CreatedAt)).max(),
                "last_recalled_at",
            )
            .group_by(recall_items::Column::MemoryId)
            .group_by(recall_items::Column::Revision)
            .group_by(Expr::col((
                recall_runs::Entity,
                recall_runs::Column::Channel,
            )))
            .into_model::<UsageGroup>()
            .all(db)
            .await?;
        let mut last_recalled = HashMap::<Uuid, PrimitiveDateTime>::new();
        for group in rows {
            // MySQL COUNT 返回有符号 BIGINT；外部计数使用显式校验后的无符号数。
            let returned = u64::try_from(group.returned).map_err(|_| Error::unavailable())?;
            let evaluated = u64::try_from(group.evaluated).map_err(|_| Error::unavailable())?;
            let useful = u64::try_from(group.useful).map_err(|_| Error::unavailable())?;
            let id = uuid_from_bytes(&group.memory_id)?;
            let Some(usage) = result.get_mut(&id) else {
                continue;
            };
            match group.channel.as_str() {
                "agent" => {
                    usage.agent_recall_count += returned;
                    if group.revision == usage.evaluated_revision {
                        usage.evaluated_count += evaluated;
                        usage.useful_count += useful;
                    }
                    if let Some(at) = group.last_recalled_at {
                        last_recalled
                            .entry(id)
                            .and_modify(|last| *last = std::cmp::max(*last, at))
                            .or_insert(at);
                    }
                }
                "web" => usage.web_recall_count += returned,
                _ => {}
            }
        }
        for (id, at) in last_recalled {
            if let Some(usage) = result.get_mut(&id) {
                usage.last_recalled_at = Some(timestamp(at)?);
            }
        }
        Ok(result)
    }

    pub(crate) async fn ranked_memories<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        order: MemoryOrder,
    ) -> Result<Vec<Uuid>> {
        let mut query = recall_items::Entity::find()
            .join(
                JoinType::InnerJoin,
                recall_items::Relation::RecallRuns.def(),
            )
            .join(JoinType::InnerJoin, recall_items::Relation::Memories.def())
            .filter(recall_items::Column::SpaceId.eq(space.bytes()))
            .filter(recall_runs::Column::Channel.eq("agent"))
            .filter(recall_runs::Column::CreatedAt.gte(now() - time::Duration::days(30)))
            .filter(memories::Column::ForgottenAt.is_null());
        if order == MemoryOrder::Useful {
            query = query.filter(recall_items::Column::Useful.eq(1)).filter(
                Expr::col((recall_items::Entity, recall_items::Column::Revision))
                    .equals((memories::Entity, memories::Column::CurrentRevision)),
            );
        }
        let rows = query
            .select_only()
            .column(recall_items::Column::MemoryId)
            .column_as(recall_items::Column::MemoryId.count(), "score")
            .group_by(recall_items::Column::MemoryId)
            .order_by_desc(recall_items::Column::MemoryId.count())
            .order_by_desc(Expr::col((recall_runs::Entity, recall_runs::Column::CreatedAt)).max())
            .order_by_asc(recall_items::Column::MemoryId)
            .limit(10)
            .into_tuple::<(Vec<u8>, i64)>()
            .all(db)
            .await?;
        rows.into_iter()
            .map(|(id, _)| uuid_from_bytes(&id))
            .collect()
    }
}
