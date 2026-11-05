#![cfg_attr(not(test), forbid(unsafe_code))]

use serde::Deserialize;

pub fn parse_optional<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Diagnostics>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    // 诊断的版本或格式不兼容时仍显示记忆结果，避免辅助信息阻断召回。
    Ok(serde_json::from_value(value).ok())
}

#[derive(Deserialize)]
pub struct Diagnostics {
    #[serde(default)]
    embedding: Option<Embedding>,
    timing: Timing,
    qdrant: Index,
    fallback: Fallback,
    selection: Selection,
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Embedding {
    #[serde(default)]
    candidates: usize,
    state: String,
    model: Option<String>,
    generation_id: Option<String>,
    elapsed_ms: Option<f64>,
    reason: Option<String>,
    #[serde(default)]
    filter: Option<CandidateFilter>,
}

#[derive(Deserialize)]
struct CandidateFilter {
    method: String,
    threshold: f64,
    filtered_keyword: usize,
    filtered_semantic: usize,
    literal_kept: usize,
}

#[derive(Deserialize)]
struct Timing {
    total_ms: f64,
    index_call_ms: f64,
    qdrant_server_ms: Option<f64>,
}

#[derive(Deserialize)]
struct Index {
    available: bool,
    field: String,
    tokenizer: String,
    candidates: usize,
    candidate_limit: usize,
    validated: usize,
    discarded: usize,
}

#[derive(Deserialize)]
struct Fallback {
    mode: String,
    terms: Vec<String>,
    examined: usize,
    matches: usize,
    candidate_limit: usize,
    pending_snapshot_limited: bool,
}

#[derive(Deserialize)]
struct Selection {
    duplicates: usize,
    project_filtered: usize,
    missing_or_hidden: usize,
    before_limit: usize,
    returned: usize,
    result_limit: usize,
    max_chars: usize,
}

#[derive(Deserialize)]
struct Hit {
    #[serde(default)]
    semantic_score: Option<f64>,
    #[serde(default)]
    fusion_score: Option<f64>,
    memory_id: String,
    #[serde(default)]
    usage: Option<crate::memory::Usage>,
    path: String,
    bm25_score: Option<f64>,
    indexed_revision: Option<i32>,
    score_matches_revision: Option<bool>,
}

fn path_label(path: &str) -> &str {
    match path {
        "qdrant_bm25" => "Qdrant BM25",
        "qdrant_vector" => "语义召回",
        "hybrid" => "关键词与语义同时命中",
        "pending_keyword" => "待同步记忆关键词兜底",
        "recent_keyword" => "索引故障：最近记忆关键词兜底",
        _ => "未知检索路径",
    }
}

fn milliseconds(value: Option<f64>) -> String {
    value
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map(|ms| format!("{ms:.2} ms"))
        .unwrap_or_else(|| "未提供".into())
}

impl Diagnostics {
    pub fn usage(&self, memory_id: &str) -> Option<&crate::memory::Usage> {
        self.hits
            .iter()
            .take(10)
            .find(|hit| hit.memory_id == memory_id)?
            .usage
            .as_ref()
    }

    pub fn overview(&self) -> crate::RecallOverview {
        crate::RecallOverview {
            has_diagnostics: true,
            index_available: self.qdrant.available,
            total_time: milliseconds(Some(self.timing.total_ms)).into(),
            index_time: milliseconds(Some(self.timing.index_call_ms)).into(),
            qdrant_time: milliseconds(self.timing.qdrant_server_ms).into(),
            index_config: format!(
                "{} · {}{}",
                self.qdrant.field,
                self.qdrant.tokenizer,
                self.embedding
                    .as_ref()
                    .map(|e| format!(
                        "\n语义 {} · {} · {} · {}{}",
                        e.state,
                        e.model.as_deref().unwrap_or("未配置"),
                        e.generation_id.as_deref().unwrap_or("无代际"),
                        milliseconds(e.elapsed_ms),
                        e.reason
                            .as_ref()
                            .map(|r| format!(" · {r}"))
                            .unwrap_or_default()
                    ))
                    .unwrap_or_default()
            )
            .into(),
            fallback_terms: serde_json::to_string(&self.fallback.terms)
                .unwrap_or_default()
                .into(),
            ..Default::default()
        }
    }

