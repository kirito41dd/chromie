//! 调度只登记有限工作；模型调用仍归持久 Worker。产物与已处理修订必须一起提交。
use super::knowledge_jobs::{AutomaticSnapshot, Snapshot};
use super::*;
use crate::{
    entity::{
        knowledge_jobs as jobs, memory_links, organization_items as items,
        organization_settings as settings, text_model_settings,
    },
    organization::Save,
    text_model::{JobKind, Queue},
};
use sea_orm::{
    Condition, ExprTrait, IntoActiveModel, PaginatorTrait, QueryOrder, Select,
    sea_query::{Expr, Query},
};

pub(super) fn heads(
    kind: &str,
    space: Option<SpaceId>,
    project: Option<Option<&str>>,
) -> Select<memories::Entity> {
    let mut revision = Query::select()
        .expr(Expr::val(1))
        .from(memory_revisions::Entity)
        .and_where(
            Expr::col((memory_revisions::Entity, memory_revisions::Column::SpaceId))
                .equals((memories::Entity, memories::Column::SpaceId)),
        )
        .and_where(
            Expr::col((memory_revisions::Entity, memory_revisions::Column::MemoryId))
                .equals((memories::Entity, memories::Column::Id)),
        )
        .and_where(
            Expr::col((memory_revisions::Entity, memory_revisions::Column::Revision))
                .equals((memories::Entity, memories::Column::CurrentRevision)),
        )
        .and_where(
            Expr::col((
                memory_revisions::Entity,
                memory_revisions::Column::MemoryType,
            ))
            .eq(kind),
        )
        .to_owned();
    if let Some(project) = project {
        revision.and_where(match project {
            Some(p) => Expr::col((
                memory_revisions::Entity,
                memory_revisions::Column::ProjectKey,
            ))
            .eq(p),
            None => Expr::col((
                memory_revisions::Entity,
                memory_revisions::Column::ProjectKey,
            ))
            .is_null(),
        });
    }
    let secret = Query::select()
        .expr(Expr::val(1))
        .from(crate::entity::secrets::Entity)
        .and_where(
            Expr::col((
                crate::entity::secrets::Entity,
                crate::entity::secrets::Column::SpaceId,
            ))
            .equals((memories::Entity, memories::Column::SpaceId)),
        )
        .and_where(
            Expr::col((
                crate::entity::secrets::Entity,
                crate::entity::secrets::Column::MemoryId,
            ))
            .equals((memories::Entity, memories::Column::Id)),
        )
        .to_owned();
    let mut query = memories::Entity::find()
        .filter(memories::Column::ForgottenAt.is_null())
        .filter(Expr::exists(revision))
        .filter(Expr::exists(secret).not());
    if let Some(space) = space {
        query = query.filter(memories::Column::SpaceId.eq(space.bytes()));
    }
    query
}
fn progress_for_head() -> sea_orm::sea_query::SelectStatement {
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
fn due(config: i64) -> sea_orm::sea_query::SelectStatement {
    let mut skip = progress_for_head();
    skip.cond_where(
        Condition::any()
            .add(Expr::col((items::Entity, items::Column::State)).eq("done"))
            .add(
                Condition::all()
                    .add(Expr::col((items::Entity, items::Column::ConfigRevision)).eq(config))
                    .add(
                        Condition::any()
                            .add(Expr::col((items::Entity, items::Column::State)).eq("queued"))
                            .add(Expr::col((items::Entity, items::Column::RunAfter)).gt(now())),
                    ),
            ),
    );
    skip
}
impl Store {
    pub(super) async fn initialize_organization<C: ConnectionTrait>(
        db: &C,
        enabled: bool,
    ) -> Result<()> {
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            revision: Set(1),
            enabled: Set(i8::from(enabled)),
            extraction_enabled: Set(0),
            last_kind: Set("organize".into()),
            batch_size: Set(10),
            interval_seconds: Set(60),
            daily_limit: Set(100),
            usage_date: Set(now().date()),
            attempts_today: Set(0),
            next_run_at: Set(now()),
            updated_at: Set(now()),
        })
        .on_conflict_do_nothing_on([settings::Column::Id])
        .exec_without_returning(db)
        .await?;
        Ok(())
    }
    pub async fn organization_settings(&self, space: SpaceId) -> Result<Value> {
        self.check_model_admin(space).await?;
        let row = settings::Entity::find_by_id(1u8).one(&self.db).await?;
        Ok(row.map_or_else(||json!({"revision":0,"enabled":false,"extraction_enabled":false,"batch_size":10,"interval_seconds":60,"daily_limit":100,"attempts_today":0}),|row|json!({"revision":row.revision,"enabled":row.enabled!=0,"extraction_enabled":row.extraction_enabled!=0,"batch_size":row.batch_size,"interval_seconds":row.interval_seconds,"daily_limit":row.daily_limit,"attempts_today":if row.usage_date==now().date(){row.attempts_today}else{0}})))
    }
    pub async fn save_organization(&self, space: SpaceId, input: Save) -> Result<Value> {
        self.check_model_admin(space).await?;
        if !(1..=10).contains(&input.batch_size)
            || !(10..=86400).contains(&input.interval_seconds)
            || !(1..=10000).contains(&input.daily_limit)
        {
            return Err(Error::invalid(
                "每批须为1～10条，间隔10～86400秒，每日尝试上限1～10000次",
            ));
        }
        let tx = self.db.begin().await?;
        let owner = Self::model_settings_locked(&tx).await?;
        if owner.admin_space_id != Some(space.bytes()) {
            return Err(Error::unauthorized());
        }
        let old = settings::Entity::find_by_id(1u8).one(&tx).await?;
        if old.as_ref().map_or(0, |row| row.revision) != input.expected_revision {
            return Err(Error::conflict("organization_settings_changed"));
        }
        Self::initialize_organization(&tx, false).await?;
        let mut row = settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?
            .into_active_model();
        row.revision = Set(input
            .expected_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?);
        row.enabled = Set(i8::from(input.enabled));
        row.extraction_enabled = Set(i8::from(input.extraction_enabled));
        row.batch_size = Set(input.batch_size);
        row.interval_seconds = Set(input.interval_seconds);
        row.daily_limit = Set(input.daily_limit);
        row.next_run_at = Set(now());
        row.updated_at = Set(now());
        row.update(&tx).await?;
        // 配置快照随设置变化失效；保留已完成修订，不让暂停造成历史重跑。
        jobs::Entity::update_many()
            .col_expr(jobs::Column::State, Expr::value("stale"))
            .col_expr(jobs::Column::Payload, Expr::value(Option::<Value>::None))
            .col_expr(
                jobs::Column::LeaseOwner,
                Expr::value(Option::<Vec<u8>>::None),
            )
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Option::<time::PrimitiveDateTime>::None),
            )
            .filter(jobs::Column::Automatic.eq(1))
            .filter(jobs::Column::State.is_in(["pending", "running", "retry"]))
            .exec(&tx)
            .await?;
        items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value("pending"))
            .col_expr(items::Column::RunAfter, Expr::value(now()))
            .filter(items::Column::State.eq("queued"))
            .exec(&tx)
            .await?;
        Self::release_extraction(&tx, None).await?;
        tx.commit().await?;
        self.organization_settings(space).await
    }
    /// 每次仅入队一个同空间、同范围的批次；管理锁防止多进程重复选取。
    pub async fn schedule_organization(&self) -> Result<usize> {
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
            .filter(|r| r.enabled != 0 && r.next_run_at <= now())
        else {
            return Ok(0);
        };
        if setting.usage_date == now().date() && setting.attempts_today >= setting.daily_limit {
            return Ok(0);
        }
        let Some(first) = heads("note", None, None)
            .filter(Expr::exists(due(config.revision)).not())
            .order_by_asc(memories::Column::UpdatedAt)
            .order_by_asc(memories::Column::Id)
            .one(&self.db)
            .await?
        else {
            return Ok(0);
        };
        let space = SpaceId(uuid_from_bytes(&first.space_id)?);
        let tx = self.begin(space).await?;
        Self::model_settings_locked(&tx).await?;
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .filter(|r| r.enabled != 0)
            .ok_or_else(Error::unavailable)?;
        let setting = settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?;
        if setting.enabled == 0
            || setting.next_run_at > now()
            || (setting.usage_date == now().date() && setting.attempts_today >= setting.daily_limit)
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
        let current = Self::head(&tx, space, uuid_from_bytes(&first.id)?).await?;
        if current.forgotten_at.is_some() {
            return Ok(0);
        }
        let first_revision = Self::revision(&tx, &current).await?;
        let group = Some(first_revision.project_key.as_deref());
        let selected = heads("note", Some(space), group)
            .filter(Expr::exists(due(config.revision)).not())
            .order_by_asc(memories::Column::UpdatedAt)
            .order_by_asc(memories::Column::Id)
            .limit(u64::from(setting.batch_size))
            .all(&tx)
            .await?;
        if selected.is_empty() {
            return Ok(0);
        }
        let mut work = selected
            .iter()
            .map(|r| uuid_from_bytes(&r.id))
            .collect::<Result<Vec<_>>>()?;
        let related = Query::select()
            .expr(Expr::val(1))
            .from(memory_links::Entity)
            .and_where(
                Expr::col((memory_links::Entity, memory_links::Column::SpaceId))
                    .equals((memories::Entity, memories::Column::SpaceId)),
            )
            .and_where(
                Expr::col((memory_links::Entity, memory_links::Column::MemoryId))
                    .equals((memories::Entity, memories::Column::Id)),
            )
            .and_where(
                Expr::col((memory_links::Entity, memory_links::Column::Revision))
                    .equals((memories::Entity, memories::Column::CurrentRevision)),
            )
            .and_where(
                Expr::col((memory_links::Entity, memory_links::Column::TargetId))
                    .is_in(selected.iter().map(|r| r.id.clone())),
            )
            .to_owned();
        let mut topics = heads("topic", Some(space), group)
            .filter(Expr::exists(related))
            .order_by_desc(memories::Column::UpdatedAt)
            .limit(6)
            .all(&tx)
            .await?;
        let more = heads("topic", Some(space), group)
            .order_by_desc(memories::Column::UpdatedAt)
            .limit(6)
            .all(&tx)
            .await?;
        for row in more {
            if topics.len() < 6 && !topics.iter().any(|r| r.id == row.id) {
                topics.push(row);
            }
        }
        let mut context = topics
            .into_iter()
            .map(|r| uuid_from_bytes(&r.id))
            .collect::<Result<Vec<_>>>()?;
        let id = Uuid::now_v7();
        loop {
            let mut ids = work.clone();
            ids.extend(context.iter());
            match self
                .enqueue_knowledge_tx(
                    &tx,
                    space,
                    Queue {
                        request_id: id,
                        kind: JobKind::Organize,
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
                Err(error)
                    if error.code == "knowledge_input_too_large"
                        && (work.len() > 1 || !context.is_empty()) =>
                {
                    if work.len() > 1 {
                        work.pop();
                    } else {
                        context.pop();
                    }
                }
                Err(error) => return Err(error),
            }
        }
        for memory in &selected {
            if !work.contains(&uuid_from_bytes(&memory.id)?) {
                continue;
            }
            let row = items::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(memory.id.clone()),
                revision: Set(memory.current_revision),
                config_revision: Set(config.revision),
                state: Set("queued".into()),
                job_id: Set(Some(id.as_bytes().to_vec())),
                run_after: Set(now()),
                last_error: Set(None),
                completed_at: Set(None),
                updated_at: Set(now()),
            };
            items::Entity::insert(row)
                .on_conflict(
                    OnConflict::columns([items::Column::SpaceId, items::Column::MemoryId])
                        .update_columns([
                            items::Column::Revision,
                            items::Column::ConfigRevision,
                            items::Column::State,
                            items::Column::JobId,
                            items::Column::RunAfter,
                            items::Column::LastError,
                            items::Column::CompletedAt,
                            items::Column::UpdatedAt,
                        ])
                        .to_owned(),
                )
                .exec_without_returning(&tx)
                .await?;
        }
        let mut setting = setting.into_active_model();
        setting.next_run_at =
            Set(now() + time::Duration::seconds(i64::from(*setting.interval_seconds.as_ref())));
        setting.last_kind = Set("organize".into());
        setting.updated_at = Set(now());
        setting.update(&tx).await?;
        tx.commit().await?;
        Ok(work.len())
    }
    pub(super) async fn automatic_allowed<C: ConnectionTrait>(db: &C) -> Result<bool> {
        Ok(settings::Entity::find_by_id(1u8)
            .one(db)
            .await?
            .is_some_and(|r| {
                (r.enabled != 0 || r.extraction_enabled != 0)
                    && (r.usage_date != now().date() || r.attempts_today < r.daily_limit)
            }))
    }
    pub(super) async fn count_automatic_attempt<C: ConnectionTrait>(db: &C) -> Result<()> {
        let mut row = settings::Entity::find_by_id(1u8)
            .one(db)
            .await?
            .ok_or_else(Error::unavailable)?
            .into_active_model();
        row.attempts_today = Set(if *row.usage_date.as_ref() == now().date() {
            row.attempts_today.as_ref().saturating_add(1)
        } else {
            1
        });
        row.usage_date = Set(now().date());
        row.update(db).await?;
        Ok(())
    }
    pub(super) async fn check_automatic<C: ConnectionTrait>(
        db: &C,
        snapshot: &Snapshot,
    ) -> Result<()> {
        if let Some(automatic) = &snapshot.automatic {
            let row = settings::Entity::find_by_id(1u8)
                .one(db)
                .await?
                .ok_or_else(Error::unavailable)?;
            if (if snapshot.kind == JobKind::Extract {
                row.extraction_enabled == 0
            } else {
                row.enabled == 0
            }) || row.revision != automatic.settings_revision
            {
                return Err(Error::conflict("organization_settings_changed"));
            }
        }
        Ok(())
    }
    pub(super) async fn finish_organization<C: ConnectionTrait>(
        db: &C,
        row: &jobs::Model,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        Self::finish_extraction(db, row, state, error).await?;
        if row.automatic == 0 || row.kind != "organize" {
            return Ok(());
        }
        items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value(state))
            .col_expr(
                items::Column::LastError,
                Expr::value(error.map(str::to_owned)),
            )
            .col_expr(
                items::Column::RunAfter,
                Expr::value(if state == "pending" {
                    now()
                } else {
                    now() + time::Duration::hours(24)
                }),
            )
            .col_expr(
                items::Column::CompletedAt,
                Expr::value(if state == "done" { Some(now()) } else { None }),
            )
            .col_expr(items::Column::UpdatedAt, Expr::value(now()))
            .filter(items::Column::SpaceId.eq(row.space_id.clone()))
            .filter(items::Column::JobId.eq(row.id.clone()))
            .exec(db)
            .await?;
        Ok(())
    }
    pub(super) async fn organization_status(&self, space: SpaceId) -> Result<Value> {
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?;
        let setting = settings::Entity::find_by_id(1u8).one(&self.db).await?;
        let total = heads("note", Some(space), None).count(&self.db).await?;
        let mut done = progress_for_head();
        done.and_where(Expr::col((items::Entity, items::Column::State)).eq("done"));
        let completed = heads("note", Some(space), None)
            .filter(Expr::exists(done))
            .count(&self.db)
            .await?;
        let mut failed = progress_for_head();
        failed.and_where(Expr::col((items::Entity, items::Column::State)).eq("failed"));
        let failed = heads("note", Some(space), None)
            .filter(Expr::exists(failed))
            .count(&self.db)
            .await?;
        let enabled = config.as_ref().is_some_and(|c| c.enabled != 0)
            && setting.as_ref().is_some_and(|s| s.enabled != 0);
        let exhausted = setting
            .as_ref()
            .is_some_and(|s| s.usage_date == now().date() && s.attempts_today >= s.daily_limit);
        let running = jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .filter(jobs::Column::Automatic.eq(1))
            .filter(jobs::Column::State.eq("running"))
            .one(&self.db)
            .await?
            .is_some();
        Ok(
            json!({"enabled":enabled,"state":if config.as_ref().is_none_or(|c|c.enabled==0){"unconfigured"}else if !enabled{"paused"}else if completed==total{"idle"}else if running{"running"}else if exhausted{"budget_limited"}else{"pending"},"total":total,"completed":completed,"pending":total.saturating_sub(completed),"failed":failed,"attempts_today":setting.as_ref().map_or(0,|s|if s.usage_date==now().date(){s.attempts_today}else{0}),"daily_limit":setting.as_ref().map_or(100,|s|s.daily_limit)}),
        )
    }
}
