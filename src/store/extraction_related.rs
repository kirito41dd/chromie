//! 候选按主题读取当前普通记忆；后台内部召回不计入Agent/Web热度，索引不可用仍有有界词面检索。
use super::knowledge_jobs::{MemorySnapshot, Snapshot};
use super::knowledge_worker::KnowledgeAdmission;
use super::*;
use crate::{extraction::Candidate, retrieval_policy::KeywordQuery};
use sea_orm::QueryOrder;
use std::collections::HashMap;
impl Store {
    pub(super) async fn extraction_related(
        &self,
        space: SpaceId,
        candidates: &[Candidate],
        admission: &KnowledgeAdmission<'_>,
        snapshot: &Snapshot,
    ) -> Result<(Vec<MemorySnapshot>, bool)> {
        let rows = super::organization::heads("note", Some(space), None)
            .order_by_desc(memories::Column::UpdatedAt)
            .limit(501)
            .all(&self.db)
            .await?;
        let mut partial = rows.len() > 500;
        let rows = rows.into_iter().take(500).collect::<Vec<_>>();
        let mut ranked: HashMap<Uuid, (f64, MemorySnapshot)> = HashMap::new();
        let mut cache: HashMap<Uuid, MemorySnapshot> = HashMap::new();
        for row in rows {
            let revision = Self::revision(&self.db, &row).await?;
            if let Some(content) = revision.content {
                let id = uuid_from_bytes(&row.id)?;
                cache.insert(
                    id,
                    MemorySnapshot {
                        id,
                        revision: revision.revision,
                        content,
                        memory_type: crate::knowledge::MemoryType::Note,
                        project_key: revision.project_key,
                        links: Self::links(&self.db, space, id, revision.revision).await?,
                    },
                );
            }
        }
        for candidate in candidates {
            self.validate_knowledge_admission(admission).await?;
            let recall = self
                .recall_for_knowledge(
                    space,
                    crate::types::Recall {
                        query: candidate.search_query.clone(),
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
            // 索引命中的旧条目也读取，不受最近500条兜底窗口限制。
            for id in scores.keys() {
                if cache.contains_key(id) {
                    continue;
                }
                let head = Self::head(&self.db, space, *id).await?;
                if head.forgotten_at.is_some()
                    || crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
                        .one(&self.db)
                        .await?
                        .is_some()
                {
                    continue;
                }
                let revision = Self::revision(&self.db, &head).await?;
                if revision.memory_type != "note" {
                    continue;
                }
                if let Some(content) = revision.content {
                    cache.insert(
                        *id,
                        MemorySnapshot {
                            id: *id,
                            revision: revision.revision,
                            content,
                            memory_type: crate::knowledge::MemoryType::Note,
                            project_key: revision.project_key,
                            links: Self::links(&self.db, space, *id, revision.revision).await?,
                        },
                    );
                }
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
                    local.push((score, id, memory.clone()));
                }
            }
            local.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            for (score, id, memory) in local.into_iter().take(4) {
                let entry = ranked.entry(id).or_insert((0.0, memory));
                entry.0 += score;
            }
        }
        let mut values = ranked
            .into_iter()
            .map(|(id, (score, memory))| (score, id, memory))
            .collect::<Vec<_>>();
        values.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let base = serde_json::to_vec(snapshot)
            .map_err(|_| Error::unavailable())?
            .len();
        let mut bytes = base;
        let mut selected = vec![];
        for (_, _, memory) in values {
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
}
