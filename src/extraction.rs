//! 候选只携带公开结论与可回查证据；核对阶段不能借新增动作绕过修订确认。
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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reviewed {
    pub decisions: Vec<Decision>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    pub job_id: Uuid,
    pub candidate_index: usize,
}
