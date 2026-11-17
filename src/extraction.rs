//! 候选携带公开结论与可回查证据；AI 决定修订，服务校验发布边界。
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Category {
    Fact,
    Decision,
    Preference,
    Method,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub event_id: Uuid,
    pub quote: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub content: String,
    pub category: Category,
    pub search_query: String,
    pub source_refs: Vec<Uuid>,
    pub evidence: Vec<Evidence>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Generated {
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub conversation_title: Option<String>,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Create,
    ProposeRevision,
    Skip,
    Defer,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Replacement {
    pub memory_id: Uuid,
    pub expected_revision: i32,
    pub content: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub candidate_index: usize,
    pub action: Action,
    pub reason: String,
    #[serde(default)]
    pub replacement: Option<Replacement>,
    /// 核对时可补上生成阶段遗漏的用户授权/变更原话；仍须逐字属于固定输入。
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// 可选的用户更正原话；不作为自主整理开关，提供时仍校验真实性。
    #[serde(default)]
    pub explicit_change: Option<Evidence>,
    /// 核对可补齐来源中已有的背景或完整链接，不得改变候选含义。
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub quality_issue: Option<String>,
}

/// 仅阻断明确残缺的操作路径；是否需要环境/机器信息由正文语义决定，不套统一模板。
pub(crate) fn incomplete_method(content: &str) -> bool {
    content.split_whitespace().any(|word| {
        (word.contains(".../") || word.contains("…/"))
            && (word.contains("http") || word.contains("install.") || word.contains(".sh"))
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reviewed {
    pub decisions: Vec<Decision>,
}
