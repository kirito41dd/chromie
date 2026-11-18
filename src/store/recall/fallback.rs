//! 仅用当前修订的公开正文匹配，按待同步快照或最近更新窗口的顺序返回。
use super::*;
use sea_orm::{ExprTrait, FromQueryResult, JoinType, RelationTrait, sea_query::Expr};

#[derive(FromQueryResult)]
struct Body {
    memory_id: Vec<u8>,
    revision: i32,
    content: Option<String>,
}

pub(super) fn matches(terms: &[String], content: &str) -> bool {
    let normalized = content.to_lowercase();
    terms.iter().any(|term| normalized.contains(term))
}

pub(super) async fn candidates(
    db: &DatabaseConnection,
    space: SpaceId,
    pending: Vec<Vec<u8>>,
    path: MatchPath,
    terms: &[String],
) -> Result<(usize, Vec<Candidate>)> {
    use memory_revisions::Column as Rev;
    let current = memory_revisions::Entity::find()
        .select_only()
        .columns([Rev::MemoryId, Rev::Revision, Rev::Content])
        .join(
            JoinType::InnerJoin,
            memory_revisions::Relation::Memories.def(),
        )
        .filter(Rev::SpaceId.eq(space.bytes()))
        .filter(
            Expr::col((memory_revisions::Entity, Rev::Revision))
                .equals((memories::Entity, memories::Column::CurrentRevision)),
        )
        .filter(memories::Column::ForgottenAt.is_null());

    let (examined, rows) = if matches!(path, MatchPath::PendingKeyword) {
        if pending.is_empty() {
            return Ok((0, Vec::new()));
        }
        let rows = current
            .filter(Rev::MemoryId.is_in(pending.iter().cloned()))
            .into_model::<Body>()
            .all(db)
            .await?;
        let mut by_id: HashMap<_, _> = rows
            .into_iter()
            .map(|body| (body.memory_id.clone(), body))
            .collect();
        let examined = pending.len();
        (
            examined,
            pending
                .into_iter()
                .filter_map(|id| by_id.remove(&id))
                .collect(),
        )
    } else {
        let rows = current
            .order_by_desc(memories::Column::UpdatedAt)
            .order_by_asc(memories::Column::Id)
            .limit(FALLBACK_LIMIT as u64)
            .into_model::<Body>()
            .all(db)
            .await?;
        (rows.len(), rows)
    };
    let hits = rows
        .into_iter()
        .filter(|row| matches(terms, row.content.as_deref().unwrap_or_default()))
        .map(|row| {
            Ok(Candidate {
                id: uuid_from_bytes(&row.memory_id)?,
                path,
                revision: row.revision,
                score: None,
                semantic_score: None,
                fusion_score: None,
                keyword_match: None,
            })
        })
        .collect::<Result<_>>()?;
    Ok((examined, hits))
}
