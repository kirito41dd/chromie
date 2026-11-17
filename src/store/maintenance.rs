//! 独立维护按每条记忆的检查时间轮转，不要求新增记忆；反馈请求与快照覆盖边界防止漏消费。
use super::knowledge_jobs::AutomaticSnapshot;
use super::*;
use crate::{
    entity::{
        knowledge_jobs as jobs, maintenance_items as items, organization_settings as settings,
        text_model_settings,
    },
    text_model::{JobKind, Queue},
};
use sea_orm::{
    ExprTrait, IntoActiveModel, PaginatorTrait, QueryOrder, Select,
    sea_query::{Expr, Query},
};

fn heads(space: Option<SpaceId>, scope: Option<Option<&str>>) -> Select<memories::Entity> {
    super::organization::head_types(&["note", "topic"], space, scope)
}
fn progress() -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .expr(Expr::val(1))
        .from(items::Entity)
        .and_where(
            Expr::col((items::Entity, items::Column::SpaceId))
                .equals((memories::Entity, memories::Column::SpaceId)),
        )
        .and_where(
            Expr::col((items::Entity, items::Column::MemoryId))
                .equals((memories::Entity, memories::Column::Id)),
        )
        .and_where(
            Expr::col((items::Entity, items::Column::Revision))
                .equals((memories::Entity, memories::Column::CurrentRevision)),
        )
        .to_owned()
}
fn covered(hours: u32) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(items::Column::State.eq("done"))
        .add(items::Column::CompletedAt.gt(now() - time::Duration::hours(i64::from(hours))))
        .add(
            Expr::col((items::Entity, items::Column::RequestedAt))
                .lte(Expr::col((items::Entity, items::Column::CompletedAt))),
        )
}
fn skip(config: i64, hours: u32) -> sea_orm::sea_query::SelectStatement {
    let mut q = progress();
    q.cond_where(
        sea_orm::Condition::any()
            .add(items::Column::State.eq("queued"))
            .add(covered(hours))
            .add(
                sea_orm::Condition::all()
                    .add(items::Column::ConfigRevision.eq(config))
                    .add(
                        sea_orm::Condition::any()
                            .add(items::Column::State.eq("blocked"))
                            .add(
                                sea_orm::Condition::all()
                                    .add(items::Column::State.eq("failed"))
                                    .add(items::Column::RunAfter.gt(now())),
                            ),
                    ),
            ),
    );
    q
}
impl Store {
    /// 负向报告只提出维护请求，不直接改正文；在途任务保留其原租约和输入。
    pub(super) async fn request_maintenance(
        tx: &DatabaseTransaction,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        if crate::entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(tx)
            .await?
            .is_some()
        {
            return Ok(());
        }
        if let Some(row) = items::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(tx)
            .await?
        {
            let queued = row.state == "queued";
            let mut row = row.into_active_model();
            row.revision = Set(revision);
            row.requested_at = Set(now());
            row.updated_at = Set(now());
            if !queued {
                row.state = Set("pending".into());
                row.run_after = Set(now());
            }
            row.update(tx).await?;
        } else {
            items::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                revision: Set(revision),
                config_revision: Set(0),
                state: Set("pending".into()),
                job_id: Set(None),
                requested_at: Set(now()),
                run_after: Set(now()),
                completed_at: Set(None),
                updated_at: Set(now()),
                last_error: Set(None),
            }
            .insert(tx)
            .await?;
        }
        Ok(())
    }

    pub async fn schedule_maintenance(&self) -> Result<usize> {
        let Some(config) = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|c| c.enabled != 0)
        else {
            return Ok(0);
        };
        let Some(setting) = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|s| s.maintenance_enabled != 0 && s.next_run_at <= now())
        else {
            return Ok(0);
        };
        if !Self::automatic_allowed(&self.db).await? {
            return Ok(0);
        }
        let mut urgent = progress();
        urgent.and_where(items::Column::State.eq("pending"));
        let query = heads(None, None)
            .filter(Expr::exists(skip(config.revision, setting.maintenance_interval_hours)).not());
        let first = query
            .clone()
            .filter(Expr::exists(urgent))
            .order_by_asc(memories::Column::UpdatedAt)
            .one(&self.db)
            .await?;
        let first = match first {
            Some(first) => Some(first),
            None => {
                query
                    .order_by_asc(memories::Column::UpdatedAt)
                    .order_by_asc(memories::Column::Id)
                    .one(&self.db)
                    .await?
            }
        };
        let Some(first) = first else { return Ok(0) };
        let space = SpaceId(uuid_from_bytes(&first.space_id)?);
        let tx = self.begin(space).await?;
        Self::model_settings_locked(&tx).await?;
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .filter(|c| c.enabled != 0)
            .ok_or_else(Error::unavailable)?;
        let setting = settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?;
        if setting.maintenance_enabled == 0
            || setting.next_run_at > now()
            || !Self::automatic_allowed(&tx).await?
        {
            return Ok(0);
        }
        if jobs::Entity::find()
            .filter(jobs::Column::Automatic.eq(1))
            .filter(jobs::Column::State.is_in(["pending", "running", "retry"]))
            .one(&tx)
            .await?
            .is_some()
        {
            return Ok(0);
        }
        let first = Self::head(&tx, space, uuid_from_bytes(&first.id)?).await?;
        if first.forgotten_at.is_some() {
            return Ok(0);
        }
        let revision = Self::revision(&tx, &first).await?;
        if !["note", "topic"].contains(&revision.memory_type.as_str()) {
            return Ok(0);
        }
        let group = Some(revision.project_key.as_deref());
        let previous = items::Entity::find_by_id((space.bytes(), first.id.clone()))
            .one(&tx)
            .await?;
        let batch = if let Some(previous) = previous.filter(|p| {
            p.config_revision == config.revision
                && p.last_error
                    .as_deref()
                    .is_some_and(crate::text_model::structural_error)
        }) {
            Self::smaller_knowledge_batch(&tx, space, previous.job_id, setting.batch_size).await?
        } else {
            u64::from(setting.batch_size)
        };
        let candidates = heads(Some(space), group)
            .filter(Expr::exists(skip(config.revision, setting.maintenance_interval_hours)).not())
            .order_by_asc(memories::Column::UpdatedAt)
            .order_by_asc(memories::Column::Id)
            .limit(batch)
            .all(&tx)
            .await?;
        // 把明确被请求的目标放在本批，而不是被更早的普通巡检任务挤出。
        let mut selected = vec![];
        if candidates.iter().any(|r| r.id == first.id) {
            selected = candidates;
        } else {
            let still_due = heads(Some(space), group)
                .filter(memories::Column::Id.eq(first.id.clone()))
                .filter(
                    Expr::exists(skip(config.revision, setting.maintenance_interval_hours)).not(),
                )
                .one(&tx)
                .await?;
            if let Some(first) = still_due {
                selected.push(first);
                selected.extend(
                    candidates
                        .into_iter()
                        .take(batch.saturating_sub(1) as usize),
                );
            }
        }
        if selected.is_empty() {
            return Ok(0);
        }
        let mut work = selected
            .iter()
            .map(|r| uuid_from_bytes(&r.id))
            .collect::<Result<Vec<_>>>()?;
        let mut context = super::organization::heads("topic", Some(space), group)
            .order_by_asc(memories::Column::UpdatedAt)
            .limit(54)
            .all(&tx)
            .await?
            .into_iter()
            .map(|r| uuid_from_bytes(&r.id))
            .collect::<Result<Vec<_>>>()?;
        context.retain(|id| !work.contains(id));
        let id = Uuid::now_v7();
        let requested_boundary = now();
        loop {
            let mut ids = work.clone();
            ids.extend(context.iter());
            match self
                .enqueue_knowledge_tx(
                    &tx,
                    space,
                    Queue {
                        request_id: id,
                        kind: JobKind::Maintain,
                        memory_ids: ids,
                        event_ids: vec![],
                    },
                    Some(AutomaticSnapshot {
                        settings_revision: setting.revision,
                        memory_ids: work.clone(),
                        conversation: None,
                    }),
                )
                .await
            {
                Ok(_) => break,
                Err(e)
                    if e.code == "knowledge_input_too_large"
                        && (work.len() > 1 || !context.is_empty()) =>
                {
                    if !context.is_empty() {
                        context.pop();
                    } else {
                        work.pop();
                    }
                }
                Err(e) => return Err(e),
            }
        }
        for row in selected.into_iter().filter(|r| {
            work.iter()
                .any(|id| id.as_bytes().as_slice() == r.id.as_slice())
        }) {
            let old = items::Entity::find_by_id((space.bytes(), row.id.clone()))
                .one(&tx)
                .await?;
            let active = items::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(row.id),
                revision: Set(row.current_revision),
                config_revision: Set(config.revision),
                state: Set("queued".into()),
                job_id: Set(Some(id.as_bytes().to_vec())),
                requested_at: Set(old.as_ref().map_or(requested_boundary, |r| r.requested_at)),
                run_after: Set(now()),
                completed_at: Set(old.and_then(|r| r.completed_at)),
                updated_at: Set(now()),
                last_error: Set(None),
            };
            items::Entity::insert(active)
                .on_conflict(
                    OnConflict::columns([items::Column::SpaceId, items::Column::MemoryId])
                        .update_columns([
                            items::Column::Revision,
                            items::Column::ConfigRevision,
                            items::Column::State,
                            items::Column::JobId,
                            items::Column::RunAfter,
                            items::Column::UpdatedAt,
                            items::Column::LastError,
                        ])
                        .to_owned(),
                )
                .exec_without_returning(&tx)
                .await?;
        }
        let mut active = setting.into_active_model();
        active.last_kind = Set("maintain".into());
        active.next_run_at =
            Set(now() + time::Duration::seconds(i64::from(*active.interval_seconds.as_ref())));
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(work.len())
    }

    pub(super) async fn finish_maintenance<C: ConnectionTrait>(
        db: &C,
        row: &jobs::Model,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if row.automatic == 0 || row.kind != "maintain" {
            return Ok(());
        }
        let setting = settings::Entity::find_by_id(1u8)
            .one(db)
            .await?
            .ok_or_else(Error::unavailable)?;
        let structural =
            state == "failed" && error.is_some_and(crate::text_model::structural_error);
        let desired = if structural && Self::knowledge_work_items(row) <= 1 {
            "blocked"
        } else {
            state
        };
        let rows = items::Entity::find()
            .filter(items::Column::SpaceId.eq(row.space_id.clone()))
            .filter(items::Column::JobId.eq(row.id.clone()))
            .all(db)
            .await?;
        for item in rows {
            let head = Self::head(
                db,
                SpaceId(uuid_from_bytes(&row.space_id)?),
                uuid_from_bytes(&item.memory_id)?,
            )
            .await?;
            let new_request = item.requested_at > row.created_at;
            let mut active = item.into_active_model();
            active.state = Set(if new_request { "pending" } else { desired }.into());
            active.last_error = Set(error.map(str::to_owned));
            active.updated_at = Set(now());
            if state == "done" {
                active.revision = Set(head.current_revision);
                active.completed_at = Set(Some(row.created_at));
            }
            active.run_after = Set(if new_request || state == "pending" {
                now()
            } else if state == "done" {
                now() + time::Duration::hours(i64::from(setting.maintenance_interval_hours))
            } else if structural {
                now() + time::Duration::seconds(30)
            } else {
                now() + time::Duration::hours(24)
            });
            active.update(db).await?;
        }
        Ok(())
    }

    pub(super) async fn release_maintenance<C: ConnectionTrait>(
        db: &C,
        space: Option<SpaceId>,
    ) -> Result<()> {
        let mut query = items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value("pending"))
            .col_expr(items::Column::RunAfter, Expr::value(now()))
            .filter(items::Column::State.eq("queued"));
        if let Some(space) = space {
            query = query.filter(items::Column::SpaceId.eq(space.bytes()));
        }
        query.exec(db).await?;
        Ok(())
    }

    pub(super) async fn maintenance_status(&self, space: SpaceId) -> Result<Value> {
        let setting = settings::Entity::find_by_id(1u8).one(&self.db).await?;
        let hours = setting
            .as_ref()
            .map_or(24, |s| s.maintenance_interval_hours);
        let total = heads(Some(space), None).count(&self.db).await?;
        let mut done = progress();
        done.cond_where(covered(hours));
        let completed = heads(Some(space), None)
            .filter(Expr::exists(done))
            .count(&self.db)
            .await?;
        let mut failed = progress();
        failed.and_where(items::Column::State.is_in(["failed", "blocked"]));
        let failed = heads(Some(space), None)
            .filter(Expr::exists(failed))
            .count(&self.db)
            .await?;
        let model = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?;
        let enabled = setting.as_ref().is_some_and(|s| s.maintenance_enabled != 0);
        let exhausted = setting
            .as_ref()
            .is_some_and(|s| s.usage_date == now().date() && s.attempts_today >= s.daily_limit);
        Ok(
            json!({"enabled":enabled,"interval_hours":hours,"total":total,"completed":completed,"pending":total.saturating_sub(completed),"failed":failed,"state":if model.is_none_or(|m|m.enabled==0){"unconfigured"}else if !enabled{"paused"}else if completed==total{"idle"}else if exhausted{"budget_limited"}else{"pending"}}),
        )
    }
}
