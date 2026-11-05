#![cfg_attr(not(test), forbid(unsafe_code))]

use serde::Deserialize;

pub struct Request {
    pub generation: u32,
    pub query: String,
}

#[derive(Default)]
pub struct RecallState {
    generation: u32,
    pending: bool,
}

impl RecallState {
    pub fn submit(&mut self, query: &str) -> Option<Request> {
        let query = query.trim();
        if query.is_empty() || query.len() > 256 {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending = true;
        Some(Request {
            generation: self.generation,
            query: query.to_owned(),
        })
    }

    pub fn finish(&mut self, generation: u32) -> bool {
        if self.generation != generation || !self.pending {
            return false;
        }
        self.pending = false;
        true
    }

    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = false;
    }
}

pub fn debug_request(space: &str, query: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "method":"POST", "path":format!("/spaces/{space}/recall"),
        "body":{"query":query,"max_chars":8000}
    }))
    .unwrap_or_default()
}

pub fn debug_response(body: &str) -> String {
    if body.len() > 40000 {
        return "响应超过调试显示上限".into();
    }
    if body.is_empty() {
        return "未收到响应正文".into();
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| body.to_owned())
}

#[derive(Deserialize)]
pub struct Results {
    #[serde(default)]
    pub retrieval_mode: String,
    #[serde(default)]
    pub semantic_available: bool,
    pub memories: Vec<Card>,
    pub index_available: bool,
    pub partial: bool,
    pub truncated: bool,
    #[serde(default, deserialize_with = "crate::diagnostics::parse_optional")]
    pub debug: Option<crate::diagnostics::Diagnostics>,
}

#[derive(Deserialize)]
pub struct Card {
    #[serde(default)]
    pub origin: crate::memory::Origin,
    #[serde(flatten)]
    pub secret: crate::memory::SecretMetadata,
    #[serde(default)]
    pub memory_id: String,
    pub content: String,
    pub revision: i32,
    pub project_key: Option<String>,
    pub updated_at: String,
    pub source_refs: Vec<String>,
    pub processing_state: String,
}

impl Results {
    pub fn parse(body: &str) -> Option<Self> {
        if body.len() > 40000 {
            return None;
        }
        let result: Self = serde_json::from_str(body).ok()?;
        (result.memories.len() <= 10).then_some(result)
    }

