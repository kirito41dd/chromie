mod agents;
mod capabilities;
mod embedding_batch_worker;
mod embedding_retry;
mod embedding_worker;
mod model_limits;
mod model_settings;
mod model_usage;
mod read;
mod recall;
mod recall_usage;
mod secret_audit;
mod secret_contracts;
mod secrets;
mod transfer;
mod transfer_import;
mod worker;
mod write;

use crate::{
    entity::{memories, memory_revisions, outbox_tasks, spaces, write_receipts},
    error::{Error, Result},
    search::Search,
    space_key::SpaceKey,
    types::{SpaceId, now, uuid_from_bytes},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, QueryFilter, QuerySelect, Set, TransactionTrait, sea_query::OnConflict,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    pub db: DatabaseConnection,
    pub search: Search,
    secret_cipher: Option<crate::secret_crypto::SecretCipher>,
    actor: Option<crate::agent::AgentAccess>,
    model_runtime: std::sync::Arc<crate::model_runtime::ModelRuntime>,
    embedding_backoff:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<Uuid, std::time::Instant>>>,
}

impl Store {
    pub fn new(db: DatabaseConnection, search: Search) -> Self {
        Self {
            db,
            search,
            secret_cipher: None,
            actor: None,
            model_runtime: Default::default(),
            embedding_backoff: Default::default(),
        }
    }

