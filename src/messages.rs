//! 只读的原始采集浏览；所有查询显式绑定空间，不读取 Secret 数据。
use crate::{
    entity::source_events as events,
    error::{Error, Result},
    store::Store,
    types::{SpaceId, now, timestamp, uuid_from_bytes},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{OffsetDateTime, PrimitiveDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsQuery {
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub page: u64,
    pub as_of: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub agent_id: Option<Uuid>,
    pub session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesQuery {
    pub agent_id: Option<Uuid>,
    pub session_id: String,
    #[serde(default)]
    pub page: u64,
    pub as_of: Option<String>,
}

#[derive(Serialize)]
pub struct SessionRow {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub origin: Value,
    pub preview: String,
    pub message_count: i64,
    pub received_at: String,
}

#[derive(Serialize)]
pub struct MessageRow {
    pub event_id: Uuid,
    pub role: String,
    pub preview: String,
    pub truncated: bool,
    pub occurred_at: String,
    pub received_at: String,
    pub turn_id: Option<String>,
    pub project_key: Option<String>,
}

#[derive(Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: u64,
    pub has_more: bool,
    pub as_of: String,
}

fn boundary(page: u64, as_of: Option<&str>) -> Result<PrimitiveDateTime> {
    if page > 500 {
        return Err(Error::invalid("页码超过上限，请刷新后重新浏览"));
    }
    let Some(value) = as_of else {
        return Ok(now());
    };
    if value.len() > 40 {
        return Err(Error::invalid("浏览时间边界无效"));
    }
    let value = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| Error::invalid("浏览时间边界无效"))?
        .to_offset(time::UtcOffset::UTC);
    let value = PrimitiveDateTime::new(value.date(), value.time());
    if value > now() + time::Duration::seconds(1) {
        return Err(Error::invalid("浏览时间边界不能在未来"));
    }
    Ok(value)
}

fn visible(space: SpaceId, at: PrimitiveDateTime) -> sea_orm::Select<events::Entity> {
    events::Entity::find()
        .filter(events::Column::SpaceId.eq(space.bytes()))
        .filter(events::Column::ForgottenAt.is_null())
        .filter(events::Column::ReceivedAt.lte(at))
}

fn in_conversation(
    query: sea_orm::Select<events::Entity>,
    key: &Conversation,
) -> sea_orm::Select<events::Entity> {
    let query = query.filter(events::Column::SessionId.eq(&key.session_id));
    match key.agent_id {
        Some(id) => query.filter(events::Column::AgentId.eq(id.as_bytes().to_vec())),
        None => query.filter(events::Column::AgentId.is_null()),
    }
}

impl Store {
    pub async fn message_sessions(
        &self,
        space: SpaceId,
        input: SessionsQuery,
    ) -> Result<Page<SessionRow>> {
        let at = boundary(input.page, input.as_of.as_deref())?;
        let tx = self.db.begin().await?;
        let mut query = visible(space, at);
        if let Some(id) = input.agent_id {
            query = query.filter(events::Column::AgentId.eq(id.as_bytes().to_vec()));
        }
        let mut groups = query
            .select_only()
            .column(events::Column::AgentId)
            .column(events::Column::SessionId)
            .column_as(events::Column::Id.count(), "message_count")
            .column_as(events::Column::ReceivedAt.max(), "latest_received")
            .group_by(events::Column::AgentId)
            .group_by(events::Column::SessionId)
            .order_by_desc(events::Column::ReceivedAt.max())
            .order_by_asc(events::Column::SessionId)
            .order_by_asc(events::Column::AgentId)
            .offset(input.page * 20)
            .limit(21)
            .into_tuple::<(Option<Vec<u8>>, String, i64, PrimitiveDateTime)>()
            .all(&tx)
            .await?;
        let has_more = groups.len() > 20;
        groups.truncate(20);
        let mut items = Vec::new();
        for (agent, session_id, message_count, received_at) in groups {
            let key = Conversation {
                agent_id: agent.as_deref().map(uuid_from_bytes).transpose()?,
                session_id,
            };
            let latest = in_conversation(visible(space, at), &key)
                .order_by_desc(events::Column::ReceivedAt)
                .order_by_desc(events::Column::Id)
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
            items.push(SessionRow {
                origin: Self::source_label(&tx, space, agent.as_deref()).await?,
                conversation: key,
                preview: latest.body.unwrap_or_default().chars().take(120).collect(),
                message_count,
                received_at: timestamp(received_at)?,
            });
        }
        tx.commit().await?;
        Ok(Page {
            items,
            page: input.page,
            has_more,
            as_of: timestamp(at)?,
        })
    }

    pub async fn conversation_messages(
        &self,
        space: SpaceId,
        input: MessagesQuery,
    ) -> Result<Page<MessageRow>> {
        crate::types::bounded(&input.session_id, 128)?;
        let conversation = Conversation {
            agent_id: input.agent_id,
            session_id: input.session_id,
        };
        let at = boundary(input.page, input.as_of.as_deref())?;
        let mut rows = in_conversation(visible(space, at), &conversation)
            .order_by_desc(events::Column::OccurredAt)
            .order_by_desc(events::Column::Id)
            .offset(input.page * 30)
            .limit(31)
            .all(&self.db)
            .await?;
        let has_more = rows.len() > 30;
        rows.truncate(30);
        let items = rows
            .into_iter()
            .rev()
            .map(|row| {
                let body = row.body.unwrap_or_default();
                Ok(MessageRow {
                    event_id: uuid_from_bytes(&row.id)?,
                    role: row.role,
                    preview: body.chars().take(360).collect(),
                    truncated: body.chars().count() > 360,
                    occurred_at: timestamp(row.occurred_at)?,
                    received_at: timestamp(row.received_at)?,
                    turn_id: row.turn_id,
                    project_key: row.project_key,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Page {
            items,
            page: input.page,
            has_more,
            as_of: timestamp(at)?,
        })
    }

    pub async fn captured_message(&self, space: SpaceId, id: Uuid) -> Result<Value> {
        let row = events::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .filter(events::Column::ForgottenAt.is_null())
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        let body = row.body.unwrap_or_default();
        Ok(
            serde_json::json!({"event_id":id,"body":body.chars().take(60_000).collect::<String>(),"truncated":body.chars().count()>60_000}),
        )
    }
}

pub(crate) async fn sessions(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<SessionsQuery>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Page<SessionRow>>> {
    let Json(input) = payload.map_err(|_| Error::invalid("会话查询格式或大小无效"))?;
    Ok(Json(store.message_sessions(space, input).await?))
}
pub(crate) async fn conversation(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<MessagesQuery>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Page<MessageRow>>> {
    let Json(input) = payload.map_err(|_| Error::invalid("消息查询格式或大小无效"))?;
    Ok(Json(store.conversation_messages(space, input).await?))
}
pub(crate) async fn detail(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(store.captured_message(space, id).await?))
}
