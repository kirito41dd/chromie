use crate::{
    entity::{memories, memory_sources, outbox_tasks, secrets, source_events, spaces},
    error::{Error, Result},
    recall_usage::{MemoryOrder, MemoryUsage},
    store::Store,
    types::{SpaceId, now, timestamp, uuid_from_bytes},
};
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardQuery {
    #[serde(default)]
    pub order: MemoryOrder,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub order: MemoryOrder,
    pub space_id: String,
    pub space_name: String,
    pub updated_at: String,
    pub memory_count: u64,
    pub secret_count: u64,
    pub event_count: u64,
    pub last_capture_at: Option<String>,
    pub pending_count: u64,
    pub retrying_count: u64,
    pub last_index_at: Option<String>,
    pub index_available: bool,
    pub memories: Vec<RecentMemory>,
    pub can_manage_system: bool,
    pub embedding: serde_json::Value,
}

#[derive(Serialize)]
pub struct RecentMemory {
    pub usage: Option<MemoryUsage>,
    pub origin: serde_json::Value,
    pub kind: &'static str,
    pub secret_stored: Option<bool>,
    pub memory_id: String,
    pub revision: i32,
    pub preview: String,
    /// 仅为普通正文或 Secret 公开说明；展开卡片时显示，不读取 Secret 载荷。
    pub content: String,
    pub project_key: Option<String>,
    pub updated_at: String,
    pub source_count: u64,
    pub index_pending: bool,
}

impl Store {
    pub async fn dashboard(&self, space: SpaceId) -> Result<Snapshot> {
        self.dashboard_ordered(space, MemoryOrder::Recent).await
    }

    pub async fn dashboard_ordered(&self, space: SpaceId, order: MemoryOrder) -> Result<Snapshot> {
        // 同一只读快照内查询头部和修订，避免并发遗忘导致正文与状态不一致。
        let tx = self.db.begin().await?;
        let row = spaces::Entity::find_by_id(space.bytes())
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let memory_query = memories::Entity::find()
            .filter(memories::Column::SpaceId.eq(space.bytes()))
            .filter(memories::Column::ForgottenAt.is_null());
        let memory_count = memory_query.clone().count(&tx).await?;
        let secret_count = secrets::Entity::find()
            .inner_join(memories::Entity)
            .filter(secrets::Column::SpaceId.eq(space.bytes()))
            .filter(secrets::Column::Ciphertext.is_not_null())
            .filter(memories::Column::ForgottenAt.is_null())
            .count(&tx)
            .await?;
        let event_query = source_events::Entity::find()
            .filter(source_events::Column::SpaceId.eq(space.bytes()))
            .filter(source_events::Column::ForgottenAt.is_null());
        let event_count = event_query.clone().count(&tx).await?;
        let last_capture = event_query
            .order_by_desc(source_events::Column::ReceivedAt)
            .one(&tx)
            .await?;
        let pending = outbox_tasks::Entity::find()
            .filter(outbox_tasks::Column::SpaceId.eq(space.bytes()))
            .filter(outbox_tasks::Column::State.eq("pending"));
        let pending_count = pending.clone().count(&tx).await?;
        let retrying_count = pending
            .filter(outbox_tasks::Column::Attempts.gt(0))
            .count(&tx)
            .await?;
        let last_index = outbox_tasks::Entity::find()
            .filter(outbox_tasks::Column::SpaceId.eq(space.bytes()))
            .filter(outbox_tasks::Column::State.eq("done"))
            .order_by_desc(outbox_tasks::Column::UpdatedAt)
            .one(&tx)
            .await?;
        let heads = if order == MemoryOrder::Recent {
            memory_query
                .order_by_desc(memories::Column::UpdatedAt)
                .order_by_asc(memories::Column::Id)
                .limit(10)
                .all(&tx)
                .await?
        } else {
            let ids = Self::ranked_memories(&tx, space, order).await?;
            let mut rows: HashMap<_, _> = memory_query
                .filter(memories::Column::Id.is_in(ids.iter().map(|id| id.as_bytes().to_vec())))
                .all(&tx)
                .await?
                .into_iter()
                .map(|row| (row.id.clone(), row))
                .collect();
            ids.into_iter()
                .filter_map(|id| rows.remove(id.as_bytes().as_slice()))
                .collect()
        };
        let versions = heads
            .iter()
            .map(|head| Ok((uuid_from_bytes(&head.id)?, head.current_revision)))
            .collect::<Result<Vec<_>>>()?;
        let mut usage = Self::memory_usage(&tx, space, &versions).await.ok();
        let mut recent = Vec::new();
        for head in heads {
            let revision = Self::revision(&tx, &head).await?;
            let sources = memory_sources::Entity::find()
                .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                .filter(memory_sources::Column::MemoryId.eq(head.id.clone()))
                .filter(memory_sources::Column::Revision.eq(head.current_revision))
                .count(&tx)
                .await?;
            let task = outbox_tasks::Entity::find_by_id((space.bytes(), head.id.clone()))
                .one(&tx)
                .await?;
            let id = uuid_from_bytes(&head.id)?;
            let secret_stored = Self::secret_storage(&tx, space, id).await?;
            let content = revision.content.ok_or_else(Error::missing)?;
            recent.push(RecentMemory {
                usage: usage.as_mut().and_then(|usage| usage.remove(&id)),
                origin: Self::memory_origin(&tx, space, id, revision.agent_id.as_deref()).await?,
                kind: if secret_stored.is_some() {
                    "secret_ref"
                } else {
                    "memory"
                },
                secret_stored,
                memory_id: id.to_string(),
                revision: head.current_revision,
                preview: content.chars().take(360).collect(),
                content,
                project_key: revision.project_key,
                updated_at: timestamp(head.updated_at)?,
                source_count: sources,
                index_pending: task.is_none_or(|task| task.state != "done"),
            });
        }
        tx.commit().await?;
        Ok(Snapshot {
            order,
            space_id: row.slug,
            space_name: row.name,
            updated_at: timestamp(now())?,
            memory_count,
            secret_count,
            event_count,
            last_capture_at: last_capture
                .map(|event| timestamp(event.received_at))
                .transpose()?,
            pending_count,
            retrying_count,
            last_index_at: last_index
                .map(|task| timestamp(task.updated_at))
                .transpose()?,
            index_available: self.search.available().await,
            memories: recent,
            can_manage_system: self.can_manage_system(space).await?,
            embedding: self.embedding_status(space).await?,
        })
    }
}
