//! 检索前的只读业务能力快照；模式表示预计执行策略，不是网络探活或相关性保证。
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    Keyword,
    Hybrid,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticState {
    Disabled,
    Building,
    Empty,
    Ready,
    Updating,
    Degraded,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticReason {
    NotConfigured,
    Disabled,
    InitialIndexBuilding,
    EmptySpace,
    RetryBackoff,
    IndexUnavailable,
}

#[derive(Serialize)]
pub struct RetrievalCapabilities {
    pub mode: RetrievalMode,
    pub semantic_state: SemanticState,
    pub reason: Option<SemanticReason>,
    pub config_revision: i64,
    pub rebuilding: bool,
}

#[derive(Serialize)]
pub struct Capabilities {
    pub capability_version: u32,
    pub space: String,
    pub cache_ttl_seconds: u32,
    pub retrieval: RetrievalCapabilities,
}