    // 同一空间的写入与索引发布共用数据库锁，避免旧任务在遗忘后再次发布。
    // MVP 接受空间内写操作串行；读取不持有这把锁，远程调用有明确超时。
    async fn begin(&self, space: SpaceId) -> Result<DatabaseTransaction> {
        let tx = self.db.begin().await?;
        spaces::Entity::find_by_id(space.bytes())
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, false).await?;
        }
        Ok(tx)
    }

    pub(crate) fn for_agent(&self, actor: crate::agent::AgentAccess) -> Self {
        let mut store = self.clone();
        store.actor = Some(actor);
        store
    }

    fn actor_bytes(&self) -> Option<Vec<u8>> {
        self.actor
            .as_ref()
            .map(|actor| actor.id.as_bytes().to_vec())
    }

    pub async fn create_space(
        &self,
        key: SpaceKey,
        name: String,
        password: String,
    ) -> Result<Uuid> {
        crate::types::bounded(&name, 100)?;
        let hash = crate::auth::hash_password(password).await?;
        let id = Uuid::now_v7();
        spaces::ActiveModel {
            id: Set(id.as_bytes().to_vec()),
            name: Set(name),
            slug: Set(key.as_str().to_owned()),
            password_hash: Set(hash),
            created_at: Set(now()),
        }
        .insert(&self.db)
        .await
        .map_err(space_write_error)?;
        Ok(id)
    }

    pub async fn resolve_space(&self, key: &SpaceKey) -> Result<SpaceId> {
        let row = spaces::Entity::find()
            .filter(spaces::Column::Slug.eq(key.as_str()))
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        Ok(SpaceId(uuid_from_bytes(&row.id)?))
    }

    pub async fn space_key(&self, space: SpaceId) -> Result<SpaceKey> {
        let row = spaces::Entity::find_by_id(space.bytes())
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        SpaceKey::parse(&row.slug).ok_or_else(Error::unavailable)
    }

    // 本机管理命令允许旧内部 ID 定位升级前的空间；网络入口只使用短标识。
    pub async fn rename_space(&self, current: &str, key: SpaceKey) -> Result<()> {
        let space = if let Ok(id) = Uuid::parse_str(current) {
            SpaceId(id)
        } else {
            let old = SpaceKey::parse(current).ok_or_else(|| Error::invalid("空间标识格式无效"))?;
            self.resolve_space(&old).await?
        };
        let tx = self.begin(space).await?;
        spaces::Entity::update_many()
            .col_expr(
                spaces::Column::Slug,
                sea_orm::sea_query::Expr::value(key.as_str()),
            )
            .filter(spaces::Column::Id.eq(space.bytes()))
            .exec(&tx)
            .await
            .map_err(space_write_error)?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_password(&self, space: SpaceId, password: String) -> Result<()> {
        let hash = crate::auth::hash_password(password).await?;
        let tx = self.begin(space).await?;
        spaces::Entity::update_many()
            .col_expr(
                spaces::Column::PasswordHash,
                sea_orm::sea_query::Expr::value(hash),
            )
            .filter(spaces::Column::Id.eq(space.bytes()))
            .exec(&tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // 仅供持有数据库配置的本地管理命令使用，不挂载到 HTTP 或 MCP。
    pub async fn admin_set_password(&self, space: Uuid, password: String) -> Result<()> {
        self.set_password(SpaceId(space), password).await
    }
    pub async fn admin_rebuild(&self, space: Uuid) -> Result<usize> {
        self.rebuild(SpaceId(space)).await
    }

    async fn head<C: ConnectionTrait>(db: &C, space: SpaceId, id: Uuid) -> Result<memories::Model> {
        memories::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(db)
            .await?
            .ok_or_else(Error::missing)
    }

    pub(crate) async fn revision<C: ConnectionTrait>(
        db: &C,
        head: &memories::Model,
    ) -> Result<memory_revisions::Model> {
        memory_revisions::Entity::find_by_id((
            head.space_id.clone(),
            head.id.clone(),
            head.current_revision,
        ))
        .one(db)
        .await?
        .ok_or_else(Error::missing)
    }

    async fn schedule<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        let current = now();
        outbox_tasks::Entity::insert(outbox_tasks::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            target_revision: Set(revision),
            state: Set("pending".into()),
            attempts: Set(0),
            run_after: Set(current),
            updated_at: Set(current),
        })
        .on_conflict(
            OnConflict::columns([
                outbox_tasks::Column::SpaceId,
                outbox_tasks::Column::MemoryId,
            ])
            .update_columns([
                outbox_tasks::Column::TargetRevision,
                outbox_tasks::Column::State,
                outbox_tasks::Column::Attempts,
                outbox_tasks::Column::RunAfter,
                outbox_tasks::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec(db)
        .await?;
        Self::schedule_embedding(db, space, id, revision).await?;
        Ok(())
    }

    fn hash<T: Serialize>(value: &T) -> Result<Vec<u8>> {
        let canonical = serde_json::to_vec(value).map_err(|_| Error::invalid("请求序列化失败"))?;
        Ok(Sha256::digest(canonical).to_vec())
    }

    async fn replay<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        operation: &str,
        request: Uuid,
        hash: &[u8],
    ) -> Result<Option<Value>> {
        let receipt = write_receipts::Entity::find_by_id((
            space.bytes(),
            operation.to_owned(),
            request.as_bytes().to_vec(),
        ))
        .one(db)
        .await?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        if receipt.input_hash != hash || receipt.agent_id != self.actor_bytes() {
            return Err(Error::conflict("idempotency_conflict"));
        }
        let target = uuid_from_bytes(&receipt.target_id)?;
        let hidden = if receipt.revision.is_some() {
            Self::head(db, space, target).await?.forgotten_at.is_some()
        } else {
            crate::entity::source_events::Entity::find_by_id((space.bytes(), receipt.target_id))
                .one(db)
                .await?
                .is_none_or(|event| event.forgotten_at.is_some())
        };
        Ok(Some(
            json!({"target_id":target,"memory_id":receipt.revision.map(|_|target),"revision":receipt.revision,"replayed":true,"hidden":hidden,"readable":!hidden}),
        ))
    }

    async fn receipt<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        operation: &str,
        request: Uuid,
        hash: Vec<u8>,
        target: (Uuid, Option<i32>),
    ) -> Result<()> {
        let (target, revision) = target;
        write_receipts::ActiveModel {
            space_id: Set(space.bytes()),
            operation: Set(operation.into()),
            request_id: Set(request.as_bytes().to_vec()),
            input_hash: Set(hash),
            target_id: Set(target.as_bytes().to_vec()),
            revision: Set(revision),
            created_at: Set(now()),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(db)
        .await?;
        Ok(())
    }
}

fn space_write_error(error: sea_orm::DbErr) -> Error {
    if matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        Error {
            code: "space_conflict",
            message: "空间标识或名称已被使用",
            status: axum::http::StatusCode::CONFLICT,
        }
    } else {
        error.into()
    }
}
