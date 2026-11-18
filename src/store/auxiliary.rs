//! 已知 Codex 辅助模板按整个会话隔离。原文保留用于调试，不作为用户事实提炼。
use super::*;
use crate::entity::source_events as events;
use sea_orm::{
    Condition, ExprTrait,
    sea_query::{Alias, Expr, Query, SelectStatement},
};

// 只匹配明确的应用模板开头；不按“标题”、JSON、短会话或执行方式猜测。
pub(super) const PREFIXES: [&str; 2] = [
    "Your task is to generate a very short title for a conversation based on the user's first message.",
    "This is an app-generated request for a suggested next user message.",
];

/// 相关子查询显式绑定空间、Agent（包括 NULL）和 session，避免同名会话串源。
fn auxiliary_session() -> SelectStatement {
    let aux = Alias::new("auxiliary_source");
    Query::select()
        .expr(Expr::val(1))
        .from_as(events::Entity, aux.clone())
        .and_where(
            Expr::col((aux.clone(), events::Column::SpaceId))
                .equals((events::Entity, events::Column::SpaceId)),
        )
        .and_where(
            Expr::col((aux.clone(), events::Column::SessionId))
                .equals((events::Entity, events::Column::SessionId)),
        )
        .cond_where(
            Condition::any()
                .add(
                    Expr::col((aux.clone(), events::Column::AgentId))
                        .equals((events::Entity, events::Column::AgentId)),
                )
                .add(
                    Condition::all()
                        .add(Expr::col((aux.clone(), events::Column::AgentId)).is_null())
                        .add(Expr::col((events::Entity, events::Column::AgentId)).is_null()),
                ),
        )
        .and_where(Expr::col((aux.clone(), events::Column::ClientType)).eq("codex"))
        .and_where(Expr::col((aux.clone(), events::Column::Role)).eq("user"))
        .cond_where(PREFIXES.iter().fold(Condition::any(), |c, prefix| {
            c.add(Expr::col((aux.clone(), events::Column::Body)).like(format!("{prefix}%")))
        }))
        .to_owned()
}

pub(super) fn eligible(query: sea_orm::Select<events::Entity>) -> sea_orm::Select<events::Entity> {
    query.filter(Expr::exists(auxiliary_session()).not())
}

impl Store {
    pub(super) async fn auxiliary_source<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<bool> {
        Ok(
            events::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .filter(Expr::exists(auxiliary_session()))
                .one(db)
                .await?
                .is_some(),
        )
    }
    pub(super) async fn auxiliary_conversation<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        key: &crate::messages::Conversation,
    ) -> Result<bool> {
        Ok(super::extraction::in_conversation(
            events::Entity::find()
                .filter(events::Column::SpaceId.eq(space.bytes()))
                .filter(Expr::exists(auxiliary_session())),
            key,
        )
        .one(db)
        .await?
        .is_some())
    }
}
