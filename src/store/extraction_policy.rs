//! 自动更正的准入只依赖本批证据和旧来源时序；模型不能指定跳过这些条件。
use super::knowledge_jobs::Snapshot;
use super::*;
use crate::{
    entity::{memory_sources, source_events},
    extraction::{Evidence, Replacement},
};

impl Store {
    pub(super) async fn explicit_change_is_current(
        tx: &DatabaseTransaction,
        space: SpaceId,
        snapshot: &Snapshot,
        replacement: &Replacement,
        proof: &Evidence,
        source_refs: &[Uuid],
    ) -> Result<bool> {
        let Some(event) = snapshot
            .events
            .iter()
            .find(|e| e.event_id == proof.event_id)
        else {
            return Ok(false);
        };
        if event.role != "user"
            || proof.quote.trim().is_empty()
            || proof.quote.len() > 1000
            || !event.body.contains(proof.quote.as_str())
            || snapshot
                .automatic
                .as_ref()
                .and_then(|a| a.conversation.as_ref())
                .is_some_and(|c| !c.work_event_ids.contains(&proof.event_id))
        {
            return Ok(false);
        }
        let basis =
            source_events::Entity::find_by_id((space.bytes(), proof.event_id.as_bytes().to_vec()))
                .one(tx)
                .await?
                .ok_or_else(Error::missing)?;
        let previous = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(replacement.memory_id.as_bytes().to_vec()))
            .filter(memory_sources::Column::Revision.eq(replacement.expected_revision))
            .all(tx)
            .await?;
        let automatic = write_receipts::Entity::find()
            .filter(write_receipts::Column::SpaceId.eq(space.bytes()))
            .filter(write_receipts::Column::Operation.eq("background_revision"))
            .filter(write_receipts::Column::TargetId.eq(replacement.memory_id.as_bytes().to_vec()))
            .filter(write_receipts::Column::Revision.eq(replacement.expected_revision))
            .one(tx)
            .await?
            .is_some();
        if !automatic || previous.is_empty() {
            let head = Self::head(tx, space, replacement.memory_id).await?;
            let revision = Self::revision(tx, &head).await?;
            if basis.occurred_at < revision.created_at {
                return Ok(false);
            }
        }
        let mut refs = source_refs.to_vec();
        for old in previous {
            let id = uuid_from_bytes(&old.event_id)?;
            if !refs.contains(&id) {
                refs.push(id);
            }
            let source = source_events::Entity::find_by_id((space.bytes(), old.event_id))
                .one(tx)
                .await?
                .ok_or_else(Error::missing)?;
            if source.forgotten_at.is_some() || source.occurred_at > basis.occurred_at {
                return Ok(false);
            }
        }
        Ok(refs.len() <= 16)
    }
}
