//! 模型限额独立于向量代际，同名模型的所有业务共享配置。
use super::*;
use crate::{
    embedding::{Connection, Save},
    entity::model_limits as limits,
    model_runtime::ModelLimit,
};
use sea_orm::IntoActiveModel;

impl Store {
    pub(super) async fn model_limit(&self, name: &str) -> Result<ModelLimit> {
        limits::Entity::find_by_id(Sha256::digest(name.as_bytes()).to_vec())
            .one(&self.db)
            .await?
            .map(|row| ModelLimit::new(row.max_concurrency, row.revision))
            .transpose()
            .map(|value| value.unwrap_or_default())
    }
    pub(super) async fn persist_model_limit<C: ConnectionTrait>(
        db: &C,
        name: &str,
        limit: ModelLimit,
    ) -> Result<()> {
        limits::Entity::insert(limits::ActiveModel {
            model_key: Set(Sha256::digest(name.as_bytes()).to_vec()),
            model_name: Set(name.into()),
            max_concurrency: Set(limit.max),
            revision: Set(limit.revision),
            updated_at: Set(now()),
        })
        .on_conflict(
            OnConflict::column(limits::Column::ModelKey)
                .update_columns([
                    limits::Column::MaxConcurrency,
                    limits::Column::Revision,
                    limits::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
        Ok(())
    }
    pub(super) async fn save_model_runtime_settings(
        &self,
        space: SpaceId,
        input: &Save,
        connection: &Connection,
        max_concurrency: u32,
        similarity_threshold: Option<crate::retrieval_policy::SimilarityThreshold>,
        batch_size: Option<crate::embedding_batch::BatchSize>,
    ) -> Result<Value> {
        let tx = self.db.begin().await?;
        let row = Self::model_settings_locked(&tx).await?;
        if row.revision != input.expected_revision || row.admin_space_id != Some(space.bytes()) {
            return Err(Error::conflict("model_settings_changed"));
        }
        let revision = row.revision.checked_add(1).ok_or_else(Error::unavailable)?;
        let limit = ModelLimit::new(max_concurrency, revision)?;
        let target = row
            .pending_generation
            .as_ref()
            .or(row.active_generation.as_ref())
            .ok_or_else(Error::unavailable)?;
        let previous = Self::generation(&tx, target).await?;
        let changed_batch = previous.batch_size != batch_size.map(|value| value.value());
        let mut generation = previous.into_active_model();
        generation.similarity_threshold =
            Set(similarity_threshold.map(|threshold| threshold.value()));
        generation.updated_at = Set(now());
        generation.batch_size = Set(batch_size.map(|value| value.value()));
        generation.update(&tx).await?;
        if changed_batch {
            use crate::entity::embedding_tasks as tasks;
            use sea_orm::{ColumnTrait, sea_query::Expr};
            tasks::Entity::update_many()
                .col_expr(tasks::Column::State, Expr::value("pending"))
                .col_expr(tasks::Column::Attempts, Expr::value(0))
                .col_expr(tasks::Column::RunAfter, Expr::value(now()))
                .filter(tasks::Column::GenerationId.eq(target.clone()))
                .filter(tasks::Column::State.eq("failed"))
                .filter(tasks::Column::LastError.eq("model_batch_response"))
                .exec(&tx)
                .await?;
        }
        Self::persist_model_limit(&tx, &connection.profile.model, limit).await?;
        let mut active = row.into_active_model();
        active.revision = Set(revision);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        self.model_runtime.apply(&connection.profile.model, limit);
        self.model_settings(space).await
    }
}
