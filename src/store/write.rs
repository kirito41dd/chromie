use super::*;
use crate::{
    entity::{memory_sources, source_events},
    types::{Correct, Event, Forget, Remember, bounded},
};
use sea_orm::{IntoActiveModel, QueryOrder};
use std::collections::HashSet;

impl Store {
    pub async fn capture(&self, space: SpaceId, event: Event) -> Result<Value> {
        bounded(&event.client_type, 32)?;
        bounded(&event.session_id, 128)?;
        bounded(&event.body, 60000)?;
        if !matches!(event.role.as_str(), "user" | "assistant") {
            return Err(Error::invalid("仅采集用户消息与最终回复"));
        }
        if let Some(value) = &event.turn_id {
            bounded(value, 128)?;
        }
        if let Some(value) = &event.project_key {
            bounded(value, 128)?;
        }
        let occurred = time::OffsetDateTime::parse(
            &event.occurred_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| Error::invalid("发生时间格式无效"))?
        .to_offset(time::UtcOffset::UTC);
        // MySQL datetime(6) 保存微秒；重试比较必须使用相同精度。
        let occurred = time::PrimitiveDateTime::new(occurred.date(), occurred.time())
            .replace_nanosecond(occurred.nanosecond() / 1000 * 1000)
            .map_err(|_| Error::invalid("发生时间精度无效"))?;
        let tx = self.begin(space).await?;
        if let Some(existing) =
            source_events::Entity::find_by_id((space.bytes(), event.id.as_bytes().to_vec()))
                .one(&tx)
                .await?
        {
            if existing.agent_id != self.actor_bytes() {
                return Err(Error::conflict("idempotency_conflict"));
            }
            if existing.forgotten_at.is_some() {
                return Ok(json!({"event_id":event.id,"hidden":true,"duplicate":true}));
            }
            if existing.body.as_deref() != Some(&event.body)
                || existing.role != event.role
                || existing.session_id != event.session_id
                || existing.turn_id != event.turn_id
                || existing.client_type != event.client_type
                || existing.project_key != event.project_key
                || existing.occurred_at != occurred
            {
                return Err(Error::conflict("idempotency_conflict"));
            }
            return Ok(json!({"event_id":event.id,"duplicate":true}));
        }
        source_events::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(event.id.as_bytes().to_vec()),
            client_type: Set(event.client_type),
            session_id: Set(event.session_id),
            turn_id: Set(event.turn_id),
            role: Set(event.role),
            body: Set(Some(event.body)),
            project_key: Set(event.project_key),
            occurred_at: Set(occurred),
            received_at: Set(now()),
            forgotten_at: Set(None),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        tx.commit().await?;
        Ok(json!({"event_id":event.id,"stored":true}))
    }

