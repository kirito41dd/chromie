//! 历史只保留有限正文；修订小元数据承接统计／回执，派生引用所依赖的正文不可压缩。
use super::*;
use crate::entity::{
    conversation_titles as titles, knowledge_jobs as jobs, memory_links, memory_sources,
};
use sea_orm::{
    ExprTrait, IntoActiveModel, QueryOrder,
    sea_query::{Expr, Query},
};
use std::collections::HashSet;

const RETAIN_REVISIONS: i32 = 5;
const RETAIN_JOB_DETAILS: u64 = 100;
const RETAIN_JOB_DAYS: i64 = 30;

impl Store {
    pub(super) async fn compact_memory_history(
        tx: &DatabaseTransaction,
        space: SpaceId,
        id: Uuid,
        current: i32,
    ) -> Result<usize> {
        if current <= RETAIN_REVISIONS {
            return Ok(0);
        }
        if crate::entity::secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(tx)
            .await?
            .is_some()
        {
            return Ok(0);
        }
        let pinned = memory_links::Entity::find()
            .filter(memory_links::Column::SpaceId.eq(space.bytes()))
            .filter(memory_links::Column::TargetId.eq(id.as_bytes().to_vec()))
            .filter(memory_links::Column::TargetRevision.is_not_null())
            .select_only()
            .column(memory_links::Column::TargetRevision)
            .into_tuple::<Option<i32>>()
            .all(tx)
            .await?
            .into_iter()
            .flatten()
            .collect::<HashSet<_>>();
        let revisions = memory_revisions::Entity::find()
            .filter(memory_revisions::Column::SpaceId.eq(space.bytes()))
            .filter(memory_revisions::Column::MemoryId.eq(id.as_bytes().to_vec()))
            .filter(memory_revisions::Column::Revision.lte(current - RETAIN_REVISIONS))
            .filter(memory_revisions::Column::Content.is_not_null())
            .select_only()
            .column(memory_revisions::Column::Revision)
            .into_tuple::<i32>()
            .all(tx)
            .await?
            .into_iter()
            .filter(|r| !pinned.contains(r))
            .collect::<Vec<_>>();
        for batch in revisions.chunks(64) {
            memory_sources::Entity::delete_many()
                .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                .filter(memory_sources::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .filter(memory_sources::Column::Revision.is_in(batch.iter().copied()))
                .exec(tx)
                .await?;
            memory_links::Entity::delete_many()
                .filter(memory_links::Column::SpaceId.eq(space.bytes()))
                .filter(memory_links::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .filter(memory_links::Column::Revision.is_in(batch.iter().copied()))
                .exec(tx)
                .await?;
            memory_revisions::Entity::update_many()
                .col_expr(
                    memory_revisions::Column::Content,
                    Expr::value(None::<String>),
                )
                .col_expr(
                    memory_revisions::Column::ChangeReason,
                    Expr::case(
                        Expr::col(memory_revisions::Column::ChangeReason).like("后台 LLM%"),
                        "后台 LLM 整理（历史正文已清理）",
                    )
                    .finally(None::<String>)
                    .into(),
                )
                .filter(memory_revisions::Column::SpaceId.eq(space.bytes()))
                .filter(memory_revisions::Column::MemoryId.eq(id.as_bytes().to_vec()))
                .filter(memory_revisions::Column::Revision.is_in(batch.iter().copied()))
                .exec(tx)
                .await?;
        }
        Ok(revisions.len())
    }

    /// 每次至多压缩每空间 100 个终态结果；不触碰在途输入、待重新核对建议或当前记忆。
    pub async fn compact_history(&self) -> Result<usize> {
        let spaces = spaces::Entity::find().all(&self.db).await?;
        let mut count = 0;
        for entry in spaces {
            let space = SpaceId(uuid_from_bytes(&entry.id)?);
            let tx = self.begin(space).await?;
            // 存量会话即使不再改名也收敛；每轮最多处理32个会话，每会话64个旧标题。
            let conversations = titles::Entity::find()
                .filter(titles::Column::SpaceId.eq(space.bytes()))
                .select_only()
                .column(titles::Column::ConversationKey)
                .group_by(titles::Column::ConversationKey)
                .having(Expr::col(titles::Column::Revision).count().gt(10))
                .order_by_asc(titles::Column::ConversationKey)
                .limit(32)
                .into_tuple::<Vec<u8>>()
                .all(&tx)
                .await?;
            for key in conversations {
                let old = titles::Entity::find()
                    .filter(titles::Column::SpaceId.eq(space.bytes()))
                    .filter(titles::Column::ConversationKey.eq(key.clone()))
                    .select_only()
                    .column(titles::Column::Revision)
                    .order_by_desc(titles::Column::Revision)
                    .offset(10)
                    .limit(64)
                    .into_tuple::<i64>()
                    .all(&tx)
                    .await?;
                if !old.is_empty() {
                    let result = titles::Entity::delete_many()
                        .filter(titles::Column::SpaceId.eq(space.bytes()))
                        .filter(titles::Column::ConversationKey.eq(key))
                        .filter(titles::Column::Revision.is_in(old))
                        .exec(&tx)
                        .await?;
                    count += result.rows_affected as usize;
                }
            }
            let pinned = Query::select()
                .expr(Expr::val(1))
                .from(memory_links::Entity)
                .and_where(
                    Expr::col((memory_links::Entity, memory_links::Column::SpaceId))
                        .equals((memory_revisions::Entity, memory_revisions::Column::SpaceId)),
                )
                .and_where(
                    Expr::col((memory_links::Entity, memory_links::Column::TargetId))
                        .equals((memory_revisions::Entity, memory_revisions::Column::MemoryId)),
                )
                .and_where(
                    Expr::col((memory_links::Entity, memory_links::Column::TargetRevision))
                        .equals((memory_revisions::Entity, memory_revisions::Column::Revision)),
                )
                .to_owned();
            let bodies = Query::select()
                .expr(Expr::val(1))
                .from(memory_revisions::Entity)
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::SpaceId))
                        .equals((memories::Entity, memories::Column::SpaceId)),
                )
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::MemoryId))
                        .equals((memories::Entity, memories::Column::Id)),
                )
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::Revision)).lte(
                        Expr::col((memories::Entity, memories::Column::CurrentRevision))
                            .sub(RETAIN_REVISIONS),
                    ),
                )
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::Content))
                        .is_not_null(),
                )
                .and_where(Expr::exists(pinned).not())
                .to_owned();
            let heads = super::organization::head_types(
                &["note", "topic", "topic_alias"],
                Some(space),
                None,
            )
            .filter(Expr::exists(bodies))
            .order_by_asc(memories::Column::Id)
            .limit(32)
            .all(&tx)
            .await?;
            for head in heads {
                count += Self::compact_memory_history(
                    &tx,
                    space,
                    uuid_from_bytes(&head.id)?,
                    head.current_revision,
                )
                .await?;
            }
            let keep = jobs::Entity::find()
                .filter(jobs::Column::SpaceId.eq(space.bytes()))
                .filter(jobs::Column::State.is_in(["done", "failed", "stale", "cancelled"]))
                .filter(jobs::Column::CreatedAt.gte(now() - time::Duration::days(RETAIN_JOB_DAYS)))
                .select_only()
                .column(jobs::Column::Id)
                .order_by_desc(jobs::Column::CreatedAt)
                .order_by_desc(jobs::Column::Id)
                .limit(RETAIN_JOB_DETAILS)
                .into_tuple::<Vec<u8>>()
                .all(&tx)
                .await?;
            let mut query = jobs::Entity::find().filter(jobs::Column::SpaceId.eq(space.bytes()))
                .filter(jobs::Column::State.is_in(["done", "failed", "stale", "cancelled"]))
                .filter(Expr::cust("JSON_EXTRACT(result, '$.detail_expired') IS NULL"))
                .filter(Expr::cust("COALESCE(JSON_EXTRACT(result, '$.pending_revisions'), 0) = 0"))
                .filter(Expr::cust("JSON_SEARCH(JSON_EXTRACT(result, '$.decisions[*].resolution'), 'one', 'rechecking') IS NULL"));
            if !keep.is_empty() {
                query = query.filter(jobs::Column::Id.is_not_in(keep));
            }
            let rows = query
                .order_by_asc(jobs::Column::CreatedAt)
                .limit(100)
                .all(&tx)
                .await?;
            for row in rows {
                let view = Self::job_view(&row);
                let old = row.result.clone().unwrap_or_default();
                let summary = json!({"detail_expired":true,"has_review":false,"pending_revisions":0,
                    "request_hash":old["request_hash"],"counts":view["result"]["counts"],
                    "memory_count":old["memory_ids"].as_array().map_or(0, Vec::len),
                    "topic_count":old["topic_changes"].as_array().map_or(0, Vec::len),
                    "merge_count":old["merged_topics"].as_array().map_or(0, Vec::len),
                    "error":old["error"],"diagnostic":old["diagnostic"]});
                let mut active = row.into_active_model();
                active.result = Set(Some(summary));
                active.payload = Set(None);
                active.update(&tx).await?;
                count += 1;
            }
            tx.commit().await?;
        }
        Ok(count)
    }
}
