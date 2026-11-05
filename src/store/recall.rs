use super::{read::budget, *};
use crate::{
    search::{CANDIDATE_LIMIT, TEXT_FIELD},
    types::{Recall, bounded, output_budget},
};
use sea_orm::QueryOrder;
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

const FALLBACK_LIMIT: usize = 500;
const RESULT_LIMIT: usize = 10;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum MatchPath {
    QdrantBm25,
    QdrantVector,
    Hybrid,
    PendingKeyword,
    RecentKeyword,
}

struct Candidate {
    id: Uuid,
    path: MatchPath,
    score: Option<f32>,
    indexed_revision: Option<i32>,
    semantic_score: Option<f32>,
    fusion_score: Option<f64>,
}

#[derive(Clone, Serialize)]
struct HitDebug {
    memory_id: Uuid,
    path: MatchPath,
    bm25_score: Option<f32>,
    indexed_revision: Option<i32>,
    score_matches_revision: Option<bool>,
    semantic_score: Option<f32>,
    fusion_score: Option<f64>,
}

impl Candidate {
    fn debug(&self, current_revision: Option<i64>) -> HitDebug {
        let matches = self
            .indexed_revision
            .map(|revision| Some(i64::from(revision)) == current_revision);
        HitDebug {
            memory_id: self.id,
            path: self.path,
            // 分数只适用于参与 BM25 的索引修订；读取期间更正后不贴到新正文上。
            bm25_score: self
                .score
                .filter(|score| score.is_finite() && matches == Some(true)),
            indexed_revision: self.indexed_revision,
            score_matches_revision: matches,
            semantic_score: self
                .semantic_score
                .filter(|score| score.is_finite() && matches == Some(true)),
            fusion_score: self.fusion_score.filter(|_| matches != Some(false)),
        }
    }
}

impl Store {
    pub async fn recall(&self, space: SpaceId, input: Recall) -> Result<Value> {
        self.recall_inner(space, input, false).await
    }

    pub(crate) async fn recall_debug(&self, space: SpaceId, input: Recall) -> Result<Value> {
        self.recall_inner(space, input, true).await
    }

