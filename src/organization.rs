//! 自动整理按当前记忆修订推进；预算限制处理尝试，不是供应商计费额度。
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: i64,
    pub enabled: bool,
    #[serde(default)]
    pub extraction_enabled: bool,
    #[serde(default)]
    pub maintenance_enabled: bool,
    #[serde(default = "default_maintenance_interval")]
    pub maintenance_interval_hours: u32,
    pub batch_size: u32,
    pub interval_seconds: u32,
    pub daily_limit: u32,
}
fn default_maintenance_interval() -> u32 {
    24
}