    async fn check_sources<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        refs: &[Uuid],
    ) -> Result<()> {
        if refs.len() > 16 {
            return Err(Error::invalid("来源最多 16 条"));
        }
        for id in refs {
            let event = source_events::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .one(db)
                .await?
                .ok_or_else(Error::missing)?;
            if event.forgotten_at.is_some() {
                return Err(Error::missing());
            }
        }
        Ok(())
    }

    async fn add_sources<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
        refs: &[Uuid],
    ) -> Result<()> {
        for event in refs.iter().collect::<HashSet<_>>() {
            memory_sources::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                revision: Set(revision),
                event_id: Set(event.as_bytes().to_vec()),
            }
            .insert(db)
            .await?;
        }
        Ok(())
    }

    pub async fn remember(&self, space: SpaceId, input: Remember) -> Result<Value> {
        bounded(&input.content, 16000)?;
        let project = input.scope.project()?;
        let hash = Self::hash(&input)?;
        let tx = self.begin(space).await?;
        if let Some(result) = self
            .replay(&tx, space, "remember", input.request_id, &hash)
            .await?
        {
            return Ok(result);
        }
        Self::check_sources(&tx, space, &input.source_refs).await?;
        let id = Uuid::now_v7();
        let current = now();
        memories::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            current_revision: Set(1),
            created_at: Set(current),
            updated_at: Set(current),
            forgotten_at: Set(None),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        memory_revisions::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            revision: Set(1),
            content: Set(Some(input.content)),
            project_key: Set(project),
            change_reason: Set(None),
            created_at: Set(current),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        Self::add_sources(&tx, space, id, 1, &input.source_refs).await?;
        Self::schedule(&tx, space, id, 1).await?;
        self.receipt(
            &tx,
            space,
            "remember",
            input.request_id,
            hash,
            (id, Some(1)),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"memory_id":id,"revision":1,"readable":true,"processing_state":"index_pending","evidence_status":if input.source_refs.is_empty(){"unverified"}else{"source_linked"}}),
        )
    }

    pub async fn correct(&self, space: SpaceId, input: Correct) -> Result<Value> {
        bounded(&input.reason, 4000)?;
        if input.content.is_none() && input.scope.is_none() {
            return Err(Error::invalid("至少修改正文或适用范围"));
        }
        if let Some(value) = &input.content {
            bounded(value, 16000)?;
        }
        let hash = Self::hash(&input)?;
        let tx = self.begin(space).await?;
        if let Some(result) = self
            .replay(&tx, space, "correct_memory", input.request_id, &hash)
            .await?
        {
            return Ok(result);
        }
        let head = Self::head(&tx, space, input.memory_id).await?;
        if head.forgotten_at.is_some() {
            return Err(Error::missing());
        }
        if head.current_revision != input.expected_revision {
            return Err(Error::conflict("revision_conflict"));
        }
        if crate::entity::secrets::Entity::find_by_id((
            space.bytes(),
            input.memory_id.as_bytes().to_vec(),
        ))
        .one(&tx)
        .await?
        .is_some()
        {
            if let Some(content) = &input.content {
                bounded(content, 256)?;
            }
            if input
                .source_refs
                .as_ref()
                .is_some_and(|refs| !refs.is_empty())
            {
                return Err(Error::invalid(
                    "Secret 引用只能更正公开说明或范围，不能关联原始事件；原文更新请使用独立 HTTP",
                ));
            }
        }
        let previous = Self::revision(&tx, &head).await?;
        let refs = match &input.source_refs {
            Some(refs) => refs
                .iter()
                .map(|v| Uuid::parse_str(v).map_err(|_| Error::invalid("来源 ID 无效")))
                .collect::<Result<Vec<_>>>()?,
            None => memory_sources::Entity::find()
                .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                .filter(memory_sources::Column::MemoryId.eq(head.id.clone()))
                .filter(memory_sources::Column::Revision.eq(head.current_revision))
                .all(&tx)
                .await?
                .into_iter()
                .map(|v| uuid_from_bytes(&v.event_id))
                .collect::<Result<Vec<_>>>()?,
        };
        Self::check_sources(&tx, space, &refs).await?;
        let revision = head
            .current_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        let project = match &input.scope {
            Some(scope) => scope.project()?,
            None => previous.project_key,
        };
        memory_revisions::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(head.id.clone()),
            revision: Set(revision),
            content: Set(input.content.or(previous.content)),
            project_key: Set(project),
            change_reason: Set(Some(input.reason)),
            created_at: Set(now()),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        Self::add_sources(&tx, space, input.memory_id, revision, &refs).await?;
        let mut active = head.into_active_model();
        active.current_revision = Set(revision);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        Self::schedule(&tx, space, input.memory_id, revision).await?;
        self.receipt(
            &tx,
            space,
            "correct_memory",
            input.request_id,
            hash,
            (input.memory_id, Some(revision)),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"memory_id":input.memory_id,"revision":revision,"readable":true,"processing_state":"index_pending"}),
        )
    }

    pub async fn forget(&self, space: SpaceId, input: Forget) -> Result<Value> {
        let target = match (input.memory_id, input.event_id) {
            (Some(id), None) | (None, Some(id)) => id,
            _ => return Err(Error::invalid("须且只能指定一个记忆或原始事件")),
        };
        let hash = Self::hash(&input)?;
        let tx = self.begin(space).await?;
        if let Some(result) = self
            .replay(&tx, space, "forget_memory", input.request_id, &hash)
            .await?
        {
            return Ok(result);
        }
        let mut affected = HashSet::new();
        let revision;
        let events: Vec<Vec<u8>>;
        if let Some(id) = input.memory_id {
            let head = Self::head(&tx, space, id).await?;
            if Some(head.current_revision) != input.expected_revision {
                return Err(Error::conflict("revision_conflict"));
            }
            revision = Some(head.current_revision);
            affected.insert(id);
            events = memory_sources::Entity::find()
                .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                .filter(memory_sources::Column::MemoryId.eq(head.id))
                .all(&tx)
                .await?
                .into_iter()
                .map(|v| v.event_id)
                .collect();
        } else {
            source_events::Entity::find_by_id((space.bytes(), target.as_bytes().to_vec()))
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
            revision = None;
            events = vec![target.as_bytes().to_vec()];
        }
        for event in events.iter().collect::<HashSet<_>>() {
            // MVP 以整条事件为证据单位，无法可靠切分的共享来源整体清理。
            for related in memory_sources::Entity::find()
                .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                .filter(memory_sources::Column::EventId.eq((*event).clone()))
                .all(&tx)
                .await?
            {
                affected.insert(uuid_from_bytes(&related.memory_id)?);
            }
            if let Some(source) =
                source_events::Entity::find_by_id((space.bytes(), (*event).clone()))
                    .one(&tx)
                    .await?
            {
                let mut active = source.into_active_model();
                active.body = Set(None);
                active.project_key = Set(None);
                active.forgotten_at = Set(Some(now()));
                active.update(&tx).await?;
            }
        }
        for id in &affected {
            let head = Self::head(&tx, space, *id).await?;
            let version = head.current_revision;
            let mut active = head.into_active_model();
            active.forgotten_at = Set(Some(now()));
            active.updated_at = Set(now());
            active.update(&tx).await?;
            for row in memory_revisions::Entity::find()
                .filter(memory_revisions::Column::SpaceId.eq(space.bytes()))
                .filter(memory_revisions::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .order_by_asc(memory_revisions::Column::Revision)
                .all(&tx)
                .await?
            {
                let mut active = row.into_active_model();
                active.content = Set(None);
                active.change_reason = Set(None);
                active.project_key = Set(None);
                active.update(&tx).await?;
            }
            crate::entity::secrets::Entity::update_many()
                .col_expr(
                    crate::entity::secrets::Column::Contract,
                    sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
                )
                .col_expr(
                    crate::entity::secrets::Column::Ciphertext,
                    sea_orm::sea_query::Expr::value(Option::<Vec<u8>>::None),
                )
                .col_expr(
                    crate::entity::secrets::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now()),
                )
                .filter(crate::entity::secrets::Column::SpaceId.eq(space.bytes()))
                .filter(crate::entity::secrets::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .exec(&tx)
                .await?;
            Self::schedule(&tx, space, *id, version).await?;
        }
        self.receipt(
            &tx,
            space,
            "forget_memory",
            input.request_id,
            hash,
            (target, revision),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"target_id":target,"hidden":true,"purge_state":"index_pending","affected_memories":affected.len()}),
        )
    }
}
