//! 会话身份与可变标题分离。标题修订防止模型响应覆盖用户刚固定的命名。
use super::*;
use crate::{
    entity::{conversation_metadata as metadata, conversation_titles as titles},
    messages::Conversation,
};
use sea_orm::{ExprTrait, IntoActiveModel, QueryOrder, sea_query::Expr};
impl Store {
    pub(crate) fn conversation_key(space: SpaceId, key: &Conversation) -> Result<Vec<u8>> {
        // JSON 元组提供明确边界；None 与 UUID 来源不同，不使用 NULL 唯一索引。
        Ok(Self::hash(&(space.0, key.agent_id, &key.session_id))?.to_vec())
    }
    pub(crate) async fn ensure_conversation<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        key: &Conversation,
    ) -> Result<metadata::Model> {
        let hash = Self::conversation_key(space, key)?;
        metadata::Entity::insert(metadata::ActiveModel {
            space_id: Set(space.bytes()),
            conversation_key: Set(hash.clone()),
            title: Set(None),
            title_locked: Set(0),
            revision: Set(0),
            updated_at: Set(now()),
        })
        .on_conflict_do_nothing_on([metadata::Column::SpaceId, metadata::Column::ConversationKey])
        .exec_without_returning(db)
        .await?;
        metadata::Entity::find_by_id((space.bytes(), hash))
            .one(db)
            .await?
            .ok_or_else(Error::missing)
    }
    pub(crate) async fn conversation_info<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        key: &Conversation,
    ) -> Result<Value> {
        let hash = Self::conversation_key(space, key)?;
        let meta = metadata::Entity::find_by_id((space.bytes(), hash.clone()))
            .one(db)
            .await?;
        let history = titles::Entity::find()
            .filter(titles::Column::SpaceId.eq(space.bytes()))
            .filter(titles::Column::ConversationKey.eq(hash))
            .order_by_desc(titles::Column::Revision)
            .limit(10)
            .all(db)
            .await?;
        Ok(
            json!({"title":meta.as_ref().and_then(|m|m.title.as_deref()),"revision":meta.as_ref().map_or(0,|m|m.revision),"locked":meta.as_ref().is_some_and(|m|m.title_locked!=0),"history":history.into_iter().map(|t|json!({"title":t.title,"revision":t.revision,"source":t.source,"at":crate::types::timestamp(t.created_at).ok()})).collect::<Vec<_>>(),"extraction":Self::extraction_progress(db,space,key).await?}),
        )
    }
    async fn write_title<C: ConnectionTrait>(
        db: &C,
        meta: metadata::Model,
        title: Option<String>,
        locked: bool,
        source: &str,
    ) -> Result<()> {
        let next = meta
            .revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        if let Some(title) = &title
            && meta.title.as_ref() != Some(title)
        {
            titles::ActiveModel {
                space_id: Set(meta.space_id.clone()),
                conversation_key: Set(meta.conversation_key.clone()),
                revision: Set(next),
                title: Set(title.clone()),
                source: Set(source.into()),
                created_at: Set(now()),
            }
            .insert(db)
            .await?;
            let keep = titles::Entity::find()
                .filter(titles::Column::SpaceId.eq(meta.space_id.clone()))
                .filter(titles::Column::ConversationKey.eq(meta.conversation_key.clone()))
                .select_only()
                .column(titles::Column::Revision)
                .order_by_desc(titles::Column::Revision)
                .limit(10)
                .into_tuple::<i64>()
                .all(db)
                .await?;
            titles::Entity::delete_many()
                .filter(titles::Column::SpaceId.eq(meta.space_id.clone()))
                .filter(titles::Column::ConversationKey.eq(meta.conversation_key.clone()))
                .filter(titles::Column::Revision.is_not_in(keep))
                .exec(db)
                .await?;
        }
        let mut active = meta.into_active_model();
        active.revision = Set(next);
        active.title = Set(title);
        active.title_locked = Set(i8::from(locked));
        active.updated_at = Set(now());
        active.update(db).await?;
        Ok(())
    }
    pub async fn set_conversation_title(
        &self,
        space: SpaceId,
        key: Conversation,
        revision: i64,
        title: Option<String>,
    ) -> Result<Value> {
        crate::types::bounded(&key.session_id, 128)?;
        let title = title.map(|s| s.trim().to_owned());
        if let Some(title) = &title {
            crate::types::bounded(title, 240)?;
            if title.contains(['\n', '\r']) {
                return Err(Error::invalid("标题须为单行文字"));
            }
        }
        let tx = self.begin(space).await?;
        // 只能为当前空间实际存在且未遗忘的会话命名。
        super::extraction::in_conversation(
            crate::entity::source_events::Entity::find()
                .filter(crate::entity::source_events::Column::SpaceId.eq(space.bytes()))
                .filter(crate::entity::source_events::Column::ForgottenAt.is_null()),
            &key,
        )
        .one(&tx)
        .await?
        .ok_or_else(Error::missing)?;
        let meta = Self::ensure_conversation(&tx, space, &key).await?;
        if meta.revision != revision {
            return Err(Error::conflict("conversation_title_changed"));
        }
        let locked = title.is_some();
        let next_title = title.or_else(|| meta.title.clone());
        Self::write_title(&tx, meta, next_title, locked, "manual").await?;
        let info = Self::conversation_info(&tx, space, &key).await?;
        tx.commit().await?;
        Ok(info)
    }
    pub(super) async fn publish_conversation_title<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        snapshot: &super::knowledge_jobs::Snapshot,
        title: Option<String>,
    ) -> Result<()> {
        let Some(context) = snapshot
            .automatic
            .as_ref()
            .and_then(|a| a.conversation.as_ref())
        else {
            return Ok(());
        };
        let title = title
            .ok_or_else(|| crate::text_model::error("model_response", "对话提炼缺少会话标题"))?;
        crate::types::bounded(&title, 240)?;
        if title.contains(['\n', '\r']) {
            return Err(crate::text_model::error(
                "model_response",
                "会话标题须为简短单行文字",
            ));
        }
        // 消费进度不按消息时间前进，但命名不能被迟到的旧主题回拨。
        let mut newest = None;
        for event in snapshot
            .events
            .iter()
            .filter(|e| context.work_event_ids.contains(&e.event_id))
        {
            let time = time::OffsetDateTime::parse(
                &event.occurred_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|_| Error::unavailable())?
            .to_offset(time::UtcOffset::UTC);
            let at = time::PrimitiveDateTime::new(time.date(), time.time());
            newest = Some(newest.map_or(at, |old: time::PrimitiveDateTime| std::cmp::max(old, at)));
        }
        let newest = newest.ok_or_else(Error::unavailable)?;
        if Self::newer_consumed_event(db, space, &context.conversation, newest).await? {
            return Ok(());
        }
        let meta = Self::ensure_conversation(db, space, &context.conversation).await?;
        if meta.title_locked == 0
            && meta.revision == context.title_revision
            && meta.title.as_deref() != Some(title.trim())
        {
            Self::write_title(db, meta, Some(title.trim().to_owned()), false, "model").await?;
        }
        Ok(())
    }
    pub(super) async fn clear_conversation_titles<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
    ) -> Result<()> {
        // 标题可能包含被遗忘事实，空间内派生标题和历史一并清空；后续从可见消息重新命名。
        titles::Entity::delete_many()
            .filter(titles::Column::SpaceId.eq(space.bytes()))
            .exec(db)
            .await?;
        metadata::Entity::update_many()
            .col_expr(metadata::Column::Title, Expr::value(Option::<String>::None))
            .col_expr(metadata::Column::TitleLocked, Expr::value(0))
            .col_expr(
                metadata::Column::Revision,
                Expr::col(metadata::Column::Revision).add(1),
            )
            .filter(metadata::Column::SpaceId.eq(space.bytes()))
            .exec(db)
            .await?;
        Ok(())
    }
}