    // 普通 MCP 和 Web 诊断共用候选选择与预算，诊断只改变额外的元数据。
    async fn recall_inner(&self, space: SpaceId, input: Recall, with_debug: bool) -> Result<Value> {
        let started = Instant::now();
        bounded(&input.query, 256)?;
        let maximum = output_budget(input.max_chars)?;
        if let Some(project) = &input.project_key {
            bounded(project, 128)?;
        }
        // 先拍下待同步集合，避免索引在查询过程中完成导致兜底遗漏。
        let mut pending = outbox_tasks::Entity::find()
            .filter(outbox_tasks::Column::SpaceId.eq(space.bytes()))
            .filter(outbox_tasks::Column::State.eq("pending"))
            .order_by_asc(outbox_tasks::Column::UpdatedAt)
            .limit((FALLBACK_LIMIT + 1) as u64)
            .all(&self.db)
            .await?;
        let pending_limited = pending.len() > FALLBACK_LIMIT;
        let mut partial = pending_limited;
        pending.truncate(FALLBACK_LIMIT);
        let index_started = Instant::now();
        let (search, semantic) = tokio::join!(
            self.search
                .query_report(space, &input.query, input.project_key.as_deref()),
            self.embedding_candidates(space, &input.query, input.project_key.as_deref())
        );
        let (semantic_hits, mut embedding_debug, semantic_filter) =
            semantic.unwrap_or_else(|error| {
                (
                    vec![],
                    json!({"state":"unavailable","reason":error.code}),
                    None,
                )
            });
        let semantic_available = embedding_debug["state"] == "ready";
        let index_call_ms = index_started.elapsed().as_secs_f64() * 1000.0;
        let available = search.is_ok();
        let mut candidates = Vec::new();
        let mut index_candidates = 0;
        let mut index_validated = 0;
        let mut server_ms = None;
        if let Ok(report) = search {
            index_candidates = report.candidate_count;
            server_ms = report.server_ms;
            for hit in report.hits {
                match Self::head(&self.db, space, hit.id).await {
                    Ok(head)
                        if head.forgotten_at.is_none() && head.current_revision == hit.revision =>
                    {
                        index_validated += 1;
                        candidates.push(Candidate {
                            id: hit.id,
                            path: MatchPath::QdrantBm25,
                            score: Some(hit.score),
                            indexed_revision: Some(hit.revision),
                            semantic_score: None,
                            fusion_score: None,
                        });
                    }
                    Ok(_) => {}
                    Err(error) if error.code == "not_found" => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let fallback: Vec<Uuid> = if available {
            pending
                .into_iter()
                .map(|row| uuid_from_bytes(&row.memory_id))
                .collect::<Result<_>>()?
        } else {
            partial = true;
            memories::Entity::find()
                .filter(memories::Column::SpaceId.eq(space.bytes()))
                .filter(memories::Column::ForgottenAt.is_null())
                .order_by_desc(memories::Column::UpdatedAt)
                .limit(FALLBACK_LIMIT as u64)
                .all(&self.db)
                .await?
                .into_iter()
                .map(|row| uuid_from_bytes(&row.id))
                .collect::<Result<_>>()?
        };
        let terms = input
            .query
            .split_whitespace()
            .take(8)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let fallback_examined = fallback.len();
        let fallback_path = if available {
            MatchPath::PendingKeyword
        } else {
            MatchPath::RecentKeyword
        };
        let mut fallback_matches = 0;
        for id in fallback {
            let head = Self::head(&self.db, space, id).await?;
            if head.forgotten_at.is_some() {
                continue;
            }
            let row = Self::revision(&self.db, &head).await?;
            let content = row.content.unwrap_or_default().to_lowercase();
            if terms.iter().any(|term| content.contains(term)) {
                fallback_matches += 1;
                candidates.push(Candidate {
                    id,
                    path: fallback_path,
                    score: None,
                    indexed_revision: None,
                    semantic_score: None,
                    fusion_score: None,
                });
            }
        }
        let mut valid_semantic = Vec::new();
        for hit in semantic_hits {
            match Self::head(&self.db, space, hit.id).await {
                Ok(head)
                    if head.forgotten_at.is_none() && head.current_revision == hit.revision =>
                {
                    valid_semantic.push(Candidate {
                        id: hit.id,
                        path: MatchPath::QdrantVector,
                        score: None,
                        indexed_revision: Some(hit.revision),
                        semantic_score: Some(hit.score),
                        fusion_score: None,
                    })
                }
                Ok(_) => {}
                Err(error) if error.code == "not_found" => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(filter) = semantic_filter {
            let report = self
                .filter_recall_candidates(
                    space,
                    &input,
                    &filter,
                    &mut candidates,
                    &mut valid_semantic,
                )
                .await?;
            if report["score_lookup_available"] == false {
                partial = true;
            }
            embedding_debug["filter"] = report;
        }
        if !valid_semantic.is_empty() {
            candidates = fuse(candidates, valid_semantic);
        }
        let mut seen = HashSet::new();
        let mut cards = Vec::new();
        let mut details = HashMap::new();
        let mut duplicates = 0;
        let mut project_filtered = 0;
        let mut missing_or_hidden = 0;
        for candidate in candidates {
            let id = candidate.id;
            if !seen.insert(id) {
                duplicates += 1;
                continue;
            }
            let card = match Self::card(&self.db, space, id, None).await {
                Ok(card) => card,
                Err(error) if error.code == "not_found" => {
                    missing_or_hidden += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if candidate.semantic_score.is_some()
                && candidate.indexed_revision.map(i64::from)
                    != card.get("revision").and_then(Value::as_i64)
            {
                continue;
            }
            if let Some(project) = &input.project_key
                && let Some(actual) = card.get("project_key").and_then(Value::as_str)
                && actual != project
            {
                project_filtered += 1;
                continue;
            }
            if with_debug {
                details.insert(
                    id.to_string(),
                    candidate.debug(card.get("revision").and_then(Value::as_i64)),
                );
            }
            cards.push(card);
        }
        let before_limit = cards.len();
        if cards.len() > RESULT_LIMIT {
            partial = true;
            cards.truncate(RESULT_LIMIT);
        }
        let mut result = budget(
            json!({"memories":cards,"index_available":available,"partial":partial,"truncated":false,
                "retrieval_mode":if semantic_available{"hybrid"}else{"keyword"},"semantic_available":semantic_available,
                "semantic_reason":embedding_debug.get("reason"),"retrieval_revision":embedding_debug.get("config_revision"),
                "similarity_threshold":embedding_debug.get("similarity_threshold")}),
            maximum,
        );
        // 只统计最终交付的条目；统计故障不掩盖已经取得的记忆结果。
        match self.record_recall(space, &result, with_debug).await {
            Ok(id) => result["recall_id"] = json!(id),
            Err(error) => {
                tracing::warn!(code = error.code, "召回统计未记录");
                result["recall_id"] = Value::Null;
                result["usage_available"] = json!(false);
            }
        }
        if with_debug {
            let heads = result
                .get("memories")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| {
                    let id = row
                        .get("memory_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())?;
                    let revision = row
                        .get("revision")
                        .and_then(Value::as_i64)
                        .and_then(|v| i32::try_from(v).ok())?;
                    Some((id, revision))
                })
                .collect::<Vec<_>>();
            let usage = match Self::memory_usage(&self.db, space, &heads).await {
                Ok(usage) => usage,
                Err(error) => {
                    tracing::warn!(code = error.code, "召回统计暂不可读");
                    result["usage_available"] = json!(false);
                    HashMap::new()
                }
            };
            // 预算可能继续删掉卡片，只附加最终输出的对象，避免诊断泄露被过滤或截去的 ID。
            let hits = result
                .get("memories")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("memory_id").and_then(Value::as_str))
                .filter_map(|id| details.get(id).map(|hit| (id, hit)))
                .map(|(id, hit)| {
                    // 辅助统计归入诊断，不挤占正文预算或改变已记录的最终结果集。
                    let mut hit = json!(hit);
                    if let Ok(id) = Uuid::parse_str(id)
                        && let Some(usage) = usage.get(&id)
                    {
                        hit["usage"] = json!(usage);
                    }
                    hit
                })
                .collect::<Vec<_>>();
            result["debug"] = json!({
                "embedding":embedding_debug,
                "fusion":{"method":"rrf","k":60},
                "timing":{"total_ms":started.elapsed().as_secs_f64()*1000.0,"index_call_ms":index_call_ms,"qdrant_server_ms":server_ms},
                "qdrant":{"available":available,"field":TEXT_FIELD,"tokenizer":"multilingual","tokens_available":false,
                    "candidate_limit":CANDIDATE_LIMIT,"candidates":index_candidates,"validated":index_validated,
                    "discarded":index_candidates.saturating_sub(index_validated)},
                "fallback":{"mode":fallback_path,"terms":terms,"candidate_limit":FALLBACK_LIMIT,
                    "examined":fallback_examined,"matches":fallback_matches,"pending_snapshot_limited":pending_limited},
                "selection":{"duplicates":duplicates,"project_filtered":project_filtered,"missing_or_hidden":missing_or_hidden,
                    "before_limit":before_limit,"returned":hits.len(),"result_limit":RESULT_LIMIT,"max_chars":maximum},
                "hits":hits
            });
        }
        Ok(result)
    }
}

impl Store {
    /// 分数只绑定产生它的修订。尚无向量／评分失败的关键词候选保留，不能误删新记忆。
    async fn filter_recall_candidates(
        &self,
        space: SpaceId,
        input: &Recall,
        filter: &super::embedding_worker::SemanticFilter,
        keyword: &mut Vec<Candidate>,
        semantic: &mut Vec<Candidate>,
    ) -> Result<Value> {
        let mut scores = semantic
            .iter()
            .filter_map(|row| Some((row.id, (row.semantic_score?, row.indexed_revision?))))
            .collect::<HashMap<_, _>>();
        let ids = keyword
            .iter()
            .filter(|row| {
                matches!(row.path, MatchPath::QdrantBm25) && !scores.contains_key(&row.id)
            })
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let lookup_available = match self
            .search
            .score_vectors(
                space,
                filter.generation,
                filter.vector.clone(),
                &ids,
                input.project_key.as_deref(),
            )
            .await
        {
            Ok(rows) => {
                for row in rows {
                    scores.insert(row.id, (row.score, row.revision));
                }
                true
            }
            Err(_) => false,
        };
        let (mut removed, mut literals, mut unscored) = (0usize, 0usize, 0usize);
        let mut kept = Vec::with_capacity(keyword.len());
        for mut row in keyword.drain(..) {
            if matches!(row.path, MatchPath::QdrantBm25) {
                if let Some((score, revision)) = scores.get(&row.id).copied()
                    && row.indexed_revision == Some(revision)
                {
                    if !filter.threshold.accepts(score) {
                        let head = match Self::head(&self.db, space, row.id).await {
                            Ok(head) => head,
                            Err(error) if error.code == "not_found" => continue,
                            Err(error) => return Err(error),
                        };
                        if head.forgotten_at.is_some() {
                            continue;
                        }
                        if head.current_revision != revision {
                            unscored += 1;
                            kept.push(row);
                            continue;
                        }
                        let current = Self::revision(&self.db, &head).await?;
                        if !crate::retrieval_policy::literal_match(
                            &input.query,
                            current.content.as_deref().unwrap_or_default(),
                        ) {
                            removed += 1;
                            continue;
                        }
                        literals += 1;
                    }
                    row.semantic_score = Some(score);
                } else {
                    unscored += 1;
                }
            }
            kept.push(row);
        }
        *keyword = kept;
        let before = semantic.len();
        semantic.retain(|row| {
            row.semantic_score
                .is_some_and(|score| filter.threshold.accepts(score))
        });
        Ok(
            json!({"method":"candidate_gate","threshold":filter.threshold.value(),
            "filtered_keyword":removed,"filtered_semantic":before-semantic.len(),
            "literal_kept":literals,"unscored_keyword":unscored,"score_lookup_available":lookup_available}),
        )
    }
}

// 按各自名次融合，BM25 分数和余弦相似度不能直接相加。
fn fuse(keyword: Vec<Candidate>, semantic: Vec<Candidate>) -> Vec<Candidate> {
    let mut merged: HashMap<Uuid, Candidate> = HashMap::new();
    for (lane, rows) in [keyword, semantic].into_iter().enumerate() {
        let mut seen = HashSet::new();
        for (rank, mut candidate) in rows.into_iter().filter(|c| seen.insert(c.id)).enumerate() {
            let increment = 1.0 / (61.0 + rank as f64);
            if let Some(previous) = merged.get_mut(&candidate.id) {
                if previous.indexed_revision.is_some()
                    && candidate.indexed_revision.is_some()
                    && previous.indexed_revision != candidate.indexed_revision
                {
                    // 两路在并发更正前后读取到不同修订，旧分数不能转贴到新正文。
                    candidate.fusion_score = Some(increment);
                    *previous = candidate;
                    continue;
                }
                previous.fusion_score = Some(previous.fusion_score.unwrap_or(0.0) + increment);
                if lane == 1 {
                    previous.semantic_score = candidate.semantic_score;
                    previous.indexed_revision = candidate.indexed_revision;
                    previous.path = MatchPath::Hybrid;
                }
            } else {
                candidate.fusion_score = Some(increment);
                merged.insert(candidate.id, candidate);
            }
        }
    }
    let mut rows = merged.into_values().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.fusion_score
            .partial_cmp(&a.fusion_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reciprocal_rank_fusion_rewards_shared_hits_without_mixing_score_scales() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let keyword = |id, score| Candidate {
            id,
            path: MatchPath::QdrantBm25,
            score: Some(score),
            indexed_revision: Some(1),
            semantic_score: None,
            fusion_score: None,
        };
        let semantic = |id, score| Candidate {
            id,
            path: MatchPath::QdrantVector,
            score: None,
            indexed_revision: Some(1),
            semantic_score: Some(score),
            fusion_score: None,
        };
        let result = fuse(
            vec![keyword(a, 1_000_000.0), keyword(b, 0.01)],
            vec![semantic(c, 0.99), semantic(b, 0.5)],
        );
        assert_eq!(result[0].id, b);
        assert_eq!(result[0].debug(Some(1)).bm25_score, Some(0.01));
        assert_eq!(result[0].debug(Some(1)).semantic_score, Some(0.5));
        assert_eq!(result[0].debug(Some(2)).semantic_score, None);
        let mut updated = semantic(a, 0.9);
        updated.indexed_revision = Some(2);
        let result = fuse(vec![keyword(a, 99.0)], vec![updated]);
        assert_eq!(
            result[0].debug(Some(2)).bm25_score,
            None,
            "旧 BM25 分数不能被融合到新修订"
        );
        assert_eq!(result[0].debug(Some(2)).semantic_score, Some(0.9));
    }

    #[test]
    fn a_score_never_moves_from_an_old_index_revision_to_new_content() {
        let candidate = Candidate {
            id: Uuid::now_v7(),
            path: MatchPath::QdrantBm25,
            score: Some(2.5),
            indexed_revision: Some(1),
            semantic_score: None,
            fusion_score: None,
        };
        assert_eq!(candidate.debug(Some(1)).bm25_score, Some(2.5));
        let changed = candidate.debug(Some(2));
        assert_eq!(changed.bm25_score, None);
        assert_eq!(changed.indexed_revision, Some(1));
        assert_eq!(changed.score_matches_revision, Some(false));
        let fallback = Candidate {
            id: candidate.id,
            path: MatchPath::PendingKeyword,
            score: None,
            indexed_revision: None,
            semantic_score: None,
            fusion_score: None,
        };
        assert_eq!(fallback.debug(Some(2)).bm25_score, None);
        assert_eq!(fallback.debug(Some(2)).score_matches_revision, None);
    }
}
