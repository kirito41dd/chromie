//! 会话列表在同一读事务中先取得稳定的一页，再一次读取该页所需的摘要。
use super::*;
use crate::entity::source_events as events;
use crate::messages::{Conversation, Page, SessionRow, SessionsQuery, boundary, visible};
use crate::types::timestamp;
use sea_orm::{FromQueryResult, QueryOrder, raw_sql};

#[derive(FromQueryResult)]
struct SessionGroup {
    anchor: Uuid,
    #[sea_orm(nested)]
    conversation: Conversation,
    message_count: i64,
    received_at: time::PrimitiveDateTime,
}

#[derive(FromQueryResult)]
struct Summary {
    title: String,
    preview: String,
    agent_name: Option<String>,
    client_type: Option<String>,
    total: i64,
    completed: i64,
    auxiliary_messages: i64,
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
            .column_as(events::Column::Id.min(), "anchor")
            .column_as(events::Column::Id.count(), "message_count")
            .column_as(events::Column::ReceivedAt.max(), "received_at")
            .group_by(events::Column::AgentId)
            .group_by(events::Column::SessionId)
            .order_by_desc(events::Column::ReceivedAt.max())
            .order_by_asc(events::Column::SessionId)
            .order_by_asc(events::Column::AgentId)
            .offset(input.page * 20)
            .limit(21)
            .into_model::<SessionGroup>()
            .all(&tx)
            .await?;
        let has_more = groups.len() > 20;
        groups.truncate(20);
        let items = Self::session_rows(&tx, space, at, groups).await?;
        tx.commit().await?;
        Ok(Page {
            items,
            page: input.page,
            has_more,
            as_of: timestamp(at)?,
            conversation_info: None,
        })
    }

    async fn session_rows(
        db: &DatabaseTransaction,
        space: SpaceId,
        at: time::PrimitiveDateTime,
        groups: Vec<SessionGroup>,
    ) -> Result<Vec<SessionRow>> {
        if groups.is_empty() {
            return Ok(Vec::new());
        }

        let mut keys = Vec::with_capacity(groups.len());
        for group in &groups {
            let key = &group.conversation;
            let hash: String = Self::conversation_key(space, key)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            keys.push(json!({"event":group.anchor, "metadata":hash}));
        }

        // 每个会话只绑定一条代表事件的 ID 与标题键。通过该事件比较同表的会话列，
        // 沿用源表的排序规则及可空 Agent 身份。ord 对应分页查询的顺序。
        // 消息数与预览遵循浏览快照，标题与提炼进度使用当前读事务的状态。
        let keys = Value::Array(keys);
        let space_id = space.bytes();
        let [title_prefix, suggestion_prefix] = super::auxiliary::PREFIXES.map(|s| format!("{s}%"));
        let rows = Summary::find_by_statement(raw_sql!(
            MySql,
            r#"
            SELECT
                COALESCE(m.title, (
                    SELECT LEFT(e.body, 48) FROM source_events e
                    WHERE e.space_id = p.space_id AND e.session_id = p.session_id
                      AND e.agent_id <=> p.agent_id AND e.forgotten_at IS NULL
                      AND e.received_at <= {at} AND e.role = 'user'
                    ORDER BY e.occurred_at, e.id LIMIT 1
                ), '未命名会话') AS title,
                COALESCE((
                    SELECT LEFT(e.body, 120) FROM source_events e
                    WHERE e.space_id = p.space_id AND e.session_id = p.session_id
                      AND e.agent_id <=> p.agent_id AND e.forgotten_at IS NULL
                      AND e.received_at <= {at}
                    ORDER BY e.received_at DESC, e.id DESC LIMIT 1
                ), '') AS preview,
                a.name AS agent_name, a.client_type, s.total, s.completed, s.auxiliary_messages
            FROM JSON_TABLE({keys}, '$[*]' COLUMNS (
                ord FOR ORDINALITY,
                event_id CHAR(36) PATH '$.event',
                metadata_hash CHAR(64) PATH '$.metadata'
            )) AS k
            JOIN source_events p ON p.space_id = {space_id} AND p.id = UUID_TO_BIN(k.event_id)
            LEFT JOIN conversation_metadata m
              ON m.space_id = p.space_id AND m.conversation_key = UNHEX(k.metadata_hash)
            LEFT JOIN agent_connections a
              ON a.space_id = p.space_id AND a.id = p.agent_id
            JOIN LATERAL (
                SELECT COUNT(CASE WHEN e.forgotten_at IS NULL THEN 1 END) AS total,
                       COUNT(CASE WHEN e.forgotten_at IS NULL AND i.state = 'done' THEN 1 END) AS completed,
                       COUNT(CASE WHEN e.client_type = 'codex' AND e.role = 'user'
                                   AND (e.body LIKE {title_prefix} OR e.body LIKE {suggestion_prefix})
                                  THEN 1 END) AS auxiliary_messages
                FROM source_events e
                LEFT JOIN extraction_items i ON i.space_id = e.space_id AND i.event_id = e.id
                WHERE e.space_id = p.space_id AND e.session_id = p.session_id
                  AND e.agent_id <=> p.agent_id
            ) s ON TRUE
            ORDER BY k.ord
            "#
        ))
        .all(db)
        .await?;

        if groups.len() != rows.len() {
            return Err(Error::unavailable());
        }
        groups
            .into_iter()
            .zip(rows)
            .map(|(group, row)| {
                let origin = match group.conversation.agent_id {
                    None => None,
                    Some(id) => Some((
                        id,
                        row.agent_name.ok_or_else(Error::missing)?,
                        row.client_type.ok_or_else(Error::missing)?,
                    )),
                };
                Ok(SessionRow {
                    conversation: group.conversation,
                    message_count: group.message_count,
                    received_at: timestamp(group.received_at)?,
                    title: row.title,
                    preview: row.preview,
                    origin: Self::format_source_label(origin),
                    extraction: super::extraction::progress_value(
                        row.total as u64,
                        row.completed as u64,
                        row.auxiliary_messages != 0,
                    ),
                })
            })
            .collect()
    }
}
