//! Provider 只管理实例级连接；模型用途保留独立配置，公共视图不包含凭据。
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub provider_id: Option<Uuid>,
    pub expected_revision: i64,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
}
