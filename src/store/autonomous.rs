//! 旧待确认结果作为核对线索；固定真实来源和当前正文后交给同一模型工作流。
use super::knowledge_jobs::{AutomaticSnapshot, PendingReview, ReviewSnapshot, Snapshot};
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, organization_settings as settings, text_model_settings},
    extraction::Candidate,
    text_model::{JobKind, Queue},
};
use sea_orm::{IntoActiveModel, QueryOrder, sea_query::Expr};
use std::collections::BTreeMap;

impl Store {
    /// 设置变更会清空在途快照；先恢复其旧建议，暂停后仍可在新设置下重新核对。
    pub(super) async fn release_pending_reviews(tx: &DatabaseTransaction) -> Result<()> {
        let jobs = jobs::Entity::find()
            .filter(jobs::Column::Automatic.eq(1))
            .filter(jobs::Column::State.is_in(["pending", "running", "retry"]))
            .all(tx)
            .await?;
        for job in jobs {
            if let Some(payload) = job.payload {
                let snapshot: Snapshot =
                    serde_json::from_value(payload).map_err(|_| Error::unavailable())?;
                Self::finish_pending_review(
                    tx,
                    SpaceId(uuid_from_bytes(&job.space_id)?),
                    &snapshot,
                    uuid_from_bytes(&job.id)?,
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// 复用对应任务开关、预算和全局自动槽；新任务与旧建议状态原子登记。
    pub(super) async fn schedule_pending_review(&self) -> Result<usize> {
        let Some(setting) = settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .filter(|s| {
                (s.extraction_enabled != 0 || s.maintenance_enabled != 0) && s.next_run_at <= now()
            })
        else {
            return Ok(0);
        };
        if setting.usage_date == now().date() && setting.attempts_today >= setting.daily_limit {
            return Ok(0);
        }
        let Some(row) = jobs::Entity::find()
            .filter(jobs::Column::State.eq("done"))
            .filter(jobs::Column::Kind.is_in([
                if setting.extraction_enabled != 0 {
                    "extract"
                } else {
                    ""
                },
                if setting.maintenance_enabled != 0 {
                    "maintain"
                } else {
                    ""
                },
            ]))
            .filter(Expr::cust(
                "JSON_EXTRACT(result, '$.pending_revisions') > 0",
            ))
            .order_by_desc(jobs::Column::CreatedAt)
            .one(&self.db)
            .await?
        else {
            return Ok(0);
        };
        let space = SpaceId(uuid_from_bytes(&row.space_id)?);
        let tx = self.begin(space).await?;
        Self::model_settings_locked(&tx).await?;
        if text_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .is_none_or(|c| c.enabled == 0)
        {
            return Ok(0);
        }
        let setting = settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?;
        let kind = if row.kind == "extract" {
            JobKind::Extract
        } else {
            JobKind::Maintain
        };
        if setting.next_run_at > now()
            || !Self::automatic_allowed(&tx).await?
            || kind == JobKind::Extract && setting.extraction_enabled == 0
            || kind == JobKind::Maintain && setting.maintenance_enabled == 0
        {
            return Ok(0);
        }
        if jobs::Entity::find()
            .filter(jobs::Column::Automatic.eq(1))
            .filter(jobs::Column::State.is_in(["pending", "running", "retry"]))
            .one(&tx)
            .await?
            .is_some()
        {
            return Ok(0);
        }
        let row = jobs::Entity::find_by_id((space.bytes(), row.id))
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let mut result = row.result.clone().unwrap_or_default();
        let Some(decision) = result["decisions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|d| {
                d["action"] == "propose_revision"
                    && d["applied"] != true
                    && d["resolution"].is_null()
            })
            .cloned()
        else {
            let mut active = row.into_active_model();
            result["pending_revisions"] = json!(0);
            active.result = Set(Some(result));
            active.update(&tx).await?;
            tx.commit().await?;
            return Ok(1);
        };
        let index = decision["candidate_index"]
            .as_u64()
            .ok_or_else(Error::unavailable)? as usize;
        let target: Uuid = serde_json::from_value(decision["replacement"]["memory_id"].clone())
            .map_err(|_| Error::unavailable())?;
        let Some(head) = memories::Entity::find_by_id((space.bytes(), target.as_bytes().to_vec()))
            .one(&tx)
            .await?
            .filter(|m| m.forgotten_at.is_none())
        else {
            Self::settle_old_review(&tx, row, index, None, Some("superseded")).await?;
            tx.commit().await?;
            return Ok(1);
        };
        if crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
            .one(&tx)
            .await?
            .is_some()
            || Self::revision(&tx, &head).await?.memory_type != "note"
        {
            Self::settle_old_review(&tx, row, index, None, Some("superseded")).await?;
            tx.commit().await?;
            return Ok(1);
        }
        let mut events = vec![];
        let candidate = if kind == JobKind::Extract {
            let parsed = serde_json::from_value::<Candidate>(json!({
                "content":decision["content"], "category":decision["category"], "search_query":"",
                "source_refs":decision["source_refs"], "evidence":decision["evidence"]
            }))
            .map_err(|_| Error::unavailable())?;
            if parsed.source_refs.is_empty()
                || parsed.source_refs.len() > 16
                || parsed.evidence.len() > 8
            {
                Self::settle_old_review(&tx, row, index, None, Some("deferred")).await?;
                tx.commit().await?;
                return Ok(1);
            }
            for id in &parsed.source_refs {
                let source = crate::entity::source_events::Entity::find_by_id((
                    space.bytes(),
                    id.as_bytes().to_vec(),
                ))
                .one(&tx)
                .await?;
                if source.is_none_or(|s| s.forgotten_at.is_some() || s.body.is_none())
                    || Self::auxiliary_source(&tx, space, *id).await?
                {
                    Self::settle_old_review(&tx, row, index, None, Some("superseded")).await?;
                    tx.commit().await?;
                    return Ok(1);
                }
            }
            events = parsed.source_refs.clone();
            Some(parsed)
        } else {
            None
        };
        let id = Uuid::now_v7();
        self.enqueue_knowledge_tx(
            &tx,
            space,
            Queue {
                request_id: id,
                kind,
                memory_ids: vec![target],
                event_ids: events,
            },
            Some(AutomaticSnapshot {
                settings_revision: setting.revision,
                memory_ids: vec![],
                conversation: None,
            }),
        )
        .await?;
        let job = jobs::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let mut snapshot: Snapshot =
            serde_json::from_value(job.payload.clone().ok_or_else(Error::unavailable)?)
                .map_err(|_| Error::unavailable())?;
        if let Some(candidate) = candidate {
            let mut rejected = BTreeMap::new();
            if let Some(issue) = Self::candidate_issue(&snapshot, &candidate) {
                rejected.insert(0, issue.to_owned());
            }
            snapshot.review = Some(ReviewSnapshot {
                candidates: vec![candidate],
                conversation_title: None,
                retrieval_partial: false,
                rejected,
            });
        }
        snapshot.pending_review = Some(PendingReview {
            job_id: uuid_from_bytes(&row.id)?,
            candidate_index: index,
        });
        let mut job = job.into_active_model();
        job.payload = Set(Some(
            serde_json::to_value(snapshot).map_err(|_| Error::unavailable())?,
        ));
        job.update(&tx).await?;
        Self::settle_old_review(&tx, row, index, Some(id), Some("rechecking")).await?;
        let mut active = setting.into_active_model();
        active.next_run_at =
            Set(now() + time::Duration::seconds(i64::from(*active.interval_seconds.as_ref())));
        active.last_kind = Set(kind.as_str().into());
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        Ok(1)
    }

    async fn settle_old_review(
        tx: &DatabaseTransaction,
        row: jobs::Model,
        index: usize,
        new_job: Option<Uuid>,
        resolution: Option<&str>,
    ) -> Result<()> {
        let mut result = row.result.clone().unwrap_or_default();
        if let Some(decision) = result["decisions"].as_array_mut().and_then(|ds| {
            ds.iter_mut()
                .find(|d| d["candidate_index"].as_u64() == Some(index as u64))
        }) {
            decision["resolution"] = json!(resolution);
            if let Some(id) = new_job {
                decision["review_job_id"] = json!(id);
            } else if resolution.is_none()
                && let Some(fields) = decision.as_object_mut()
            {
                fields.remove("review_job_id");
            }
        }
        result["pending_revisions"] = json!(Self::pending_proposal_count(&result));
        let mut active = row.into_active_model();
        active.result = Set(Some(result));
        active.updated_at = Set(now());
        active.update(tx).await?;
        Ok(())
    }

    pub(super) async fn finish_pending_review(
        tx: &DatabaseTransaction,
        space: SpaceId,
        snapshot: &Snapshot,
        job: Uuid,
        resolution: Option<&str>,
    ) -> Result<()> {
        let Some(pending) = &snapshot.pending_review else {
            return Ok(());
        };
        let Some(row) =
            jobs::Entity::find_by_id((space.bytes(), pending.job_id.as_bytes().to_vec()))
                .lock_exclusive()
                .one(tx)
                .await?
        else {
            return Ok(());
        };
        Self::settle_old_review(
            tx,
            row,
            pending.candidate_index,
            resolution.map(|_| job),
            resolution,
        )
        .await
    }
}
