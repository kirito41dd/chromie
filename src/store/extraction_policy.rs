//! AI 自主修订的发布护栏：真实来源、来源时序及人工新决定仍由服务校验。
use super::knowledge_jobs::Snapshot;
use super::*;
use crate::{
    entity::{memory_sources, source_events},
    extraction::{Category, Evidence, Replacement},
};

impl Store {
    pub(super) async fn autonomous_change_is_current(
        tx: &DatabaseTransaction,
        space: SpaceId,
        snapshot: &Snapshot,
        replacement: &Replacement,
        source_refs: &[Uuid],
        evidence: &[Evidence],
        category: Category,
    ) -> Result<bool> {
        // 候选和核对阶段已逐字校验引文；在其实际来源中取最新依据，不能用处理时间代替。
        let Some(event) = snapshot
            .events
            .iter()
            .filter(|e| evidence.iter().any(|proof| proof.event_id == e.event_id))
            .filter(|e| {
                !matches!(category, Category::Preference | Category::Decision) || e.role == "user"
            })
            .max_by_key(|e| (&e.occurred_at, e.event_id))
        else {
            return Ok(false);
        };
        let basis =
            source_events::Entity::find_by_id((space.bytes(), event.event_id.as_bytes().to_vec()))
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
