#![cfg_attr(not(test), forbid(unsafe_code))]
use serde::Deserialize;

#[derive(Clone, Default, Deserialize)]
pub struct Settings {
    pub revision: i64,
    pub enabled: bool,
    pub active: Option<Generation>,
    pub pending: Option<Generation>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub available: bool,
    pub today: Totals,
    pub seven_days: Totals,
    pub all_time: Totals,
    pub days: Vec<Day>,
    pub truncated: bool,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Totals {
    pub tokens: u64,
    pub requests: u64,
    pub failed: u64,
    pub unknown: u64,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Day {
    pub date: String,
    pub model: String,
    #[serde(flatten)]
    pub totals: Totals,
}
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
impl Totals {
    pub fn view(&self) -> crate::UsageTotal {
        crate::UsageTotal {
            tokens: grouped(self.tokens).into(),
            requests: format!("{} 次请求", grouped(self.requests)).into(),
        }
    }
    pub fn note(&self) -> String {
        let mut parts = Vec::new();
        if self.unknown > 0 {
            parts.push(format!("用量未知 {} 次", grouped(self.unknown)));
        }
        if self.failed > 0 {
            parts.push(format!("模型调用失败 {} 次", grouped(self.failed)));
        }
        parts.join(" · ")
    }
}
impl Usage {
    pub fn note(&self) -> String {
        let mut text = if self.all_time.requests == 0 {
            "尚无模型调用；从启用统计起累计，不补算历史用量。".to_owned()
        } else {
            "仅累加接口已报告的用量，包含连接测试、索引生成与召回查询。".to_owned()
        };
        let unknown = self.all_time.note();
        if !unknown.is_empty() {
            text.push_str(&format!("\n{unknown}；未知用量不按零计算。"));
        }
        if self.truncated {
            text.push_str("\n最近明细仅展示前 100 行，汇总仍包含全部模型。");
        }
        text
    }
    pub fn rows(&self) -> Vec<crate::UsageDay> {
        self.days
            .iter()
            .map(|day| crate::UsageDay {
                date: day.date.clone().into(),
                model: day.model.clone().into(),
                tokens: grouped(day.totals.tokens).into(),
                requests: format!("{} 次请求", grouped(day.totals.requests)).into(),
                note: day.totals.note().into(),
            })
            .collect()
    }
}
#[derive(Clone, Deserialize)]
pub struct Generation {
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub similarity_threshold_override: Option<f64>,
    #[serde(default)]
    pub batch_size_override: Option<u32>,
    pub profile: Profile,
    pub base_url: String,
    pub has_api_key: bool,
    pub progress: Progress,
    pub scan_complete: bool,
}
fn default_concurrency() -> u32 {
    4
}

pub fn parse_batch_size(value: &str) -> Result<Option<u32>, &'static str> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    match value.trim().parse::<u32>() {
        Ok(n) if (1..=10).contains(&n) => Ok(Some(n)),
        _ => Err("每批文本数须为 1～10 的整数；留空使用自动值。"),
    }
}
pub fn parse_similarity(value: &str) -> Result<Option<f64>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<f64>() {
        Ok(n) if n.is_finite() && (0.0..=1.0).contains(&n) => Ok(Some(n)),
        _ => Err("相似度下限须为 0～1；留空使用自动值，0 关闭过滤。"),
    }
}
#[derive(Clone, PartialEq, Deserialize)]
pub struct Profile {
    pub model: String,
    pub model_version: String,
    pub dimensions: u32,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub total: u64,
    pub ready: u64,
    pub pending: u64,
    pub running: u64,
    pub retry: u64,
    pub failed: u64,
    pub last_error: Option<String>,
    pub last_updated_at: Option<String>,
    pub next_retry_at: Option<String>,
}

