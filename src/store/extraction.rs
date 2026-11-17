//! 事件级消费记录是进度事实；会话 ID 和标题不充当游标，迟到补传也会被发现。
use super::knowledge_jobs::{AutomaticSnapshot, ConversationSnapshot};
use super::*;
use crate::{
    entity::{
        extraction_items as items, knowledge_jobs as jobs, organization_settings as settings,
        source_events as events, text_model_settings,
    },
    messages::Conversation,
    text_model::{JobKind, Queue},
};
use sea_orm::{
    Condition, ExprTrait, IntoActiveModel, PaginatorTrait, QueryOrder,
    sea_query::{Expr, Query},
};

fn skip(config: i64) -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .expr(Expr::val(1))
        .from(items::Entity)
        .and_where(
            Expr::col((items::Entity, items::Column::SpaceId))
                .equals((events::Entity, events::Column::SpaceId)),
        )
        .and_where(
            Expr::col((items::Entity, items::Column::EventId))
                .equals((events::Entity, events::Column::Id)),
        )
        .cond_where(
            Condition::any().add(items::Column::State.eq("done")).add(
                Condition::all()
                    .add(items::Column::ConfigRevision.eq(config))
                    .add(
                        Condition::any()
                            .add(items::Column::State.eq("queued"))
                            .add(items::Column::State.eq("blocked"))
                            .add(items::Column::RunAfter.gt(now())),
                    ),
            ),
        )
        .to_owned()
}
fn due(config: i64) -> sea_orm::Select<events::Entity> {
    events::Entity::find()
        .filter(events::Column::ForgottenAt.is_null())
        .filter(events::Column::Body.is_not_null())
        .filter(Expr::exists(skip(config)).not())
        .filter(events::Column::ReceivedAt.lte(now() - time::Duration::seconds(30)))
}
pub(super) fn in_conversation(
    query: sea_orm::Select<events::Entity>,
    key: &Conversation,
) -> sea_orm::Select<events::Entity> {
    let q = query.filter(events::Column::SessionId.eq(&key.session_id));
    match key.agent_id {
        Some(id) => q.filter(events::Column::AgentId.eq(id.as_bytes().to_vec())),
        None => q.filter(events::Column::AgentId.is_null()),
    }
}
impl Store {
    /// 两类业务交替获得同一个自动工作槽；空的一类不会阻塞另一类。
    pub async fn schedule_automatic(&self) -> Result<usize> {
        let last = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .map(|s| s.last_kind)
            .unwrap_or_else(|| "maintain".into());
        let order = match last.as_str() {
            "organize" => [JobKind::Extract, JobKind::Maintain, JobKind::Organize],
            "extract" => [JobKind::Maintain, JobKind::Organize, JobKind::Extract],
            _ => [JobKind::Organize, JobKind::Extract, JobKind::Maintain],
        };
        for kind in order {
            let count = match kind {
                JobKind::Extract => self.schedule_extraction().await?,
                JobKind::Organize => self.schedule_organization().await?,
                JobKind::Maintain => self.schedule_maintenance().await?,
            };
            if count > 0 {
                return Ok(count);
            }
        }
        Ok(0)
    }
    pub async fn schedule_extraction(&self) -> Result<usize> {
        let Some(config) = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|r| r.enabled != 0)
        else {
            return Ok(0);
        };
        let Some(setting) = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|s| s.extraction_enabled != 0 && s.next_run_at <= now())
        else {
            return Ok(0);
        };
        if setting.usage_date == now().date() && setting.attempts_today >= setting.daily_limit {
            return Ok(0);
        }
        let Some(first) = due(config.revision)
            .order_by_asc(events::Column::ReceivedAt)
            .order_by_asc(events::Column::Id)
            .one(&self.db)
            .await?
        else {
            return Ok(0);
        };
        let space = SpaceId(uuid_from_bytes(&first.space_id)?);
        let key = Conversation {
            agent_id: first.agent_id.as_deref().map(uuid_from_bytes).transpose()?,
            session_id: first.session_id,
        };
        let tx = self.begin(space).await?;
        Self::model_settings_locked(&tx).await?;
        let Some(config) = text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .filter(|r| r.enabled != 0)
        else {
            return Ok(0);
        };
        let setting = settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?;
        if setting.extraction_enabled == 0
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
        let selected = in_conversation(
            due(config.revision).filter(events::Column::SpaceId.eq(space.bytes())),
            &key,
        )
        .order_by_asc(events::Column::OccurredAt)
        .order_by_asc(events::Column::Id)
        .limit(batch)
        .all(&tx)
        .await?;
        let Some(first) = selected.first() else {
            return Ok(0);
        };
        let work = selected
            .iter()
            .map(|r| uuid_from_bytes(&r.id))
            .collect::<Result<Vec<_>>>()?;
        let mut work = work;
        let mut context = in_conversation(
            events::Entity::find()
                .filter(events::Column::SpaceId.eq(space.bytes()))
                .filter(events::Column::ForgottenAt.is_null()),
            &key,
        )
        .filter(events::Column::OccurredAt.lte(first.occurred_at))
        .filter(events::Column::Id.is_not_in(selected.iter().map(|r| r.id.clone())))
        .order_by_desc(events::Column::OccurredAt)
        .order_by_desc(events::Column::Id)
        .limit(4)
        .all(&tx)
        .await?
        .into_iter()
        .map(|r| uuid_from_bytes(&r.id))
        .collect::<Result<Vec<_>>>()?;
        let mut memories = vec![];
        let metadata = Self::ensure_conversation(&tx, space, &key).await?;
        let id = Uuid::now_v7();
        loop {
            let mut event_ids = work.clone();
            event_ids.extend(context.iter());
            let input = Queue {
                request_id: id,
                kind: JobKind::Extract,
                memory_ids: memories.clone(),
                event_ids,
            };
            let automatic = AutomaticSnapshot {
                settings_revision: setting.revision,
                memory_ids: vec![],
                conversation: Some(ConversationSnapshot {
                    conversation: key.clone(),
                    title: metadata.title.clone(),
                    title_revision: metadata.revision,
                    title_locked: metadata.title_locked != 0,
                    work_event_ids: work.clone(),
                }),
            };
            // 显式空记忆列表仍允许零上下文；不会为缩小输入反而重新选满最近记忆。
            match self
                .enqueue_knowledge_tx(&tx, space, input, Some(automatic))
                .await
            {
                Ok(_) => break,
                Err(e)
                    if e.code == "knowledge_input_too_large"
                        && (!context.is_empty() || !memories.is_empty() || work.len() > 1) =>
                {
                    if !context.is_empty() {
                        context.pop();
                    } else if !memories.is_empty() {
                        memories.pop();
                    } else {
                        work.pop();
                    }
                }
                Err(e) if e.code == "knowledge_input_too_large" => {
                    // 原始字节合法仍可能因 JSON 转义膨胀，毒性单项不能卡住全实例调度。
                    for event in &work {
                        items::Entity::insert(items::ActiveModel {
                            space_id: Set(space.bytes()),
                            event_id: Set(event.as_bytes().to_vec()),
                            config_revision: Set(config.revision),
                            state: Set("failed".into()),
                            job_id: Set(None),
                            run_after: Set(now() + time::Duration::hours(24)),
                            last_error: Set(Some(e.code.into())),
                            updated_at: Set(now()),
                        })
                        .on_conflict(
                            OnConflict::columns([items::Column::SpaceId, items::Column::EventId])
                                .update_columns([
                                    items::Column::ConfigRevision,
                                    items::Column::State,
                                    items::Column::JobId,
                                    items::Column::RunAfter,
                                    items::Column::LastError,
                                    items::Column::UpdatedAt,
                                ])
                                .to_owned(),
                        )
                        .exec_without_returning(&tx)
                        .await?;
                    }
                    tx.commit().await?;
                    return Ok(0);
                }
                Err(e) => return Err(e),
            }
        }
        for event in &work {
            items::Entity::insert(items::ActiveModel {
                space_id: Set(space.bytes()),
                event_id: Set(event.as_bytes().to_vec()),
                config_revision: Set(config.revision),
                state: Set("queued".into()),
                job_id: Set(Some(id.as_bytes().to_vec())),
                run_after: Set(now()),
                last_error: Set(None),
                updated_at: Set(now()),
            })
            .on_conflict(
                OnConflict::columns([items::Column::SpaceId, items::Column::EventId])
                    .update_columns([
                        items::Column::ConfigRevision,
                        items::Column::State,
                        items::Column::JobId,
                        items::Column::RunAfter,
                        items::Column::LastError,
                        items::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(&tx)
            .await?;
        }
        let mut active = setting.into_active_model();
        active.next_run_at =
            Set(now() + time::Duration::seconds(i64::from(*active.interval_seconds.as_ref())));
        active.last_kind = Set("extract".into());
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(work.len())
    }
    pub(super) async fn finish_extraction<C: ConnectionTrait>(
        db: &C,
        row: &jobs::Model,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if row.automatic == 0 || row.kind != "extract" {
            return Ok(());
        }
        let structural =
            state == "failed" && error.is_some_and(crate::text_model::structural_error);
        let state = if structural && Self::knowledge_work_items(row) <= 1 {
            "blocked"
        } else {
            state
        };
        items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value(state))
            .col_expr(
                items::Column::LastError,
                Expr::value(error.map(str::to_owned)),
            )
            .col_expr(
                items::Column::RunAfter,
                Expr::value(if structural {
                    now() + time::Duration::seconds(30)
                } else if state == "pending" {
                    now()
                } else {
                    now() + time::Duration::hours(24)
                }),
            )
            .col_expr(items::Column::UpdatedAt, Expr::value(now()))
            .filter(items::Column::SpaceId.eq(row.space_id.clone()))
            .filter(items::Column::JobId.eq(row.id.clone()))
            .exec(db)
            .await?;
        Ok(())
    }
    pub(super) async fn extraction_progress<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        key: &Conversation,
    ) -> Result<Value> {
        let total = in_conversation(
            events::Entity::find()
                .filter(events::Column::SpaceId.eq(space.bytes()))
                .filter(events::Column::ForgottenAt.is_null()),
            key,
        );
        let complete = Query::select()
            .expr(Expr::val(1))
            .from(items::Entity)
            .and_where(
                Expr::col((items::Entity, items::Column::SpaceId))
                    .equals((events::Entity, events::Column::SpaceId)),
            )
            .and_where(
                Expr::col((items::Entity, items::Column::EventId))
                    .equals((events::Entity, events::Column::Id)),
            )
            .and_where(items::Column::State.eq("done"))
            .to_owned();
        let done = total
            .clone()
            .filter(Expr::exists(complete))
            .count(db)
            .await?;
        let total = total.count(db).await?;
        Ok(json!({"total":total,"completed":done,"pending":total.saturating_sub(done)}))
    }
    pub(super) async fn newer_consumed_event<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        key: &Conversation,
        newest: time::PrimitiveDateTime,
    ) -> Result<bool> {
        let completed = Query::select()
            .expr(Expr::val(1))
            .from(items::Entity)
            .and_where(
                Expr::col((items::Entity, items::Column::SpaceId))
                    .equals((events::Entity, events::Column::SpaceId)),
            )
            .and_where(
                Expr::col((items::Entity, items::Column::EventId))
                    .equals((events::Entity, events::Column::Id)),
            )
            .and_where(items::Column::State.eq("done"))
            .to_owned();
        Ok(in_conversation(
            events::Entity::find()
                .filter(events::Column::SpaceId.eq(space.bytes()))
                .filter(events::Column::ForgottenAt.is_null()),
            key,
        )
        .filter(events::Column::OccurredAt.gt(newest))
        .filter(Expr::exists(completed))
        .one(db)
        .await?
        .is_some())
    }
    pub(super) async fn extraction_status(&self, space: SpaceId) -> Result<Value> {
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?;
        let setting = settings::Entity::find_by_id(1u8).one(&self.db).await?;
        let base = events::Entity::find()
            .filter(events::Column::SpaceId.eq(space.bytes()))
            .filter(events::Column::ForgottenAt.is_null());
        let total = base.clone().count(&self.db).await?;
        let completed = items::Entity::find()
            .filter(items::Column::SpaceId.eq(space.bytes()))
            .filter(items::Column::State.eq("done"))
            .filter(
                items::Column::EventId.in_subquery(
                    Query::select()
                        .column(events::Column::Id)
                        .from(events::Entity)
                        .and_where(events::Column::SpaceId.eq(space.bytes()))
                        .and_where(events::Column::ForgottenAt.is_null())
                        .to_owned(),
                ),
            )
            .count(&self.db)
            .await?;
        let enabled = setting.as_ref().is_some_and(|s| s.extraction_enabled != 0);
        let exhausted = setting
            .as_ref()
            .is_some_and(|s| s.usage_date == now().date() && s.attempts_today >= s.daily_limit);
        Ok(
            json!({"enabled":enabled,"total":total,"completed":completed,"pending":total.saturating_sub(completed),"state":if config.is_none_or(|c|c.enabled==0){"unconfigured"}else if !enabled{"paused"}else if total==completed{"idle"}else if exhausted{"budget_limited"}else{"pending"}}),
        )
    }
    pub(super) async fn release_extraction<C: ConnectionTrait>(
        db: &C,
        space: Option<SpaceId>,
    ) -> Result<()> {
        let mut q = items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value("pending"))
            .col_expr(items::Column::RunAfter, Expr::value(now()))
            .filter(items::Column::State.eq("queued"));
        if let Some(space) = space {
            q = q.filter(items::Column::SpaceId.eq(space.bytes()))
        }
        q.exec(db).await?;
        Ok(())
    }
}