    pub fn stages(&self) -> Vec<crate::RecallStage> {
        let mut index_note = if self.qdrant.available {
            format!(
                "校验通过 {} · 丢弃 {} · 候选上限 {}",
                self.qdrant.validated, self.qdrant.discarded, self.qdrant.candidate_limit
            )
        } else {
            "索引不可用，本次使用有限关键词检索。".into()
        };
        if let Some(semantic) = &self.embedding {
            index_note.push_str(&match semantic.state.as_str() {
                "ready" => format!(
                    "\n语义候选 {} · {} · {}",
                    semantic.candidates,
                    semantic.model.as_deref().unwrap_or("模型"),
                    milliseconds(semantic.elapsed_ms)
                ),
                "building" => "\n首次向量索引准备中，本次使用关键词。".into(),
                "empty" => "\n当前空间无有效记忆，本次未调用向量模型。".into(),
                "unavailable" if semantic.reason.as_deref() == Some("model_busy") => {
                    "\n模型并发等待超时，本次使用关键词检索。".into()
                }
                "unavailable" => "\n语义服务暂不可用，本次已回退关键词。".into(),
                _ => "\n未启用向量模型，本次使用关键词。".into(),
            });
        }
        let fallback_range = if self.fallback.mode == "recent_keyword" {
            "最近记忆"
        } else {
            "待同步记忆"
        };
        let mut fallback_note = format!(
            "{} · 检查 {} / {}",
            fallback_range, self.fallback.examined, self.fallback.candidate_limit
        );
        if self.fallback.pending_snapshot_limited {
            fallback_note.push_str(" · 待同步快照达到上限");
        }
        let mut selection_note = format!(
            "去重 {} · 项目过滤 {} · 读取时失效 {}",
            self.selection.duplicates,
            self.selection.project_filtered,
            self.selection.missing_or_hidden
        );
        if let Some(filter) = self.embedding.as_ref().and_then(|e| e.filter.as_ref())
            && filter.method == "candidate_gate"
            && filter.threshold.is_finite()
        {
            selection_note.push_str(&format!(
                "\n候选余弦下限 {} · 过滤关键词 {} / 语义 {} · 精确匹配保留 {}",
                filter.threshold,
                filter.filtered_keyword,
                filter.filtered_semantic,
                filter.literal_kept
            ));
        }
        vec![
            crate::RecallStage {
                number: "01".into(),
                title: if self.embedding.as_ref().is_some_and(|e| e.state == "ready") {
                    "双路召回"
                } else {
                    "BM25 候选"
                }
                .into(),
                value: if self.qdrant.available {
                    self.qdrant.candidates.to_string().into()
                } else {
                    "未完成".into()
                },
                note: index_note.into(),
                warning: !self.qdrant.available,
            },
            crate::RecallStage {
                number: "02".into(),
                title: "关键词补充".into(),
                value: self.fallback.matches.to_string().into(),
                note: fallback_note.into(),
                warning: self.fallback.mode == "recent_keyword"
                    || self.fallback.pending_snapshot_limited,
            },
            crate::RecallStage {
                number: "03".into(),
                title: if self.embedding.as_ref().is_some_and(|e| e.state == "ready") {
                    "RRF 融合与过滤"
                } else {
                    "合并与过滤"
                }
                .into(),
                value: self.selection.before_limit.to_string().into(),
                note: selection_note.into(),
                warning: false,
            },
            crate::RecallStage {
                number: "04".into(),
                title: "本次输出".into(),
                value: self.selection.returned.to_string().into(),
                note: format!(
                    "最多 {} 条 · 记忆预算 {} 字符",
                    self.selection.result_limit, self.selection.max_chars
                )
                .into(),
                warning: false,
            },
        ]
    }

    pub fn pending_visible(&self) -> usize {
        self.hits
            .iter()
            .take(10)
            .filter(|hit| hit.path == "pending_keyword")
            .count()
    }

    pub fn fallback_limit(&self) -> usize {
        self.fallback.candidate_limit
    }