#[derive(Deserialize)]
pub struct ConnectionTest {
    model: String,
    dimensions: u32,
    elapsed_ms: f64,
    #[serde(default)]
    batch_size: u32,
}
impl ConnectionTest {
    pub fn label(&self) -> String {
        format!(
            "连接正常 · {} · {} 维 · 验证 {} 段 · {:.0} ms。测试未修改配置。",
            self.model, self.dimensions, self.batch_size, self.elapsed_ms
        )
    }
}
impl Progress {
    pub fn label(&self) -> String {
        let mut text = format!("已就绪 {} / {} 条", self.ready, self.total);
        for (label, count) in [
            ("待处理", self.pending),
            ("处理中", self.running),
            ("自动重试", self.retry),
            ("待修复", self.failed),
        ] {
            if count > 0 {
                text.push_str(&format!(" · {label} {count}"));
            }
        }
        if self.retry > 0 {
            text.push_str("\n临时故障会自动恢复，无需手动重试。退避间隔最长 5 分钟。");
            if let Some(at) = &self.next_retry_at {
                text.push_str(&format!("下次尝试：{} UTC。", short_time(at)));
            }
        }
        if self.failed > 0 {
            text.push_str("\n请检查模型配置，保存有效设置后会自动继续。");
        }
        if (self.retry > 0 || self.failed > 0)
            && let Some(error) = &self.last_error
        {
            text.push_str("\n最近问题：");
            text.push_str(error_label(error));
        }
        if let Some(at) = &self.last_updated_at {
            text.push_str("\n最近处理：");
            text.push_str(&short_time(at));
            text.push_str(" UTC");
        }
        text
    }
    pub fn ratio(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.ready.min(self.total) as f64 / self.total as f64) as f32
        }
    }
}
fn short_time(value: &str) -> String {
    value.split('.').next().unwrap_or(value).replace('T', " ")
}
impl Settings {
    pub fn target(&self) -> Option<&Generation> {
        self.pending.as_ref().or(self.active.as_ref())
    }
}
pub fn error_label(code: &str) -> &str {
    match code {
        "model_busy" => "模型并发已满，等待超时，请稍后重试。",
        "model_auth" => "模型服务鉴权失败，请检查 API Key。",
        "model_rate_limited" => "模型服务限流，等待重试。",
        "model_unreachable" => "模型服务不可达或请求超时。",
        "model_server_error" => "模型服务暂时故障，后台会自动重试。",
        "model_batch_response" => "批量请求或返回格式不匹配，请调小每批文本数后保存。",
        "model_response" => "模型接口或返回格式不匹配。",
        "model_dimensions" => "向量维度或数值无效，请核对模型配置。",
        "model_settings_changed" => "配置已变化，请刷新页面核对后重新保存。",
        "model_rebuild_confirmation_required" => "更换模型需要确认全量重建。",
        _ => "处理未完成，请稍后重试；持续失败时请检查服务配置。",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_size_input_accepts_auto_and_bounded_integer() {
        assert_eq!(parse_batch_size(" ").unwrap(), None);
        assert_eq!(parse_batch_size(" 10 ").unwrap(), Some(10));
        assert_eq!(parse_batch_size("1").unwrap(), Some(1));
        for text in ["0", "11", "-1", "1.5", "NaN"] {
            assert!(parse_batch_size(text).is_err());
        }
    }
    #[test]
    fn similarity_input_preserves_auto_zero_and_validates_finite_range() {
        assert_eq!(parse_similarity(" ").unwrap(), None);
        assert_eq!(parse_similarity("0").unwrap(), Some(0.0));
        assert_eq!(parse_similarity(" 0.56 ").unwrap(), Some(0.56));
        for value in ["NaN", "inf", "-0.1", "1.1", "abc"] {
            assert!(parse_similarity(value).is_err());
        }
    }
    #[test]
    fn daily_usage_keeps_unknown_calls_visible_without_inventing_tokens() {
        let usage: Usage = serde_json::from_value(serde_json::json!({"available":true,"all_time":{"tokens":23000,"requests":12,"unknown":2,"failed":1},"days":[{"date":"2026-11-03","model":"fictional-model","tokens":0,"requests":2,"unknown":2,"failed":1}]})).unwrap();
        assert_eq!(usage.all_time.view().tokens.as_str(), "23,000");
        assert_eq!(usage.rows()[0].requests.as_str(), "2 次请求");
        assert!(usage.rows()[0].note.contains("用量未知 2 次"));
        assert!(usage.note().contains("未知用量不按零计算"));
        assert!(Usage::default().note().contains("尚无模型调用"));
    }
    #[test]
    fn progress_uses_current_memory_coverage_and_preserves_retry_failure_difference() {
        let p = Progress {
            total: 10,
            ready: 6,
            retry: 3,
            failed: 1,
            ..Default::default()
        };
        assert_eq!(p.ratio(), 0.6);
        assert!(p.label().contains("自动重试 3 · 待修复 1"));
        assert!(!p.label().contains("待处理 0"));
        assert_eq!(
            Progress {
                total: 3,
                ready: 5,
                ..Default::default()
            }
            .ratio(),
            1.0
        );
        assert_eq!(Progress::default().ratio(), 0.0);
    }
}
