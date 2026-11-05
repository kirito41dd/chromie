//! 只读取当前空间的索引覆盖情况；配置快照和代际读取在同一短事务内。
use super::*;
use crate::{
    entity::{
        embedding_generations as generations, embedding_tasks as tasks,
        instance_model_settings as settings,
    },
    retrieval_capabilities::{
        Capabilities, RetrievalCapabilities, RetrievalMode, SemanticReason, SemanticState,
    },
};
use sea_orm::{
    AccessMode, ExprTrait, IsolationLevel,
    sea_query::{Expr, Query},
};
use std::time::Duration;

impl Store {
    pub async fn capabilities(&self, space: SpaceId) -> Result<Capabilities> {
        let tx = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, false).await?;
        }
        let slug = spaces::Entity::find_by_id(space.bytes())
            .select_only()
            .column(spaces::Column::Slug)
            .into_tuple::<String>()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let row = settings::Entity::find_by_id(1u8).one(&tx).await?;
        let mut retrieval = RetrievalCapabilities {
            mode: RetrievalMode::Keyword,
            semantic_state: SemanticState::Disabled,
            reason: Some(SemanticReason::NotConfigured),
            config_revision: row.as_ref().map_or(0, |r| r.revision),
            rebuilding: row
                .as_ref()
                .is_some_and(|r| r.enabled != 0 && r.pending_generation.is_some()),
        };
        if let Some(row) = row {
            if row.enabled == 0 {
                if row.active_generation.is_some() || row.pending_generation.is_some() {
                    retrieval.reason = Some(SemanticReason::Disabled);
                }
            } else if let Some(active) = row.active_generation {
                let generation_state = generations::Entity::find_by_id(active.clone())
                    .select_only()
                    .column(generations::Column::State)
                    .into_tuple::<String>()
                    .one(&tx)
                    .await?
                    .ok_or_else(Error::unavailable)?;
                if generation_state != "active" {
                    retrieval.semantic_state = SemanticState::Degraded;
                    retrieval.reason = Some(SemanticReason::IndexUnavailable);
                } else {
                    let backed_off = self
                        .embedding_backoff
                        .lock()
                        .await
                        .get(&uuid_from_bytes(&active)?)
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(30));
                    let heads = memories::Entity::find()
                        .filter(memories::Column::SpaceId.eq(space.bytes()))
                        .filter(memories::Column::ForgottenAt.is_null());
                    if heads.clone().one(&tx).await?.is_none() {
                        retrieval.semantic_state = SemanticState::Empty;
                        retrieval.reason = Some(SemanticReason::EmptySpace);
                    } else if backed_off {
                        retrieval.semantic_state = SemanticState::Degraded;
                        retrieval.reason = Some(SemanticReason::RetryBackoff);
                    } else {
                        // 与实际查询一样使用当前生效代际；新代重建不抹掉旧代可用性。
                        let ready = Query::select()
                            .expr(Expr::val(1))
                            .from(tasks::Entity)
                            .and_where(
                                Expr::col((tasks::Entity, tasks::Column::GenerationId)).eq(active),
                            )
                            .and_where(
                                Expr::col((tasks::Entity, tasks::Column::SpaceId))
                                    .equals((memories::Entity, memories::Column::SpaceId)),
                            )
                            .and_where(
                                Expr::col((tasks::Entity, tasks::Column::MemoryId))
                                    .equals((memories::Entity, memories::Column::Id)),
                            )
                            .and_where(
                                Expr::col((tasks::Entity, tasks::Column::IndexedRevision))
                                    .equals((memories::Entity, memories::Column::CurrentRevision)),
                            )
                            .and_where(Expr::col((tasks::Entity, tasks::Column::State)).eq("done"))
                            .to_owned();
                        let incomplete = heads
                            .filter(Expr::exists(ready).not())
                            .one(&tx)
                            .await?
                            .is_some();
                        retrieval.mode = RetrievalMode::Hybrid;
                        retrieval.semantic_state = if incomplete {
                            SemanticState::Updating
                        } else {
                            SemanticState::Ready
                        };
                        retrieval.reason = None;
                    }
                }
            } else {
                retrieval.semantic_state = SemanticState::Building;
                retrieval.reason = Some(SemanticReason::InitialIndexBuilding);
            }
        }
        tx.commit().await?;
        Ok(Capabilities {
            capability_version: 1,
            space: slug,
            cache_ttl_seconds: 30,
            retrieval,
        })
    }
}