    pub fn hit_labels(&self, memory_id: &str) -> (String, String) {
        let Some(hit) = self
            .hits
            .iter()
            .take(10)
            .find(|hit| hit.memory_id == memory_id)
        else {
            return ("服务端未提供命中路径。".into(), "未提供分数。".into());
        };
        let path = path_label(&hit.path).to_owned();
        let mut score = if let Some(score) = hit
            .bm25_score
            .filter(|score| score.is_finite() && hit.score_matches_revision == Some(true))
        {
            format!(
                "BM25 分数 {score:.6} · 索引修订 {}；仅用于本次排序。",
                hit.indexed_revision
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "未提供".into())
            )
        } else if hit.score_matches_revision == Some(false) {
            "正文修订已变更，旧索引分数不适用于当前内容。".into()
        } else if hit.path == "pending_keyword" || hit.path == "recent_keyword" {
            "关键词兜底未计算 BM25 分数。".into()
        } else {
            "未提供适用的 BM25 分数。".into()
        };
        if let Some(value) = hit.semantic_score.filter(|v| v.is_finite()) {
            score.push_str(&format!("\n语义相似度 {value:.6}，不代表准确率。"));
        }
        if let Some(value) = hit.fusion_score.filter(|v| v.is_finite()) {
            score.push_str(&format!("\nRRF 融合分数 {value:.6}。"));
        }
        (path, score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_distinguish_fallback_scores_and_unavailable_server_timing() {
        let mut body = serde_json::json!({
            "timing":{"total_ms":24.0,"index_call_ms":3.0,"qdrant_server_ms":null},
            "qdrant":{"available":false,"field":"content","tokenizer":"multilingual","candidates":0,"candidate_limit":30,"validated":0,"discarded":0},
            "fallback":{"mode":"recent_keyword","terms":["虚构查询"],"examined":2,"matches":1,"candidate_limit":500,"pending_snapshot_limited":false},
            "selection":{"duplicates":0,"project_filtered":0,"missing_or_hidden":0,"before_limit":1,"returned":1,"result_limit":10,"max_chars":8000},
            "hits":[{"memory_id":"虚构记忆 ID","path":"recent_keyword","bm25_score":null,"indexed_revision":null,"score_matches_revision":null}]
        });
        let diagnostic: Diagnostics = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(diagnostic.overview().qdrant_time.as_str(), "未提供");
        let stages = diagnostic.stages();
        assert_eq!(stages.len(), 4);
        assert_eq!(stages[0].value.as_str(), "未完成");
        assert!(stages[0].warning);
        assert!(stages[1].note.contains("最近记忆"));
        assert!(diagnostic.hit_labels("虚构记忆 ID").0.contains("索引故障"));
        assert!(
            diagnostic
                .hit_labels("虚构记忆 ID")
                .1
                .contains("未计算 BM25")
        );
        body["hits"][0] = serde_json::json!({"memory_id":"虚构记忆 ID","path":"qdrant_bm25","bm25_score":2.5,"indexed_revision":1,"score_matches_revision":true});
        let diagnostic: Diagnostics = serde_json::from_value(body.clone()).unwrap();
        assert!(diagnostic.hit_labels("虚构记忆 ID").1.contains("2.500000"));
        body["hits"][0]["score_matches_revision"] = serde_json::json!(false);
        let diagnostic: Diagnostics = serde_json::from_value(body).unwrap();
        assert!(!diagnostic.hit_labels("虚构记忆 ID").1.contains("2.500000"));
        assert!(
            diagnostic
                .hit_labels("虚构记忆 ID")
                .1
                .contains("正文修订已变更")
        );
        assert!(diagnostic.hit_labels("其他记忆 ID").0.contains("未提供"));
    }
    #[test]
    fn candidate_gate_counts_explain_why_retrieved_candidates_disappear() {
        let body = serde_json::json!({
            "embedding":{"state":"ready","candidates":29,"filter":{"method":"candidate_gate","threshold":0.56,"filtered_keyword":19,"filtered_semantic":29,"literal_kept":0}},
            "timing":{"total_ms":24.0,"index_call_ms":3.0,"qdrant_server_ms":2.0},
            "qdrant":{"available":true,"field":"content","tokenizer":"multilingual","candidates":19,"candidate_limit":30,"validated":19,"discarded":0},
            "fallback":{"mode":"pending_keyword","terms":["虚构"],"examined":0,"matches":0,"candidate_limit":500,"pending_snapshot_limited":false},
            "selection":{"duplicates":0,"project_filtered":0,"missing_or_hidden":0,"before_limit":0,"returned":0,"result_limit":10,"max_chars":8000},"hits":[]
        });
        let diagnostic: Diagnostics = serde_json::from_value(body).unwrap();
        let stages = diagnostic.stages();
        assert_eq!(stages[0].value.as_str(), "19");
        assert_eq!(stages[2].value.as_str(), "0");
        assert!(stages[2].note.contains("0.56") && stages[2].note.contains("关键词 19 / 语义 29"));
    }
}
