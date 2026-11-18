//! 召回列表按排名保留可辨认的正文前缀。详情和证据有独立的分页入口。
use serde_json::{Value, json};

const PREVIEW_CHARS: usize = 160;
const ENVELOPE_RESERVE: usize = 128;

struct Preview {
    metadata: Value,
    body: String,
}

impl Preview {
    fn new(mut card: Value) -> Self {
        let body = match card["content"].take() {
            Value::String(body) => body,
            _ => String::new(),
        };
        Self {
            metadata: card,
            body,
        }
    }

    fn render(&self, chars: usize) -> Value {
        let mut card = self.metadata.clone();
        let prefix: String = self.body.chars().take(chars).collect();
        card["content_truncated"] = json!(prefix.len() < self.body.len());
        card["content"] = Value::String(prefix);
        card
    }
}

pub(super) fn pack(cards: Vec<Value>, mut result: Value, maximum: usize) -> Value {
    let limit = maximum.saturating_sub(ENVELOPE_RESERVE);
    let previews: Vec<_> = cards.into_iter().map(Preview::new).collect();
    result["memories"] = json!([]);
    // 两个布尔标记始终存在，序列化长度随正文前缀单调递增；JSON 转义也计入预算。
    result["truncated"] = json!(false);
    let base = result.to_string().chars().count();
    let size = |rows: &[Preview], chars: usize| {
        base + rows.len().saturating_sub(1)
            + rows
                .iter()
                .map(|row| row.render(chars).to_string().chars().count())
                .sum::<usize>()
    };

    let mut count = previews.len();
    while count > 1 && size(&previews[..count], PREVIEW_CHARS) > limit {
        count -= 1;
    }
    let selected = &previews[..count];
    if size(selected, 0) > limit {
        return json!({"truncated":true,"code":"budget_too_small","message":"元数据超出预算，请增大 max_chars"});
    }

    let longest = selected
        .iter()
        .map(|row| row.body.chars().count())
        .max()
        .unwrap_or(0);
    let full = count == previews.len() && longest <= limit && size(selected, longest) <= limit;
    let mut length = longest;
    if !full {
        let mut low = 0;
        let mut high = longest.min(limit);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if size(selected, mid) <= limit {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        length = low;
    }
    result["memories"] = Value::Array(selected.iter().map(|row| row.render(length)).collect());
    result["truncated"] = json!(!full);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_keeps_readable_ranked_prefixes_and_counts_json_escapes() {
        let long = "带引号\"、换行\n和表情😀的一段。".repeat(100);
        let bodies = ["短记忆", &long, &long, &long, &long, &long, &long];
        let cards: Vec<_> = bodies
            .iter()
            .enumerate()
            .map(|(rank, body)| json!({"memory_id":rank,"content":body,"revision":1}))
            .collect();
        for maximum in [1024, 3000, 8000] {
            let result = pack(cards.clone(), json!({}), maximum);
            let rows = result["memories"].as_array().unwrap();
            assert!(result.to_string().chars().count() + ENVELOPE_RESERVE <= maximum);
            assert_eq!(rows[0]["content"], bodies[0]);
            assert_eq!(rows[0]["content_truncated"], false);
            for (rank, row) in rows.iter().enumerate().skip(1) {
                let preview = row["content"].as_str().unwrap();
                assert_eq!(row["memory_id"], rank);
                assert!(bodies[rank].starts_with(preview));
                assert!(preview.chars().count() >= PREVIEW_CHARS);
                assert_eq!(row["content_truncated"], preview != bodies[rank]);
            }
            assert_eq!(
                result["truncated"],
                rows.len() < cards.len() || rows.iter().any(|row| row["content_truncated"] == true)
            );
        }
    }

    #[test]
    fn first_hit_can_use_a_smaller_preview_under_the_minimum_budget() {
        let metadata = "长".repeat(750);
        let result = pack(
            vec![json!({"memory_id":"首条","scope":metadata,"content":"文".repeat(999)})],
            json!({}),
            1024,
        );
        let row = &result["memories"][0];
        let length = row["content"].as_str().unwrap().chars().count();
        assert!(length > 0 && length < PREVIEW_CHARS);
        assert_eq!(row["content_truncated"], true);
        assert_eq!(row["scope"], metadata);
        assert!(result.to_string().chars().count() + ENVELOPE_RESERVE <= 1024);
    }
}