    pub fn note(&self) -> String {
        let mut notes = Vec::new();
        if self.retrieval_mode == "hybrid" {
            notes.push("本次结合关键词与语义召回，按 RRF 融合排序。".into());
        }
        if self.retrieval_mode == "keyword" {
            notes.push("本次使用关键词检索。".into());
        }
        if !self.index_available && self.semantic_available {
            notes.push("关键词索引暂不可用，语义召回仍可用；关键词补充范围有限。".into());
        } else if !self.index_available {
            notes.push(self.debug.as_ref().map(|debug| format!("索引暂不可用，本次使用有限关键词检索，仅检查最近最多 {} 条有效记忆，可能遗漏较早内容。", debug.fallback_limit()))
                .unwrap_or_else(|| "索引暂不可用，本次使用有限关键词检索，结果可能不完整。".into()));
        } else if let Some(debug) = &self.debug {
            let pending = debug.pending_visible();
            if pending > 0 {
                notes.push(format!(
                    "其中 {pending} 条记忆仍在等待索引，通过关键词补充找到。"
                ));
            }
        }
        if self.partial {
            notes.push("本次检索或返回范围受限，结果可能不完整。".into());
        }
        if self.truncated {
            notes.push("部分正文或结果已受长度限制。".into());
        }
        notes.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{RecallState, Results, debug_request, debug_response};

    #[test]
    fn debug_json_contains_only_request_parameters_and_actual_response() {
        let query = "虚构的带引号\"与换行\n查询";
        let request: serde_json::Value =
            serde_json::from_str(&debug_request("demo", query)).unwrap();
        assert_eq!(request["method"], "POST");
        assert_eq!(request["path"], "/spaces/demo/recall");
        assert_eq!(request["body"]["query"], query);
        assert_eq!(request["body"]["max_chars"], 8000);
        assert!(request.get("headers").is_none());
        let response = "{\"index_available\":false,\"partial\":true}";
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&debug_response(response)).unwrap(),
            serde_json::from_str::<serde_json::Value>(response).unwrap()
        );
        assert_eq!(debug_response("虚构的非 JSON 错误"), "虚构的非 JSON 错误");
        assert!(debug_response("").contains("未收到"));
        assert!(debug_response(&"x".repeat(40001)).contains("上限"));
    }

    #[test]
    fn edited_queries_and_logout_discard_old_results() {
        let mut state = RecallState::default();
        assert!(state.submit("  ").is_none());
        assert!(state.submit(&"中".repeat(86)).is_none());
        let first = state.submit(" 项目决定 ").unwrap();
        assert_eq!(first.query, "项目决定");
        let second = state.submit("使用经验").unwrap();
        assert!(!state.finish(first.generation));
        assert!(state.finish(second.generation));
        assert!(!state.finish(second.generation));
        let refresh = state.submit("使用经验").unwrap();
        assert_eq!(refresh.query, "使用经验");
        state.clear();
        assert!(!state.finish(refresh.generation));
    }

    #[test]
    fn pending_and_degraded_results_explain_the_actual_search_limit() {
        let mut body = serde_json::json!({
            "memories":[{"memory_id":"虚构记忆","content":"虚构正文","revision":1,"project_key":null,
                "updated_at":"2026-10-28T00:00:00Z","source_refs":[],"processing_state":"index_pending"}],
            "index_available":true,"partial":false,"truncated":false,
            "debug":{
                "timing":{"total_ms":20.0,"index_call_ms":3.0,"qdrant_server_ms":1.0},
                "qdrant":{"available":true,"field":"content","tokenizer":"multilingual","candidates":0,"candidate_limit":30,"validated":0,"discarded":0},
                "fallback":{"mode":"pending_keyword","terms":["虚构"],"examined":1,"matches":1,"candidate_limit":500,"pending_snapshot_limited":false},
                "selection":{"duplicates":0,"project_filtered":0,"missing_or_hidden":0,"before_limit":1,"returned":1,"result_limit":10,"max_chars":8000},
                "hits":[{"memory_id":"虚构记忆","path":"pending_keyword","bm25_score":null,"indexed_revision":null,"score_matches_revision":null}]
            }
        });
        let result = Results::parse(&body.to_string()).unwrap();
        assert!(result.note().contains("其中 1 条记忆仍在等待索引"));
        assert!(!result.note().contains("索引暂不可用"));
        let stages = result.debug.as_ref().unwrap().stages();
        assert_eq!(stages[0].value.as_str(), "0");
        assert!(!stages[0].warning);
        assert_eq!(stages[1].value.as_str(), "1");
        body["index_available"] = serde_json::json!(false);
        body["partial"] = serde_json::json!(true);
        body["truncated"] = serde_json::json!(true);
        body["debug"]["qdrant"]["available"] = serde_json::json!(false);
        body["debug"]["fallback"]["mode"] = serde_json::json!("recent_keyword");
        let result = Results::parse(&body.to_string()).unwrap();
        assert!(result.note().contains("最近最多 500 条"));
        assert!(result.note().contains("不完整"));
        assert!(result.note().contains("长度限制"));
        assert_eq!(
            result.debug.as_ref().unwrap().stages()[0].value.as_str(),
            "未完成"
        );
    }

    #[test]
    fn result_flags_and_cards_are_preserved_without_inventing_completeness() {
        let body = serde_json::json!({
            "memories":[{"kind":"secret_ref","secret_stored":true,"secret_ref":"secret://demo/01900000-0000-7000-8000-000000000000","memory_id":"01900000-0000-7000-8000-000000000000","content":"虚构召回正文","revision":2,"project_key":"demo",
                "updated_at":"2026-10-27T00:00:00Z","source_refs":["虚构来源"],"processing_state":"index_pending"}],
            "index_available":false,"partial":true,"truncated":true
        }).to_string();
        let result = Results::parse(&body).unwrap();
        assert!(result.debug.is_none(), "兼容不带诊断的旧服务响应");
        let mut incompatible: serde_json::Value = serde_json::from_str(&body).unwrap();
        incompatible["debug"] = serde_json::json!({"unknown_version":true});
        let compatible = Results::parse(&incompatible.to_string()).unwrap();
        assert!(compatible.debug.is_none(), "诊断不兼容不影响召回正文");
        assert_eq!(compatible.memories[0].content, "虚构召回正文");
        let card = &result.memories[0];
        assert_eq!(card.secret.kind, "secret_ref");
        assert!(card.secret.stored());
        assert_eq!(card.secret.status(), "密文已保存");
        assert!(
            card.secret
                .secret_ref
                .as_ref()
                .unwrap()
                .starts_with("secret://demo/")
        );
        assert_eq!(card.origin.agent_name, "历史记录");
        assert_eq!(card.memory_id, "01900000-0000-7000-8000-000000000000");
        assert_eq!(card.content, "虚构召回正文");
        assert_eq!(card.revision, 2);
        assert_eq!(card.project_key.as_deref(), Some("demo"));
        assert_eq!(card.updated_at, "2026-10-27T00:00:00Z");
        assert_eq!(card.source_refs.len(), 1);
        assert_eq!(card.processing_state, "index_pending");
        assert!(result.note().contains("有限关键词检索"));
        assert!(result.note().contains("不完整"));
        assert!(result.note().contains("长度限制"));
        assert!(Results::parse("{\"memories\":[]}").is_none());
        assert!(Results::parse(&" ".repeat(40001)).is_none());
        assert!(Results::parse("{\"truncated\":true,\"code\":\"budget_too_small\"}").is_none());
    }
}
