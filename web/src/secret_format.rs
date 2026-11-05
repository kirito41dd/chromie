#![cfg_attr(not(test), forbid(unsafe_code))]

/// 展示只重排 JSON 空白，保留数字、重复键和转义的原始字节表达。
#[derive(Default, Debug)]
pub struct Line {
    pub key: String,
    pub value: String,
    pub kind: i32,
}

pub fn formatted_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| format!("{}{}", line.key, line.value))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn json_lines(raw: &str) -> Option<Vec<Line>> {
    // 仅验证语法；不把反序列化的值重新序列化，以免大整数或重复键变化。
    serde_json::from_str::<serde::de::IgnoredAny>(raw).ok()?;
    if !matches!(raw.trim_start().chars().next(), Some('{' | '[')) {
        return None;
    }
    let mut pretty = String::new();
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if quoted {
            pretty.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                pretty.push(ch);
            }
            '{' | '[' => {
                pretty.push(ch);
                depth += 1;
                newline(&mut pretty, depth);
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut pretty, depth);
                pretty.push(ch);
            }
            ',' => {
                pretty.push(ch);
                newline(&mut pretty, depth);
            }
            ':' => pretty.push_str(": "),
            ch if ch.is_whitespace() => {}
            _ => pretty.push(ch),
        }
    }
    Some(pretty.lines().map(line).collect())
}

fn newline(output: &mut String, depth: usize) {
    output.push('\n');
    // 极深嵌套也不能把字段挤出窄屏。
    output.push_str(&"  ".repeat(depth.min(8)));
}

fn line(text: &str) -> Line {
    let mut quoted = false;
    let mut escaped = false;
    let mut split = None;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ':' && !quoted {
            split = Some(i + 1);
            break;
        }
    }
    let (key, value) = split
        .and_then(|i| text.get(..i).zip(text.get(i..)))
        .unwrap_or(("", text));
    let kind = match value.trim_start().chars().next() {
        Some('"') => 1,
        Some('-' | '0'..='9') => 2,
        Some('t' | 'f' | 'n') => 3,
        _ => 0,
    };
    Line {
        key: key.into(),
        value: value.into(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formatting_preserves_numeric_literals_duplicate_keys_and_escapes() {
        let raw =
            r#"{"a":123456789012345678901234567890,"a":"\\\"你好","arr":[true,null,1.2300e+50]}"#;
        let rows = json_lines(raw).unwrap();
        let rebuilt = formatted_text(&rows);
        assert!(rebuilt.contains("123456789012345678901234567890"));
        assert!(rebuilt.contains("1.2300e+50"));
        assert_eq!(rebuilt.matches("\"a\":").count(), 2);
        assert!(rows.iter().any(|r| r.kind == 1));
        assert!(rows.iter().any(|r| r.kind == 2));
        assert!(rows.iter().any(|r| r.kind == 3));
        serde_json::from_str::<serde::de::IgnoredAny>(&rebuilt).unwrap();
        assert!(json_lines("001234").is_none());
        assert!(json_lines("{broken}").is_none());
    }

    #[test]
    fn formatted_selection_keeps_field_boundaries_and_whitespace_in_one_document() {
        let rows = json_lines(r#"{"密码":"虚构🙂","enabled":true,"port":3306}"#).unwrap();
        let formatted = formatted_text(&rows);
        assert_eq!(
            formatted,
            "{\n  \"密码\": \"虚构🙂\",\n  \"enabled\": true,\n  \"port\": 3306\n}"
        );
    }
}
