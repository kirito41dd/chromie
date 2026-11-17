//! 自主修订共用原子写入事务；历史建议的生命周期与当前版本分开维护。
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, memory_sources},
    extraction::Replacement,
    knowledge::MemoryType,
    types::Correct,
};
use sea_orm::IntoActiveModel;

impl Store {
    pub async fn knowledge_review(&self, space: SpaceId, job_id: Uuid) -> Result<Value> {
        let row = jobs::Entity::find_by_id((space.bytes(), job_id.as_bytes().to_vec()))
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        let mut view = Self::job_view(&row);
        let mut result = row.result.unwrap_or_else(|| json!({}));
        if let Some(fields) = result.as_object_mut() {
            fields.remove("request_hash");
        }
        view["result"] = result;
        Ok(view)
    }

    /// 调用方持空间锁并校验来源证据；此处统一维护旧来源、关系、修订和幂等回执。
    pub(super) async fn apply_replacement_tx(
        &self,
        tx: &DatabaseTransaction,
        space: SpaceId,
        replacement: Replacement,
        source_refs: &[Uuid],
        reason: String,
    ) -> Result<Value> {
        let head = Self::head(tx, space, replacement.memory_id).await?;
        let old = Self::revision(tx, &head).await?;
        if old.memory_type != MemoryType::Note.as_str()
            || crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
                .one(tx)
                .await?
                .is_some()
        {
            return Err(Error::invalid("此方案只能修订普通记忆"));
        }
        let mut refs = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(head.id.clone()))
            .filter(memory_sources::Column::Revision.eq(head.current_revision))
            .all(tx)
            .await?
            .into_iter()
            .map(|s| uuid_from_bytes(&s.event_id))
            .collect::<Result<Vec<_>>>()?;
        for id in source_refs {
            if !refs.contains(id) {
                refs.push(*id);
            }
        }
        // 来源没有上限扩张：超限时保留旧修订并暂缓，不静默丢弃旧依据。
        if refs.len() > 16 {
            return Err(Error::invalid("修订关联来源超过16条，保留当前正文并暂缓"));
        }
        self.correct_tx(
            tx,
            space,
            Correct {
                memory_id: replacement.memory_id,
                expected_revision: replacement.expected_revision,
                request_id: Uuid::now_v7(),
                reason,
                content: Some(replacement.content),
                scope: None,
                source_refs: Some(refs.into_iter().map(|id| id.to_string()).collect()),
                memory_type: None,
                links: None,
            },
        )
        .await
    }

    pub(super) fn pending_proposal_count(result: &Value) -> usize {
        result["decisions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| {
                d["action"] == "propose_revision"
                    && d["applied"] != true
                    && d["resolution"].is_null()
            })
            .count()
    }

    /// 只扫描仍有待复核结果的任务；JSON 路径固定，目标 ID 通过绑定参数传递。
    async fn proposals_for<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<Vec<jobs::Model>> {
        use sea_orm::sea_query::Expr;
        Ok(jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .filter(jobs::Column::State.eq("done"))
            .filter(Expr::cust("JSON_EXTRACT(result, '$.pending_revisions') > 0"))
            .filter(Expr::cust_with_values("JSON_CONTAINS(JSON_EXTRACT(result, '$.decisions[*].replacement.memory_id'), JSON_QUOTE(?))", [id.to_string()]))
            .all(db).await?)
    }

    pub(super) async fn pending_revisions<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<usize> {
        let mut count = 0;
        for row in Self::proposals_for(db, space, id).await? {
            let result = row.result.unwrap_or_default();
            count += result["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| {
                    d["replacement"]["memory_id"] == id.to_string()
                        && d["replacement"]["expected_revision"] == revision
                        && d["action"] == "propose_revision"
                        && d["applied"] != true
                        && d["resolution"].is_null()
                })
                .count();
        }
        Ok(count)
    }

    /// 任何成功更正都会使基于更旧修订的草案失效；不会自动把旧草案套到新正文上。
    pub(super) async fn supersede_proposals(
        tx: &DatabaseTransaction,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<()> {
        for row in Self::proposals_for(tx, space, id).await? {
            let mut result = row.result.clone().unwrap_or_default();
            if let Some(decisions) = result["decisions"].as_array_mut() {
                for d in decisions {
                    if d["replacement"]["memory_id"] == id.to_string()
                        && d["replacement"]["expected_revision"]
                            .as_i64()
                            .is_some_and(|r| r < i64::from(revision))
                        && d["applied"] != true
                    {
                        d["resolution"] = json!("superseded");
                    }
                }
            }
            result["pending_revisions"] = json!(Self::pending_proposal_count(&result));
            let mut active = row.into_active_model();
            active.result = Set(Some(result));
            active.updated_at = Set(now());
            active.update(tx).await?;
        }
        Ok(())
    }
}
