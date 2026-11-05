//! 持久游标、租约和代际屏障。模型在事务外计算，发布与遗忘共用空间锁。
use super::*;
use crate::entity::{
    embedding_generations as generations, embedding_tasks as tasks,
    instance_model_settings as settings,
};
use sea_orm::{Condition, IntoActiveModel, QueryOrder};
use std::time::{Duration, Instant};

/// 查询向量仅在本次召回内复用，不能序列化或作为模型调用的另一份输入。
pub(super) struct SemanticFilter {
    pub generation: Uuid,
    pub vector: Vec<f32>,
    pub threshold: crate::retrieval_policy::SimilarityThreshold,
}

impl Store {
    pub(super) async fn schedule_embedding<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        let Some(row) = settings::Entity::find_by_id(1u8)
            .lock_exclusive()
            .one(db)
            .await?
        else {
            return Ok(());
        };
        let head = Self::head(db, space, id).await?;
        let ids = if head.forgotten_at.is_some() {
            generations::Entity::find()
                .filter(generations::Column::State.ne("cleaned"))
                .all(db)
                .await?
                .into_iter()
                .map(|g| g.id)
                .collect::<Vec<_>>()
        } else if row.enabled != 0 {
            [row.active_generation, row.pending_generation]
                .into_iter()
                .flatten()
                .collect()
        } else {
            vec![]
        };
        for generation in ids {
            Self::enqueue_vector(db, generation, space, id, revision).await?;
        }
        Ok(())
    }

    async fn enqueue_vector<C: ConnectionTrait>(
        db: &C,
        generation: Vec<u8>,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        tasks::Entity::insert(tasks::ActiveModel {
            generation_id: Set(generation),
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            target_revision: Set(revision),
            indexed_revision: Set(None),
            state: Set("pending".into()),
            attempts: Set(0),
            run_after: Set(now()),
            lease_token: Set(None),
            lease_until: Set(None),
            last_error: Set(None),
            updated_at: Set(now()),
        })
        .on_conflict(
            OnConflict::columns([
                tasks::Column::GenerationId,
                tasks::Column::SpaceId,
                tasks::Column::MemoryId,
            ])
            .update_columns([
                tasks::Column::TargetRevision,
                tasks::Column::State,
                tasks::Column::Attempts,
                tasks::Column::RunAfter,
                tasks::Column::LeaseToken,
                tasks::Column::LeaseUntil,
                tasks::Column::LastError,
                tasks::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec(db)
        .await?;
        Ok(())
    }

    async fn scan_embedding_page(&self) -> Result<()> {
        let Some(state) = settings::Entity::find_by_id(1u8).one(&self.db).await? else {
            return Ok(());
        };
        if state.enabled == 0 {
            return Ok(());
        }
        let Some(id) = state.pending_generation else {
            return Ok(());
        };
        let generation = Self::generation(&self.db, &id).await?;
        if generation.scan_complete != 0 {
            return Ok(());
        }
        let mut query = memories::Entity::find().filter(memories::Column::ForgottenAt.is_null());
        if let (Some(space), Some(memory)) = (&generation.cursor_space, &generation.cursor_memory) {
            query = query.filter(
                Condition::any()
                    .add(memories::Column::SpaceId.gt(space.clone()))
                    .add(
                        Condition::all()
                            .add(memories::Column::SpaceId.eq(space.clone()))
                            .add(memories::Column::Id.gt(memory.clone())),
                    ),
            );
        }
        let rows = query
            .order_by_asc(memories::Column::SpaceId)
            .order_by_asc(memories::Column::Id)
            .limit(64)
            .all(&self.db)
            .await?;
        for row in &rows {
            let space = SpaceId(uuid_from_bytes(&row.space_id)?);
            let memory = uuid_from_bytes(&row.id)?;
            let tx = self.begin(space).await?;
            let state = Self::model_settings_locked(&tx).await?;
            if state.pending_generation.as_ref() != Some(&id) || state.enabled == 0 {
                return Ok(());
            }
            let head = Self::head(&tx, space, memory).await?;
            if head.forgotten_at.is_none() {
                let task = tasks::Entity::find_by_id((id.clone(), space.bytes(), row.id.clone()))
                    .one(&tx)
                    .await?;
                if task.is_none_or(|task| task.target_revision != head.current_revision) {
                    Self::enqueue_vector(&tx, id.clone(), space, memory, head.current_revision)
                        .await?;
                }
            }
            tx.commit().await?;
        }
        let tx = self.db.begin().await?;
        let state = Self::model_settings_locked(&tx).await?;
        if state.pending_generation.as_ref() != Some(&id) || state.enabled == 0 {
            return Ok(());
        }
        let current = Self::generation(&tx, &id).await?;
        // 并发扫描只允许从相同游标前进，不能把已推进的游标写回去。
        if current.cursor_space != generation.cursor_space
            || current.cursor_memory != generation.cursor_memory
        {
            return Ok(());
        }
        let mut active = current.into_active_model();
        if let Some(last) = rows.last() {
            active.cursor_space = Set(Some(last.space_id.clone()));
            active.cursor_memory = Set(Some(last.id.clone()));
        }
        if rows.len() < 64 {
            active.scan_complete = Set(1);
        }
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn activate_embedding_if_ready(&self) -> Result<()> {
        let tx = self.db.begin().await?;
        let row = Self::model_settings_locked(&tx).await?;
        if row.enabled == 0 {
            return Ok(());
        }
        let Some(id) = row.pending_generation.as_ref() else {
            return Ok(());
        };
        let generation = Self::generation(&tx, id).await?;
        if generation.scan_complete == 0 || generation.state != "building" {
            return Ok(());
        }
        let progress = Self::embedding_progress(&tx, id, None).await?;
        if progress["ready"] != progress["total"] {
            return Ok(());
        }
        if let Some(old) = row.active_generation.as_ref() {
            Self::retire_generation(&tx, old, "retired").await?;
        }
        let mut active = generation.into_active_model();
        active.state = Set("active".into());
        active.updated_at = Set(now());
        active.update(&tx).await?;
        let next_revision = row.revision.checked_add(1).ok_or_else(Error::unavailable)?;
        let id = id.clone();
        let mut active = row.into_active_model();
        active.active_generation = Set(Some(id));
        active.pending_generation = Set(None);
        active.revision = Set(next_revision);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn clean_retired_embedding(&self) -> Result<()> {
        let candidate = generations::Entity::find()
            .filter(generations::Column::State.is_in(["retired", "cancelled", "cleaning"]))
            .filter(generations::Column::RetiredAt.lt(now() - time::Duration::hours(1)))
            .one(&self.db)
            .await?;
        let Some(generation) = candidate else {
            return Ok(());
        };
        let tx = self.db.begin().await?;
        let state = Self::model_settings_locked(&tx).await?;
        if [
            state.active_generation.as_ref(),
            state.pending_generation.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|id| *id == generation.id)
        {
            return Ok(());
        }
        let mut cleaning = Self::generation(&tx, &generation.id)
            .await?
            .into_active_model();
        cleaning.state = Set("cleaning".into());
        cleaning.updated_at = Set(now());
        cleaning.update(&tx).await?;
        tx.commit().await?;
        let id = uuid_from_bytes(&generation.id)?;
        for space in spaces::Entity::find().all(&self.db).await? {
            self.search
                .delete_vector_generation(SpaceId(uuid_from_bytes(&space.id)?), id)
                .await?;
        }
        let tx = self.db.begin().await?;
        let _state = Self::model_settings_locked(&tx).await?;
        tasks::Entity::delete_many()
            .filter(tasks::Column::GenerationId.eq(generation.id.clone()))
            .exec(&tx)
            .await?;
        let mut active = Self::generation(&tx, &generation.id)
            .await?
            .into_active_model();
        active.state = Set("cleaned".into());
        active.key_cipher = Set(None);
        active.key_nonce = Set(None);
        active.key_id = Set(None);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn sync_embedding_due(&self) -> Result<usize> {
        if settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .is_none()
        {
            return Ok(0);
        }
        self.scan_embedding_page().await?;
        let mut done = 0;
        for _ in 0..4 {
            let processed = self.run_embedding_batch().await?;
            if processed == 0 {
                break;
            }
            done += processed;
        }
        self.activate_embedding_if_ready().await?;
        self.clean_retired_embedding().await?;
        Ok(done)
    }

    pub(super) async fn embedding_candidates(
        &self,
        space: SpaceId,
        query: &str,
        project: Option<&str>,
    ) -> Result<(Vec<crate::search::SearchHit>, Value, Option<SemanticFilter>)> {
        let started = Instant::now();
        let Some(row) = settings::Entity::find_by_id(1u8).one(&self.db).await? else {
            return Ok((
                vec![],
                json!({"state":"disabled","reason":"not_configured","config_revision":0}),
                None,
            ));
        };
        if row.enabled == 0 {
            let reason = if row.active_generation.is_none() && row.pending_generation.is_none() {
                "not_configured"
            } else {
                "disabled"
            };
            return Ok((
                vec![],
                json!({"state":"disabled","reason":reason,"config_revision":row.revision}),
                None,
            ));
        }
        let Some(id) = row.active_generation else {
            return Ok((
                vec![],
                json!({"state":"building","reason":"initial_index_building","config_revision":row.revision}),
                None,
            ));
        };
        let generation = Self::generation(&self.db, &id).await?;
        let id = uuid_from_bytes(&id)?;
        if memories::Entity::find()
            .filter(memories::Column::SpaceId.eq(space.bytes()))
            .filter(memories::Column::ForgottenAt.is_null())
            .one(&self.db)
            .await?
            .is_none()
        {
            return Ok((
                vec![],
                json!({"state":"empty","reason":"empty_space","config_revision":row.revision,"generation_id":id,"model":generation.profile.get("model")}),
                None,
            ));
        }
        if self
            .embedding_backoff
            .lock()
            .await
            .get(&id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        {
            return Ok((
                vec![],
                json!({"state":"unavailable","reason":"retry_backoff","config_revision":row.revision}),
                None,
            ));
        }
        let result: Result<(Vec<crate::search::SearchHit>, Option<SemanticFilter>)> = async {
            let connection = self.model_connection(&generation)?;
            let vector = self
                .embed_model(&connection, query, Duration::from_secs(3))
                .await?;
            let threshold = crate::retrieval_policy::resolve(
                &connection.profile,
                generation.similarity_threshold,
            )?;
            let hits = self
                .search
                .query_vectors(space, id, vector.clone(), project)
                .await?;
            Ok((
                hits,
                threshold.enabled().then_some(SemanticFilter {
                    generation: id,
                    vector,
                    threshold,
                }),
            ))
        }
        .await;
        let mut debug = json!({"generation_id":id,"config_revision":row.revision,"model":generation.profile.get("model"),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0});
        match result {
            Ok((hits, filter)) => {
                debug["state"] = json!("ready");
                debug["candidates"] = json!(hits.len());
                debug["similarity_threshold"] =
                    json!(filter.as_ref().map(|f| f.threshold.value()).unwrap_or(0.0));
                Ok((hits, debug, filter))
            }
            Err(error) => {
                // 模型连接故障可共享退避；单个空间尚无向量索引不能拖累其他空间。
                if error.code.starts_with("model_") && error.code != "model_busy" {
                    self.embedding_backoff
                        .lock()
                        .await
                        .insert(id, Instant::now());
                }
                debug["state"] = json!("unavailable");
                debug["reason"] = json!(error.code);
                Ok((vec![], debug, None))
            }
        }
    }
}
