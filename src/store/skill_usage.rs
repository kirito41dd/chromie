use super::*;
use crate::entity::{skill_revisions as revisions, skill_usage_reports as reports, skills};
use sea_orm::{IntoActiveModel, QueryOrder};

fn increment(counts: &mut Value, outcome: &str) {
    let count = counts[outcome]
        .as_u64()
        .unwrap_or_default()
        .saturating_add(1);
    counts[outcome] = Value::from(count);
}

impl Store {
    pub async fn skill_report(
        &self,
        space: SpaceId,
        id: Uuid,
        revision: Option<i32>,
        request_id: Uuid,
        outcome: &str,
        reason: Option<&str>,
    ) -> Result<Value> {
        if !matches!(outcome, "used" | "verified" | "corrected")
            || revision.is_some_and(|r| r < 1)
            || reason.is_some_and(|s| s.len() > 1000)
        {
            return Err(Error::invalid("Skill 使用报告的结果、修订或说明无效"));
        }
        let tx = self.begin(space).await?;
        let hash = Self::hash(&(id, revision, outcome, reason))?;
        let receipt = write_receipts::Entity::find_by_id((
            space.bytes(),
            "skill_report".into(),
            request_id.as_bytes().to_vec(),
        ))
        .one(&tx)
        .await?;
        let response = json!({"skill_id":id,"revision":revision,"recorded":true});
        if let Some(r) = receipt {
            if r.input_hash != hash || r.agent_id != self.actor_bytes() {
                return Err(Error::conflict("idempotency_conflict"));
            }
            tx.commit().await?;
            return Ok(response);
        }
        let mut head = skills::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        increment(&mut head.usage_counts, outcome);
        if revision.is_none() {
            increment(&mut head.usage_counts["unknown_revision"], outcome);
        }
        let counts = head.usage_counts.clone();
        let mut head = head.into_active_model();
        head.usage_counts = Set(counts);
        head.update(&tx).await?;
        if let Some(n) = revision {
            let mut row = revisions::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec(), n))
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
            increment(&mut row.usage_counts, outcome);
            let counts = row.usage_counts.clone();
            let mut row = row.into_active_model();
            row.usage_counts = Set(counts);
            row.update(&tx).await?;
        }
        reports::ActiveModel {
            space_id: Set(space.bytes()),
            skill_id: Set(id.as_bytes().to_vec()),
            agent_id: Set(self.actor_bytes().ok_or_else(Error::unauthorized)?),
            request_id: Set(request_id.as_bytes().to_vec()),
            revision: Set(revision),
            outcome: Set(outcome.into()),
            reason: Set(reason.map(str::to_owned)),
            created_at: Set(now()),
        }
        .insert(&tx)
        .await?;
        self.receipt(&tx, space, "skill_report", request_id, hash, (id, revision))
            .await?;
        self.trim_skill_reports(&tx, space).await?;
        tx.commit().await?;
        Ok(response)
    }

    pub(super) async fn trim_skill_reports(
        &self,
        tx: &DatabaseTransaction,
        space: SpaceId,
    ) -> Result<()> {
        let mut trim = sea_orm::Condition::any()
            .add(reports::Column::CreatedAt.lt(now() - time::Duration::days(30)));
        let excess = reports::Entity::find()
            .filter(reports::Column::SpaceId.eq(space.bytes()))
            .order_by_desc(reports::Column::CreatedAt)
            .order_by_desc(reports::Column::RequestId)
            .offset(100)
            .limit(256)
            .all(tx)
            .await?;
        if !excess.is_empty() {
            trim = trim
                .add(reports::Column::RequestId.is_in(excess.into_iter().map(|r| r.request_id)));
        }
        reports::Entity::delete_many()
            .filter(reports::Column::SpaceId.eq(space.bytes()))
            .filter(trim)
            .exec(tx)
            .await?;
        Ok(())
    }
}
