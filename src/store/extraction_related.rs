//! 候选按主题读取当前普通记忆；后台内部召回不计入 Agent/Web 热度。
use super::knowledge_jobs::{MemorySnapshot, Snapshot};
use super::knowledge_worker::KnowledgeAdmission;
use super::*;
use crate::{
    entity::{memory_links, secrets},
    extraction::Candidate,
    retrieval_policy::KeywordQuery,
};
use sea_orm::{
    Condition, ExprTrait, FromQueryResult, JoinType, QueryOrder, RelationTrait, Select,
    sea_query::Expr,
};
use std::collections::HashMap;

#[derive(FromQueryResult)]
struct NoteBody {
    memory_id: Vec<u8>,
    revision: i32,
    content: Option<String>,
    project_key: Option<String>,
}

/// 同一条查询固定空间、有效记忆和当前修订，窗口内与索引补读共享这些约束。
fn current_notes(space: SpaceId) -> Select<memory_revisions::Entity> {
    use memory_revisions::Column as Rev;
    memory_revisions::Entity::find()
        .select_only()
        .columns([Rev::MemoryId, Rev::Revision, Rev::Content, Rev::ProjectKey])
        .join(
            JoinType::InnerJoin,
            memory_revisions::Relation::Memories.def(),
        )
        .join(JoinType::LeftJoin, memories::Relation::Secrets.def())
        .filter(Rev::SpaceId.eq(space.bytes()))
        .filter(
            Expr::col((memory_revisions::Entity, Rev::Revision))
                .equals((memories::Entity, memories::Column::CurrentRevision)),
        )
        .filter(Rev::MemoryType.eq("note"))
        .filter(memories::Column::ForgottenAt.is_null())
        .filter(secrets::Column::MemoryId.is_null())
}

fn cache_bodies(
    cache: &mut HashMap<Uuid, MemorySnapshot>,
    rows: impl IntoIterator<Item = NoteBody>,
) -> Result<()> {
    for row in rows {
        if let Some(content) = row.content {
            let id = uuid_from_bytes(&row.memory_id)?;
            cache.insert(
                id,
                MemorySnapshot {
                    id,
                    revision: row.revision,
                    content,
                    memory_type: crate::knowledge::MemoryType::Note,
                    project_key: row.project_key,
                    links: vec![],
                },
            );
        }
    }
    Ok(())
}

impl Store {
    pub(super) async fn extraction_related(
        &self,
        space: SpaceId,
        candidates: &[Candidate],
        admission: &KnowledgeAdmission<'_>,
        snapshot: &Snapshot,
    ) -> Result<(Vec<MemorySnapshot>, bool)> {
        let rows = current_notes(space)
            .order_by_desc(memories::Column::UpdatedAt)
            .order_by_asc(memories::Column::Id)
            .limit(501)
            .into_model::<NoteBody>()
            .all(&self.db)
            .await?;
        let mut partial = rows.len() > 500;
        let mut ranked: HashMap<Uuid, f64> = HashMap::new();
        let mut cache = HashMap::new();
        cache_bodies(&mut cache, rows.into_iter().take(500))?;
        for candidate in candidates {
            self.validate_knowledge_admission(admission).await?;
            let recall = self
                .recall_for_knowledge(
                    space,
                    crate::types::Recall {
                        query: candidate.search_query.clone(),
                        prefer_secret_refs: false,
                        project_key: None,
                        max_chars: Some(8000),
                    },
                    admission,
                )
                .await?;
            partial |= recall["partial"] == true;
            let mut scores = HashMap::new();
            for (rank, card) in recall["memories"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(id) = card["memory_id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    scores.insert(id, 12.0 / (rank + 1) as f64);
                }
            }
            // 一次补齐本候选所有窗口外的索引命中，不逐条查询。
            let missing = scores
                .keys()
                .filter(|id| !cache.contains_key(id))
                .map(|id| id.as_bytes().to_vec())
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                let rows = current_notes(space)
                    .filter(memory_revisions::Column::MemoryId.is_in(missing))
                    .into_model::<NoteBody>()
                    .all(&self.db)
                    .await?;
                cache_bodies(&mut cache, rows)?;
            }
            let query = KeywordQuery::new(&candidate.search_query);
            let terms = candidate
                .search_query
                .split_whitespace()
                .map(str::to_lowercase)
                .filter(|t| t.chars().count() >= 2)
                .collect::<Vec<_>>();
            let mut local = vec![];
            for (id, memory) in &cache {
                let id = *id;
                let content = &memory.content;
                let lower = content.to_lowercase();
                let matched = terms.iter().filter(|t| lower.contains(t.as_str())).count();
                let score = scores.get(&id).copied().unwrap_or(0.0)
                    + if query.matches(content).is_some() {
                        15.0
                    } else if matched >= 2 || terms.len() == 1 && matched == 1 {
                        matched as f64
                    } else {
                        0.0
                    };
                if score > 0.0 {
                    local.push((score, id));
                }
            }
            local.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            for (score, id) in local.into_iter().take(4) {
                *ranked.entry(id).or_default() += score;
            }
        }
        let mut ranked: Vec<_> = ranked.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut shortlisted = Vec::with_capacity(ranked.len());
        for (id, _) in ranked {
            shortlisted.push(cache.remove(&id).ok_or_else(Error::unavailable)?);
        }
        if !shortlisted.is_empty() {
            let mut versions = Condition::any();
            for memory in &shortlisted {
                versions = versions.add(
                    Condition::all()
                        .add(memory_links::Column::MemoryId.eq(memory.id.as_bytes().to_vec()))
                        .add(memory_links::Column::Revision.eq(memory.revision)),
                );
            }
            let rows = memory_links::Entity::find()
                .filter(memory_links::Column::SpaceId.eq(space.bytes()))
                .filter(versions)
                .order_by_asc(memory_links::Column::TargetId)
                .order_by_asc(memory_links::Column::Relation)
                .all(&self.db)
                .await?;
            let mut links = HashMap::<Uuid, Vec<_>>::new();
            for row in rows {
                links
                    .entry(uuid_from_bytes(&row.memory_id)?)
                    .or_default()
                    .push(Self::decode_link(row)?);
            }
            for memory in &mut shortlisted {
                memory.links = links.remove(&memory.id).unwrap_or_default();
            }
        }
        // 带完整关系后再计算大小，超预算项之后的较短记忆仍可入选。
        let base = serde_json::to_vec(snapshot)
            .map_err(|_| Error::unavailable())?
            .len();
        select_within_budget(shortlisted, base, partial)
    }
}

fn select_within_budget(
    shortlisted: Vec<MemorySnapshot>,
    mut bytes: usize,
    mut partial: bool,
) -> Result<(Vec<MemorySnapshot>, bool)> {
    let mut selected = vec![];
    for memory in shortlisted {
        let size = serde_json::to_vec(&memory)
            .map_err(|_| Error::unavailable())?
            .len();
        if selected.len() == 15 || bytes.saturating_add(size) > 240 * 1024 {
            partial = true;
            continue;
        }
        bytes += size;
        selected.push(memory);
    }
    Ok((selected, partial))
}

#[cfg(test)]
mod tests;
