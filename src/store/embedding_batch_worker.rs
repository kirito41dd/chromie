//! 每批模型调用在事务外执行；领取和发布逐条持锁并复核修订、租约与代际。
use super::embedding_retry::{RETRYABLE_ERRORS, Recovery};
use super::*;
use crate::entity::{embedding_generations as generations, embedding_tasks as tasks};
use sea_orm::{
    Condition, ExprTrait, IntoActiveModel, QueryOrder, Select,
    sea_query::{Expr, Query},
};
use std::time::Duration;

struct ClaimedTask {
    generation: generations::Model,
    space: SpaceId,
    memory: Uuid,
    revision: i32,
    token: Uuid,
    attempts: i32,
    content: Option<String>,
    project: Option<String>,
}

impl Store {
    fn due_embedding_tasks() -> Select<tasks::Entity> {
        // 以非锁定查询跳过整代取消的生成任务，避免旧队列阻塞新代。
        // 已遗忘对象的删除仍可被领取，停用语义检索也不暂停删除传播。
        let live_generation = Query::select()
            .expr(Expr::val(1))
            .from(generations::Entity)
            .and_where(
                Expr::col((generations::Entity, generations::Column::Id))
                    .equals((tasks::Entity, tasks::Column::GenerationId)),
            )
            .and_where(
                Expr::col((generations::Entity, generations::Column::State))
                    .is_in(["active", "building"]),
            )
            .to_owned();
        let deleted_memory = Query::select()
            .expr(Expr::val(1))
            .from(memories::Entity)
            .and_where(
                Expr::col((memories::Entity, memories::Column::SpaceId))
                    .equals((tasks::Entity, tasks::Column::SpaceId)),
            )
            .and_where(
                Expr::col((memories::Entity, memories::Column::Id))
                    .equals((tasks::Entity, tasks::Column::MemoryId)),
            )
            .and_where(Expr::col((memories::Entity, memories::Column::ForgottenAt)).is_not_null())
            .to_owned();
        tasks::Entity::find()
            .filter(
                Condition::any()
                    .add(Expr::exists(live_generation))
                    .add(Expr::exists(deleted_memory)),
            )
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(tasks::Column::State.is_in(["pending", "retry"]))
                            .add(tasks::Column::RunAfter.lte(now())),
                    )
                    .add(
                        Condition::all()
                            .add(tasks::Column::State.eq("failed"))
                            .add(tasks::Column::LastError.is_in(RETRYABLE_ERRORS.iter().copied()))
                            .add(tasks::Column::RunAfter.lte(now())),
                    )
                    .add(
                        Condition::all()
                            .add(tasks::Column::State.eq("running"))
                            .add(tasks::Column::LeaseUntil.lte(now())),
                    ),
            )
            .order_by_asc(tasks::Column::RunAfter)
    }
    async fn claim_embedding_task(
        &self,
        task: tasks::Model,
        expected_connection: Option<i32>,
    ) -> Result<Option<ClaimedTask>> {
        let space = SpaceId(uuid_from_bytes(&task.space_id)?);
        let memory = uuid_from_bytes(&task.memory_id)?;
        let tx = self.begin(space).await?;
        // 所有任务写路径统一为：空间锁 → 实例锁 → 向量任务锁。
        // 配置更新和清理只持实例锁，不能先占任务行再等待实例锁。
        let state = Self::model_settings_locked(&tx).await?;
        let current = tasks::Entity::find_by_id((
            task.generation_id.clone(),
            task.space_id.clone(),
            task.memory_id.clone(),
        ))
        .lock_exclusive()
        .one(&tx)
        .await?
        .ok_or_else(Error::unavailable)?;
        if (current.state == "running" && current.lease_until.is_some_and(|until| until > now()))
            || !(["pending", "retry", "running"].contains(&current.state.as_str())
                || (current.state == "failed"
                    && current
                        .last_error
                        .as_deref()
                        .is_some_and(|code| RETRYABLE_ERRORS.contains(&code))))
            || current.run_after > now()
        {
            return Ok(None);
        }
        let generation = Self::generation(&tx, &current.generation_id).await?;
        if expected_connection.is_some_and(|revision| revision != generation.connection_revision) {
            return Ok(None);
        }
        let head = Self::head(&tx, space, memory).await?;
        let live = state.enabled != 0
            && [
                state.active_generation.as_ref(),
                state.pending_generation.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|id| *id == current.generation_id)
            && ["active", "building"].contains(&generation.state.as_str());
        if !live && head.forgotten_at.is_none() {
            let mut active = current.into_active_model();
            active.state = Set("skipped".into());
            active.lease_token = Set(None);
            active.lease_until = Set(None);
            active.update(&tx).await?;
            tx.commit().await?;
            return Ok(None);
        }
        let revision = head.current_revision;
        let (content, project) = if head.forgotten_at.is_none() {
            let revision = Self::revision(&tx, &head).await?;
            (
                Some(revision.content.ok_or_else(Error::missing)?),
                revision.project_key,
            )
        } else {
            (None, None)
        };
        let attempts = current.attempts;
        let token = Uuid::now_v7();
        let mut claimed = current.into_active_model();
        claimed.state = Set("running".into());
        claimed.target_revision = Set(revision);
        claimed.lease_token = Set(Some(token.as_bytes().to_vec()));
        claimed.lease_until = Set(Some(now() + time::Duration::seconds(120)));
        claimed.updated_at = Set(now());
        claimed.update(&tx).await?;
        tx.commit().await?;
        Ok(Some(ClaimedTask {
            generation,
            space,
            memory,
            revision,
            token,
            attempts,
            content,
            project,
        }))
    }

    async fn finish_embedding_task(
        &self,
        claim: ClaimedTask,
        vector: Result<Option<Vec<f32>>>,
    ) -> Result<()> {
        let ClaimedTask {
            generation,
            space,
            memory,
            revision,
            token,
            attempts,
            content,
            project,
        } = claim;
        let tx = self.begin(space).await?;
        let state = Self::model_settings_locked(&tx).await?;
        let Some(current) = tasks::Entity::find_by_id((
            generation.id.clone(),
            space.bytes(),
            memory.as_bytes().to_vec(),
        ))
        .one(&tx)
        .await?
        else {
            return Ok(());
        };
        if current.lease_token != Some(token.as_bytes().to_vec())
            || current.target_revision != revision
        {
            return Ok(());
        }
        let head = Self::head(&tx, space, memory).await?;
        let latest = Self::generation(&tx, &generation.id).await?;
        let allowed = state.enabled != 0
            && [
                state.active_generation.as_ref(),
                state.pending_generation.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|id| *id == generation.id)
            && ["active", "building"].contains(&latest.state.as_str());
        let mut active = current.into_active_model();
        active.lease_token = Set(None);
        active.lease_until = Set(None);
        active.updated_at = Set(now());
        if head.current_revision != revision
            || (head.forgotten_at.is_some() && content.is_some())
            || generation.connection_revision != latest.connection_revision
            // 调度设置不改变向量或连接身份；旧批错误不能覆盖已经保存的兼容批大小。
            || (vector.as_ref().is_err_and(|error|error.code=="model_batch_response")
                && generation.batch_size!=latest.batch_size)
        {
            active.state = Set("pending".into());
            active.run_after = Set(now());
            active.target_revision = Set(head.current_revision);
            active.update(&tx).await?;
            tx.commit().await?;
            return Ok(());
        }
        if !allowed && head.forgotten_at.is_none() {
            active.state = Set("skipped".into());
            active.update(&tx).await?;
            tx.commit().await?;
            return Ok(());
        }
        let id = uuid_from_bytes(&generation.id)?;
        let result = match vector {
            Ok(Some(vector)) => {
                self.search
                    .upsert_vector(space, id, memory, revision, project.as_deref(), vector)
                    .await
            }
            Ok(None) => self.search.delete_vector(space, id, memory).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                active.state = Set("done".into());
                active.indexed_revision = Set(Some(revision));
                active.last_error = Set(None);
            }
            Err(error) if error.code == "model_busy" => {
                active.state = Set("retry".into());
                active.run_after = Set(now() + time::Duration::seconds(1));
                active.last_error = Set(Some(error.code.to_owned()));
            }
            Err(error) => {
                let attempts = attempts.saturating_add(1);
                active.attempts = Set(attempts);
                match Recovery::after_failure(error.code, attempts) {
                    Recovery::Automatic(delay) => {
                        active.state = Set("retry".into());
                        active.run_after = Set(now() + delay);
                    }
                    Recovery::ConfigurationRequired => active.state = Set("failed".into()),
                }
                active.last_error = Set(Some(error.code.to_owned()));
            }
        }
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 一批只包含同空间、同代际和同连接修订；不等待凑满。
    pub(super) async fn run_embedding_batch(&self) -> Result<usize> {
        let Some(task) = Self::due_embedding_tasks().one(&self.db).await? else {
            return Ok(0);
        };
        let Some(first) = self.claim_embedding_task(task, None).await? else {
            return Ok(1);
        };
        if first.content.is_none() {
            self.finish_embedding_task(first, Ok(None)).await?;
            return Ok(1);
        }
        let connection = self.model_connection(&first.generation);
        let batch_size = connection
            .as_ref()
            .map_err(|error| Error {
                code: error.code,
                message: error.message,
                status: error.status,
            })
            .and_then(|connection| {
                crate::embedding_batch::BatchSize::resolve(
                    &connection.profile,
                    first.generation.batch_size,
                )
            });
        let batch_size = match batch_size {
            Ok(value) => value.value(),
            Err(error) => {
                self.finish_embedding_task(first, Err(error)).await?;
                return Ok(1);
            }
        };
        let candidates = Self::due_embedding_tasks()
            .filter(tasks::Column::GenerationId.eq(first.generation.id.clone()))
            .filter(tasks::Column::SpaceId.eq(first.space.bytes()))
            .limit(u64::from(batch_size.saturating_sub(1)))
            .all(&self.db)
            .await?;
        let mut claims = vec![first];
        let mut deleted = 0;
        for task in candidates {
            if let Some(claim) = self
                .claim_embedding_task(task, Some(claims[0].generation.connection_revision))
                .await?
            {
                if claim.content.is_none() {
                    self.finish_embedding_task(claim, Ok(None)).await?;
                    deleted += 1;
                } else {
                    claims.push(claim);
                }
            }
        }
        let count = claims.len() + deleted;
        let texts = claims
            .iter()
            .filter_map(|claim| claim.content.as_deref())
            .collect::<Vec<_>>();
        let vectors = match connection {
            Ok(connection) => {
                self.embed_model_batch(&connection, &texts, Duration::from_secs(15))
                    .await
            }
            Err(error) => Err(error),
        };
        // 一条发布失败不能让同批其他租约悬空；失败项仍由原有租约恢复。
        let mut first_error = None;
        match vectors {
            Ok(vectors) => {
                for (claim, vector) in claims.into_iter().zip(vectors) {
                    if let Err(error) = self.finish_embedding_task(claim, Ok(Some(vector))).await {
                        first_error.get_or_insert(error);
                    }
                }
            }
            Err(error) => {
                for claim in claims {
                    let error = Error {
                        code: error.code,
                        message: error.message,
                        status: error.status,
                    };
                    if let Err(error) = self.finish_embedding_task(claim, Err(error)).await {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(count)
    }
}
