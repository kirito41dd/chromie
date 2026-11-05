use super::*;
use sea_orm::{IntoActiveModel, QueryOrder};

impl Store {
    pub async fn sync_due(&self) -> Result<usize> {
        let tasks = outbox_tasks::Entity::find()
            .filter(outbox_tasks::Column::State.eq("pending"))
            .filter(outbox_tasks::Column::RunAfter.lte(now()))
            .order_by_asc(outbox_tasks::Column::RunAfter)
            .limit(20)
            .all(&self.db)
            .await?;
        let mut count = 0;
        for task in tasks {
            let space = SpaceId(uuid_from_bytes(&task.space_id)?);
            let id = uuid_from_bytes(&task.memory_id)?;
            let tx = self.begin(space).await?;
            let task = outbox_tasks::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
            if task.state != "pending" || task.run_after > now() {
                continue;
            }
            let head = Self::head(&tx, space, id).await?;
            let result = if head.forgotten_at.is_some() {
                self.search.delete(space, id).await
            } else {
                let revision = Self::revision(&tx, &head).await?;
                self.search
                    .upsert(
                        space,
                        id,
                        head.current_revision,
                        revision.content.as_deref().ok_or_else(Error::missing)?,
                        revision.project_key.as_deref(),
                    )
                    .await
            };
            let mut active = task.clone().into_active_model();
            active.updated_at = Set(now());
            if result.is_ok() {
                active.state = Set("done".into());
                count += 1;
            } else {
                let attempts = task.attempts.saturating_add(1);
                active.attempts = Set(attempts);
                active.run_after =
                    Set(now() + time::Duration::seconds(i64::from(attempts.min(30)) * 2));
            }
            active.update(&tx).await?;
            tx.commit().await?;
        }
        Ok(count)
    }

    pub async fn rebuild(&self, space: SpaceId) -> Result<usize> {
        let tx = self.begin(space).await?;
        let rows = memories::Entity::find()
            .filter(memories::Column::SpaceId.eq(space.bytes()))
            .all(&tx)
            .await?;
        for row in &rows {
            Self::schedule(&tx, space, uuid_from_bytes(&row.id)?, row.current_revision).await?;
        }
        tx.commit().await?;
        Ok(rows.len())
    }
}
