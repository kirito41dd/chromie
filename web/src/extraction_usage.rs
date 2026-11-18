#![forbid(unsafe_code)]
use crate::ExtractionUsageView;
use serde::Deserialize;

#[derive(Deserialize)]
struct NegativeReasons {
    scope_mismatch: u64,
    wrong_result_type: u64,
    missing_detail: u64,
    unspecified: u64,
}

#[derive(Deserialize)]
struct Usage {
    generated: u64,
    recalled: u64,
    evaluated: u64,
    not_recalled: u64,
    negative_reasons: NegativeReasons,
}

pub(crate) fn view(body: &str) -> Option<ExtractionUsageView> {
    let usage: Usage = serde_json::from_str(body).ok()?;
    let reasons = usage.negative_reasons;
    let mut counts = [
        ("范围不符", reasons.scope_mismatch),
        ("结果类型不符", reasons.wrong_result_type),
        ("缺少细节", reasons.missing_detail),
        ("未注明", reasons.unspecified),
    ];
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let details = counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{label} {count} 次"))
        .collect::<Vec<_>>();
    let reasons = if details.is_empty() {
        "暂无明确的无用反馈。".to_owned()
    } else {
        format!("无用反馈按条次统计 · {}", details.join("、"))
    };
    Some(ExtractionUsageView {
        generated: usage.generated.to_string().into(),
        recalled: usage.recalled.to_string().into(),
        evaluated: usage.evaluated.to_string().into(),
        not_recalled: usage.not_recalled.to_string().into(),
        reasons: reasons.into(),
        has_memories: usage.generated > 0,
        loaded: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_absent_data_from_zero_and_orders_negative_counts() {
        assert!(view("{}").is_none());
        let mut data = serde_json::json!({"generated":0,"recalled":0,"evaluated":0,"not_recalled":0,
            "negative_reasons":{"scope_mismatch":0,"wrong_result_type":0,"missing_detail":0,"unspecified":0}
        });
        let empty = view(&data.to_string()).unwrap();
        assert!(empty.loaded && !empty.has_memories);
        data["generated"] = 4.into();
        data["not_recalled"] = 3.into();
        data["recalled"] = 1.into();
        data["evaluated"] = 1.into();
        data["negative_reasons"]["unspecified"] = 3.into();
        data["negative_reasons"]["scope_mismatch"] = 1.into();
        let stats = view(&data.to_string()).unwrap();
        assert_eq!(stats.not_recalled.as_str(), "3");
        assert_eq!(
            stats.reasons.as_str(),
            "无用反馈按条次统计 · 未注明 3 次、范围不符 1 次"
        );
    }
}
