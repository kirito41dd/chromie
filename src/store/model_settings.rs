//! 实例配置的短事务协调重建、增量登记与切换；不能持锁调用模型。
use super::*;
use crate::{
    embedding::{Change, Connection, Save, validate_url},
    entity::{
        embedding_generations as generations, embedding_tasks as tasks,
        instance_model_settings as settings,
    },
};
use p256::elliptic_curve::zeroize::Zeroizing;
use sea_orm::{
    Condition, ExprTrait, IntoActiveModel, PaginatorTrait, QueryOrder,
    sea_query::{Expr, Query},
};
use std::time::Duration;

struct PreparedSettings {
    current: settings::Model,
    target: Option<generations::Model>,
    connection: Connection,
    max_concurrency: u32,
    similarity_threshold: Option<crate::retrieval_policy::SimilarityThreshold>,
    batch_size: Option<crate::embedding_batch::BatchSize>,
}

impl Store {
    pub async fn initialize_model_admin(&self, owner: Option<SpaceId>) -> Result<()> {
        let tx = self.db.begin().await?;
        if let Some(owner) = owner {
            spaces::Entity::find_by_id(owner.bytes())
                .lock_exclusive()
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
        }
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            admin_space_id: Set(None),
            enabled: Set(0),
            active_generation: Set(None),
            pending_generation: Set(None),
            revision: Set(0),
            updated_at: Set(now()),
        })
        .on_conflict_do_nothing_on([settings::Column::Id])
        .exec(&tx)
        .await?;
        let row = Self::model_settings_locked(&tx).await?;
        if let Some(owner) = owner {
            if row
                .admin_space_id
                .as_ref()
                .is_some_and(|id| *id != owner.bytes())
            {
                return Err(Error::invalid("系统管理空间已经绑定，不能由初始化覆盖"));
            }
            spaces::Entity::find_by_id(owner.bytes())
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
            if row.admin_space_id.is_none() {
                let mut active = row.into_active_model();
                active.admin_space_id = Set(Some(owner.bytes()));
                active.update(&tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn model_settings_locked<C: ConnectionTrait>(
        db: &C,
    ) -> Result<settings::Model> {
        settings::Entity::find_by_id(1u8)
            .lock_exclusive()
            .one(db)
            .await?
            .ok_or_else(Error::unavailable)
    }

    pub async fn can_manage_system(&self, space: SpaceId) -> Result<bool> {
        Ok(self.actor.is_none()
            && settings::Entity::find_by_id(1u8)
                .one(&self.db)
                .await?
                .is_some_and(|row| row.admin_space_id == Some(space.bytes())))
    }

    pub(super) async fn check_model_admin(&self, space: SpaceId) -> Result<()> {
        if !self.can_manage_system(space).await? {
            return Err(Error {
                code: "system_admin_required",
                message: "请登录初始管理空间后修改系统设置",
                status: reqwest::StatusCode::FORBIDDEN,
            });
        }
        Ok(())
    }

    pub(super) async fn generation<C: ConnectionTrait>(
        db: &C,
        id: &[u8],
    ) -> Result<generations::Model> {
        generations::Entity::find_by_id(id.to_vec())
            .one(db)
            .await?
            .ok_or_else(Error::unavailable)
    }

    pub(super) fn model_connection(&self, row: &generations::Model) -> Result<Connection> {
        let key = match (&row.key_cipher, &row.key_nonce, &row.key_id) {
            (Some(bytes), Some(nonce), Some(key_id)) => {
                let plain = self.cipher()?.open_model(
                    uuid_from_bytes(&row.id)?,
                    crate::secret_crypto::EncryptedPayload {
                        revision: row.connection_revision,
                        format: 1,
                        key_id,
                        nonce,
                        ciphertext: bytes,
                    },
                )?;
                Some(Zeroizing::new(
                    String::from_utf8(plain.to_vec()).map_err(|_| Error::unavailable())?,
                ))
            }
            (None, None, None) => None,
            _ => return Err(Error::unavailable()),
        };
        Ok(Connection {
            base_url: row.base_url.clone(),
            profile: serde_json::from_value(row.profile.clone())
                .map_err(|_| Error::unavailable())?,
            key,
        })
    }

    pub async fn model_settings(&self, space: SpaceId) -> Result<Value> {
        self.check_model_admin(space).await?;
        let row = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .ok_or_else(Error::unavailable)?;
        let mut result =
            json!({"revision":row.revision,"enabled":row.enabled!=0,"active":null,"pending":null});
        for (name, id) in [
            ("active", row.active_generation),
            ("pending", row.pending_generation),
        ] {
            if let Some(id) = id {
                let generation = Self::generation(&self.db, &id).await?;
                result[name] = self.generation_view(&generation, None).await?;
            }
        }
        Ok(result)
    }

    async fn generation_view(
        &self,
        row: &generations::Model,
        space: Option<SpaceId>,
    ) -> Result<Value> {
        let profile: crate::embedding::Profile =
            serde_json::from_value(row.profile.clone()).map_err(|_| Error::unavailable())?;
        let concurrency = self.model_limit(&profile.model).await?.max;
        let threshold = crate::retrieval_policy::resolve(&profile, row.similarity_threshold)?;
        let recommended = crate::retrieval_policy::recommended(&profile);
        let batch_size = crate::embedding_batch::BatchSize::resolve(&profile, row.batch_size)?;
        Ok(
            json!({"generation_id":uuid_from_bytes(&row.id)?,"profile":row.profile,"base_url":row.base_url,
            "has_api_key":row.key_cipher.is_some(),"max_concurrency":concurrency,
            "similarity_threshold":threshold,"similarity_threshold_override":row.similarity_threshold,"recommended_similarity_threshold":recommended,
            "batch_size":batch_size,"batch_size_override":row.batch_size,
            "state":row.state,"progress":Self::embedding_progress(&self.db,&row.id,space).await?,"scan_complete":row.scan_complete!=0}),
        )
    }

    pub async fn embedding_status(&self, space: SpaceId) -> Result<Value> {
        let Some(row) = settings::Entity::find_by_id(1u8).one(&self.db).await? else {
            return Ok(json!({"state":"disabled"}));
        };
        if row.enabled == 0 {
            return Ok(json!({"state":"disabled"}));
        }
        let id = row
            .pending_generation
            .as_ref()
            .or(row.active_generation.as_ref());
        let Some(id) = id else {
            return Ok(json!({"state":"disabled"}));
        };
        let generation = Self::generation(&self.db, id).await?;
        let progress = Self::embedding_progress(&self.db, id, Some(space)).await?;
        let failed = progress["failed"].as_u64().unwrap_or(0) > 0;
        let backed_off = self
            .embedding_backoff
            .lock()
            .await
            .get(&uuid_from_bytes(id)?)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30));
        let state = if row.pending_generation.is_some() {
            "building"
        } else if failed || backed_off {
            "unavailable"
        } else if progress["ready"] != progress["total"] {
            "updating"
        } else {
            "ready"
        };
        Ok(json!({"state":state,"model":generation.profile.get("model"),"progress":progress}))
    }

    /// 使用当前头部与完成修订相关联，历史任务、重试次数不构成覆盖数量。
    pub(super) async fn embedding_progress<C: ConnectionTrait>(
        db: &C,
        generation: &[u8],
        space: Option<SpaceId>,
    ) -> Result<Value> {
        let mut heads = memories::Entity::find().filter(memories::Column::ForgottenAt.is_null());
        if let Some(space) = space {
            heads = heads.filter(memories::Column::SpaceId.eq(space.bytes()));
        }
        let ready = Query::select()
            .expr(Expr::val(1))
            .from(tasks::Entity)
            .and_where(
                Expr::col((tasks::Entity, tasks::Column::GenerationId)).eq(generation.to_vec()),
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
        let total = heads.clone().count(db).await?;
        let completed = heads.filter(Expr::exists(ready)).count(db).await?;
        let live = Query::select()
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
            .and_where(
                Expr::col((memories::Entity, memories::Column::CurrentRevision))
                    .equals((tasks::Entity, tasks::Column::TargetRevision)),
            )
            .and_where(Expr::col((memories::Entity, memories::Column::ForgottenAt)).is_null())
            .to_owned();
        let mut jobs = tasks::Entity::find()
            .filter(tasks::Column::GenerationId.eq(generation.to_vec()))
            .filter(Expr::exists(live));
        if let Some(space) = space {
            jobs = jobs.filter(tasks::Column::SpaceId.eq(space.bytes()));
        }
        let mut result = json!({"total":total,"ready":completed});
        let automatic = Condition::any().add(tasks::Column::State.eq("retry")).add(
            Condition::all().add(tasks::Column::State.eq("failed")).add(
                tasks::Column::LastError
                    .is_in(super::embedding_retry::RETRYABLE_ERRORS.iter().copied()),
            ),
        );
        for state in ["pending", "running", "retry", "failed"] {
            let condition =
                match state {
                    "retry" => automatic.clone(),
                    "failed" => Condition::all().add(tasks::Column::State.eq("failed")).add(
                        Condition::any()
                            .add(tasks::Column::LastError.is_null())
                            .add(tasks::Column::LastError.is_not_in(
                                super::embedding_retry::RETRYABLE_ERRORS.iter().copied(),
                            )),
                    ),
                    _ => Condition::all().add(tasks::Column::State.eq(state)),
                };
            result[state] = json!(jobs.clone().filter(condition).count(db).await?);
        }
        let next_retry = jobs
            .clone()
            .filter(automatic)
            .order_by_asc(tasks::Column::RunAfter)
            .one(db)
            .await?;
        result["next_retry_at"] = json!(next_retry.map(|row| row.run_after.to_string()));
        let error = jobs
            .clone()
            .filter(tasks::Column::LastError.is_not_null())
            .order_by_desc(tasks::Column::UpdatedAt)
            .one(db)
            .await?;
        let last = jobs.order_by_desc(tasks::Column::UpdatedAt).one(db).await?;
        result["last_updated_at"] = json!(last.as_ref().map(|row| row.updated_at.to_string()));
        result["last_error"] = json!(error.and_then(|row| row.last_error));
        Ok(result)
    }

    async fn prepare_model_settings(
        &self,
        space: SpaceId,
        input: &Save,
    ) -> Result<PreparedSettings> {
        self.check_model_admin(space).await?;
        let base_url = validate_url(&input.base_url)?;
        let profile = input.profile();
        profile.validate()?;
        let max_concurrency = match input.max_concurrency {
            Some(value) => value,
            None => self.model_limit(&profile.model).await?.max,
        };
        crate::model_runtime::ModelLimit::new(max_concurrency, 0)?;
        if input
            .api_key
            .as_ref()
            .is_some_and(|key| key.len() > 8192 || key.chars().any(char::is_control))
        {
            return Err(Error::invalid("API Key 格式无效"));
        }
        let before = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .ok_or_else(Error::unavailable)?;
        if before.revision != input.expected_revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        let target = match before
            .pending_generation
            .as_ref()
            .or(before.active_generation.as_ref())
        {
            Some(id) => Some(Self::generation(&self.db, id).await?),
            None => None,
        };
        let key = if input.clear_api_key {
            None
        } else if let Some(value) = input.api_key.as_ref().filter(|value| !value.is_empty()) {
            Some(Zeroizing::new(value.clone()))
        } else {
            target
                .as_ref()
                .map(|row| self.model_connection(row))
                .transpose()?
                .and_then(|connection| connection.key)
        };
        // 省略字段兼容旧客户端；新模型不继承旧模型的自定义阈值。
        let profile_fingerprint = profile.fingerprint()?;
        let similarity_threshold = match input.similarity_threshold {
            Some(value) => value,
            None => target
                .as_ref()
                .filter(|row| row.fingerprint == profile_fingerprint)
                .and_then(|row| row.similarity_threshold)
                .map(crate::retrieval_policy::SimilarityThreshold::new)
                .transpose()?,
        };
        let batch_size = match input.batch_size {
            Some(value) => value,
            None => target
                .as_ref()
                .filter(|row| row.fingerprint == profile_fingerprint)
                .and_then(|row| row.batch_size)
                .map(crate::embedding_batch::BatchSize::new)
                .transpose()?,
        };
        Ok(PreparedSettings {
            current: before,
            target,
            max_concurrency,
            similarity_threshold,
            batch_size,
            connection: Connection {
                base_url,
                profile,
                key,
            },
        })
    }

    /// 复用保存时的校验与 Key 选择，只调用固定文本，不改配置、代际或任务。
    pub async fn test_model_connection(&self, space: SpaceId, input: Save) -> Result<Value> {
        let prepared = self.prepare_model_settings(space, &input).await?;
        let batch = crate::embedding_batch::BatchSize::resolve(
            &prepared.connection.profile,
            prepared.batch_size.map(|value| value.value()),
        )?;
        self.probe_model_connection(&prepared.connection, batch)
            .await
    }

    pub async fn save_model_settings(&self, space: SpaceId, input: Save) -> Result<Value> {
        let PreparedSettings {
            current: before,
            target,
            connection,
            max_concurrency,
            similarity_threshold,
            batch_size,
        } = self.prepare_model_settings(space, &input).await?;
        let fingerprint = connection.profile.fingerprint()?;
        let rebuild = input.force_rebuild
            || target
                .as_ref()
                .is_none_or(|row| row.fingerprint != fingerprint)
            || before.enabled == 0;
        if rebuild && target.is_some() && !input.confirm_rebuild {
            return Err(Error::conflict("model_rebuild_confirmation_required"));
        }
        let model_name = connection.profile.model.clone();
        if !rebuild
            && target
                .as_ref()
                .is_some_and(|row| row.base_url == connection.base_url)
            && input.api_key.as_ref().is_none_or(|key| key.is_empty())
            && !input.clear_api_key
            && (self.model_limit(&model_name).await?.max != max_concurrency
                || target.as_ref().and_then(|row| row.similarity_threshold)
                    != similarity_threshold.map(|threshold| threshold.value())
                || target.as_ref().and_then(|row| row.batch_size)
                    != batch_size.map(|value| value.value()))
        {
            return self
                .save_model_runtime_settings(
                    space,
                    &input,
                    &connection,
                    max_concurrency,
                    similarity_threshold,
                    batch_size,
                )
                .await;
        }
        self.probe_model_connection(
            &connection,
            crate::embedding_batch::BatchSize::resolve(
                &connection.profile,
                batch_size.map(|value| value.value()),
            )?,
        )
        .await?;
        let Connection {
            base_url,
            profile,
            key,
        } = connection;
        let tx = self.db.begin().await?;
        let row = Self::model_settings_locked(&tx).await?;
        if row.revision != input.expected_revision || row.admin_space_id != Some(space.bytes()) {
            return Err(Error::conflict("model_settings_changed"));
        }
        let id = if rebuild {
            Uuid::now_v7()
        } else {
            uuid_from_bytes(&target.as_ref().ok_or_else(Error::unavailable)?.id)?
        };
        let connection_revision = if rebuild {
            1
        } else {
            target
                .as_ref()
                .ok_or_else(Error::unavailable)?
                .connection_revision
                .checked_add(1)
                .ok_or_else(Error::unavailable)?
        };
        let envelope = key
            .as_ref()
            .map(|key| {
                self.cipher()?
                    .seal_model(id, connection_revision, key.as_bytes())
            })
            .transpose()?;
        let mut generation = if rebuild {
            if let Some(old) = row.pending_generation.as_ref() {
                Self::retire_generation(&tx, old, "cancelled").await?;
            }
            generations::ActiveModel {
                id: Set(id.as_bytes().to_vec()),
                profile: Set(json!(profile)),
                fingerprint: Set(fingerprint),
                state: Set("building".into()),
                cursor_space: Set(None),
                cursor_memory: Set(None),
                scan_complete: Set(0),
                created_at: Set(now()),
                retired_at: Set(None),
                ..Default::default()
            }
        } else {
            target.ok_or_else(Error::unavailable)?.into_active_model()
        };
        generation.base_url = Set(base_url);
        generation.connection_revision = Set(connection_revision);
        generation.key_id = Set(envelope
            .as_ref()
            .map(|_| self.cipher().map(|cipher| cipher.key_id().to_owned()))
            .transpose()?);
        generation.key_nonce = Set(envelope.as_ref().map(|e| e.nonce.clone()));
        generation.key_cipher = Set(envelope.map(|e| e.ciphertext));
        generation.updated_at = Set(now());
        generation.similarity_threshold =
            Set(similarity_threshold.map(|threshold| threshold.value()));
        generation.batch_size = Set(batch_size.map(|value| value.value()));
        if rebuild {
            generation.insert(&tx).await?;
        } else {
            generation.update(&tx).await?;
        }
        if !rebuild {
            tasks::Entity::update_many()
                .col_expr(tasks::Column::State, Expr::value("pending"))
                .col_expr(tasks::Column::Attempts, Expr::value(0))
                .col_expr(tasks::Column::RunAfter, Expr::value(now()))
                .filter(tasks::Column::GenerationId.eq(id.as_bytes().to_vec()))
                .filter(tasks::Column::State.is_in(["failed", "retry"]))
                .exec(&tx)
                .await?;
        }
        let was_disabled = row.enabled == 0;
        let mut active = row.into_active_model();
        active.enabled = Set(1);
        if was_disabled {
            active.active_generation = Set(None);
        }
        if rebuild {
            active.pending_generation = Set(Some(id.as_bytes().to_vec()));
        }
        active.revision = Set(input
            .expected_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?);
        active.updated_at = Set(now());
        let limit = crate::model_runtime::ModelLimit::new(
            max_concurrency,
            input
                .expected_revision
                .checked_add(1)
                .ok_or_else(Error::unavailable)?,
        )?;
        Self::persist_model_limit(&tx, &model_name, limit).await?;
        active.update(&tx).await?;
        tx.commit().await?;
        self.model_runtime.apply(&model_name, limit);
        self.embedding_backoff.lock().await.clear();
        self.model_settings(space).await
    }

    pub(super) async fn retire_generation<C: ConnectionTrait>(
        db: &C,
        id: &[u8],
        state: &str,
    ) -> Result<()> {
        let mut active = Self::generation(db, id).await?.into_active_model();
        active.state = Set(state.to_owned());
        active.retired_at = Set(Some(now()));
        active.updated_at = Set(now());
        active.update(db).await?;
        Ok(())
    }

    pub async fn disable_embedding(&self, space: SpaceId, input: Change) -> Result<Value> {
        self.check_model_admin(space).await?;
        let tx = self.db.begin().await?;
        let row = Self::model_settings_locked(&tx).await?;
        if row.revision != input.expected_revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        for id in [&row.active_generation, &row.pending_generation]
            .into_iter()
            .flatten()
        {
            Self::retire_generation(&tx, id, "retired").await?;
        }
        // 保留最后的配置用于再次编辑；enabled 为 false 时绝不使用它查询或生成。
        let kept = row
            .pending_generation
            .clone()
            .or(row.active_generation.clone());
        let mut active = row.into_active_model();
        active.enabled = Set(0);
        active.active_generation = Set(kept);
        active.pending_generation = Set(None);
        active.revision = Set(input
            .expected_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        self.model_settings(space).await
    }

    pub async fn retry_embedding(&self, space: SpaceId, input: Change) -> Result<Value> {
        self.check_model_admin(space).await?;
        let tx = self.db.begin().await?;
        let row = Self::model_settings_locked(&tx).await?;
        if row.revision != input.expected_revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        for id in [row.active_generation, row.pending_generation]
            .into_iter()
            .flatten()
        {
            tasks::Entity::update_many()
                .col_expr(tasks::Column::State, Expr::value("pending"))
                .col_expr(tasks::Column::Attempts, Expr::value(0))
                .col_expr(tasks::Column::RunAfter, Expr::value(now()))
                .filter(tasks::Column::GenerationId.eq(id))
                .filter(tasks::Column::State.is_in(["failed", "retry"]))
                .exec(&tx)
                .await?;
        }
        tx.commit().await?;
        self.embedding_backoff.lock().await.clear();
        self.model_settings(space).await
    }
}
