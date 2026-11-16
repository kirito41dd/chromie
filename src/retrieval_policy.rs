//! 相关性阈值不参与向量身份；自动值只适用于已经评测的模型配置。
use crate::{
    embedding::Profile,
    error::{Error, Result},
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SimilarityThreshold(f64);

impl SimilarityThreshold {
    pub fn new(value: f64) -> Result<Self> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::invalid("相似度下限须为 0～1 的有限数值"));
        }
        Ok(Self(value))
    }
    pub fn value(self) -> f64 {
        self.0
    }
    pub fn enabled(self) -> bool {
        self.0 > 0.0
    }
    pub fn accepts(self, score: f32) -> bool {
        score.is_finite() && score >= self.0 as f32
    }
}
impl<'de> Deserialize<'de> for SimilarityThreshold {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

pub fn recommended(profile: &Profile) -> Option<SimilarityThreshold> {
    (profile.model == "text-embedding-v4"
        && profile.model_version.is_empty()
        && profile.dimensions == 1024
        && profile.encoding == "plain-v1")
        .then_some(SimilarityThreshold(0.56))
}

pub(crate) fn resolve(profile: &Profile, stored: Option<f64>) -> Result<SimilarityThreshold> {
    stored
        .map(SimilarityThreshold::new)
        .transpose()
        .map(|custom| {
            custom
                .or_else(|| recommended(profile))
                .unwrap_or(SimilarityThreshold(0.0))
        })
}

/// 缺字段保留同模型覆盖值；显式 null 恢复自动，数字为显式覆盖（含 0 关闭）。
pub fn deserialize_update<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<SimilarityThreshold>>, D::Error> {
    Option::<SimilarityThreshold>::deserialize(deserializer).map(Some)
}

pub(crate) fn literal_match(query: &str, content: &str) -> bool {
    let query = query.trim().to_lowercase();
    // 这里仅豁免已经通过 BM25 的完整词面命中。两字词、短缩写仍是有效关键词，
    // 不能用自然语言问法的余弦门槛将它们清空；不放宽单字或零散词匹配。
    query.chars().count() >= 2 && content.to_lowercase().contains(&query)
}

/// 仅描述关键词通道的词面依据，不表示余弦通过或记忆包含所问答案。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum KeywordMatch {
    Literal,
    FullCoverage { parts: usize },
}

/// 每次查询只解析一次。低余弦的 BM25 候选仍须完整覆盖查询，不能漏掉技术名或只碰巧含单字。
pub(crate) struct KeywordQuery {
    normalized: String,
    segments: Vec<Segment>,
}

enum Segment {
    Han(String),
    Identifier(String),
    Unsupported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Han,
    Identifier,
}

impl KeywordQuery {
    pub(crate) fn new(query: &str) -> Self {
        let normalized = query.trim().to_lowercase();
        let segments = segments(&normalized);
        Self {
            normalized,
            segments,
        }
    }

    pub(crate) fn matches(&self, content: &str) -> Option<KeywordMatch> {
        let content = content.to_lowercase();
        if literal_match(&self.normalized, &content) {
            return Some(KeywordMatch::Literal);
        }
        let identifiers = segments(&content)
            .into_iter()
            .filter_map(|segment| match segment {
                Segment::Identifier(word) => Some(word),
                Segment::Han(_) | Segment::Unsupported => None,
            })
            .collect::<HashSet<_>>();
        let mut parts = 0;
        for segment in &self.segments {
            match segment {
                Segment::Identifier(word) => {
                    if !identifiers.contains(word) {
                        return None;
                    }
                    parts += usize::from(word.chars().count() >= 2);
                }
                Segment::Han(text) if text.chars().count() == 1 => {
                    // 单字只能约束其他有效片段，不能单独产生新的保留依据。
                    if !content.contains(text) {
                        return None;
                    }
                }
                Segment::Han(text) => parts += cover_han(text, &content)?,
                Segment::Unsupported => return None,
            }
        }
        (parts >= 2).then_some(KeywordMatch::FullCoverage { parts })
    }
}

fn han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{323af}')
}

fn segments(value: &str) -> Vec<Segment> {
    fn finish(output: &mut Vec<Segment>, text: &mut String, run: Option<Run>) {
        let text = std::mem::take(text);
        if text.is_empty() {
            return;
        }
        match run {
            Some(Run::Han) => output.push(Segment::Han(text)),
            Some(Run::Identifier) => {
                // 非中文标识按原符号与组合标记完整保留，不能把 .NET、C++ 或变音符降成另一个名字。
                if text.chars().any(char::is_alphanumeric) {
                    output.push(Segment::Identifier(text));
                } else {
                    output.push(Segment::Unsupported);
                }
            }
            None => {}
        }
    }
    let (mut output, mut text, mut run) = (Vec::new(), String::new(), None);
    for c in value.chars() {
        let next = if han(c) {
            Some(Run::Han)
        } else if separator(c) {
            None
        } else {
            Some(Run::Identifier)
        };
        if next != run || next.is_none() {
            finish(&mut output, &mut text, run);
        }
        if next.is_some() {
            text.push(c);
        }
        run = next;
    }
    finish(&mut output, &mut text, run);
    output
}

// 只把明确的自然语言分隔符视为边界，其余符号必须完整留在标识里或使新覆盖失败关闭。
fn separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ',' | ';'
                | ':'
                | '/'
                | '\\'
                | '|'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '"'
                | '\''
                | '`'
                | '?'
                | '!'
                | '，'
                | '。'
                | '；'
                | '：'
                | '、'
                | '！'
                | '？'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '「'
                | '」'
                | '『'
                | '』'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
}

/// 用连续的至少双字片段完整分解中文查询；不是分词器，也不允许丢字或依靠重叠片段补齐。
/// 如“数据库运维”可由正文中的“数据库”和“运维”覆盖；只有“数”“据”“库”等散字则不行。
fn cover_han(query: &str, content: &str) -> Option<usize> {
    if content.contains(query) {
        return Some(1);
    }
    let chars = query.chars().collect::<Vec<_>>();
    let mut covered: Vec<Option<usize>> = vec![None; chars.len() + 1];
    covered[0] = Some(0);
    for start in 0..chars.len() {
        let Some(parts) = covered[start] else {
            continue;
        };
        let mut fragment = String::new();
        for (end, c) in chars.iter().enumerate().skip(start) {
            fragment.push(*c);
            if end > start && content.contains(&fragment) {
                let next = &mut covered[end + 1];
                *next = Some(next.map_or(parts + 1, |old| old.min(parts + 1)));
            }
        }
    }
    covered[chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_evaluated_profile_gets_a_default_and_zero_disables() {
        let mut p = Profile {
            model: "text-embedding-v4".into(),
            model_version: String::new(),
            dimensions: 1024,
            encoding: "plain-v1".into(),
        };
        assert_eq!(resolve(&p, None).unwrap().value(), 0.56);
        assert!(!resolve(&p, Some(0.0)).unwrap().enabled());
        p.dimensions = 512;
        assert!(recommended(&p).is_none());
        p.dimensions = 1024;
        p.model_version = "changed".into();
        assert!(recommended(&p).is_none());
        assert_eq!(resolve(&p, None).unwrap().value(), 0.0);
        for n in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            assert!(SimilarityThreshold::new(n).is_err());
        }
        let t = SimilarityThreshold::new(0.5).unwrap();
        assert!(t.accepts(0.5));
        assert!(!t.accepts(0.49));
        assert!(!t.accepts(f32::NAN));
    }
    #[test]
    fn exact_exception_requires_the_entire_query_and_minimum_unicode_length() {
        assert!(literal_match(" MySQL 8.4 ", "默认使用 mysql 8.4 数据库"));
        assert!(literal_match("雾松项目", "雾松项目约定"));
        for (query, content) in [
            ("虚构", "这是一段虚构的长记忆"),
            (" 密码 ", "密码仅通过 Secret 取用"),
            ("AI", "使用 AI 辅助整理"),
            ("GPU", "GPU 驱动的验证结果"),
            ("你好", "用户明确保存的问候语：你好"),
        ] {
            assert!(literal_match(query, content));
        }
        assert!(!literal_match("你好", "一条与问候无关的记忆"));
        assert!(!literal_match("构", "这是一段虚构的长记忆"));
        assert!(!literal_match("  ", "这是一段虚构的长记忆"));
        assert!(!literal_match("雾松 项目", "雾松项目约定"));
    }

    #[test]
    fn full_keyword_coverage_preserves_composed_topics_without_a_literal_phrase() {
        let content = "云杉集群运维：日常巡检通过数据库连接执行只读查询。";
        for query in ["数据库运维", "数据库 运维", "数据库/运维"] {
            assert!(matches!(
                KeywordQuery::new(query).matches(content),
                Some(KeywordMatch::FullCoverage { parts: 2 })
            ));
        }
        assert!(matches!(
            KeywordQuery::new("MySQL 运维").matches("MySQL 数据库的日常运维指南"),
            Some(KeywordMatch::FullCoverage { .. })
        ));
        assert_eq!(
            KeywordQuery::new(" 数据库 ").matches(content),
            Some(KeywordMatch::Literal)
        );
    }

    #[test]
    fn keyword_protection_cannot_drop_identifiers_characters_or_missing_actions() {
        for (query, content) in [
            ("数据库运维", "数、据、库分散出现；只有运维二字相邻"),
            // 重叠的“数据”和“据库”不能覆盖查询的前三个字。
            ("数据库运维", "数据；据库；运维"),
            ("数据库备份运维", "数据库入口；运维只记录巡检方法"),
            ("MySQL 运维", "PostgreSQL 数据库运维指南"),
            ("SQL 运维", "MySQL 的运维指南"),
            ("MySQL 8.4 运维", "MySQL 8.0 的运维指南"),
            ("你好", "你可以阅读这份良好的数据库运维指南"),
            ("你 好", "你可以阅读这份良好的数据库运维指南"),
            ("今天的天气", "数据库运维指南"),
            ("库", "数据库运维指南"),
            ("", "数据库运维指南"),
        ] {
            assert_eq!(
                KeywordQuery::new(query).matches(content),
                None,
                "不能因部分字词或缺失技术标识保护查询 {query}"
            );
        }
    }

    #[test]
    fn complete_technical_identifiers_keep_symbols_and_combining_marks() {
        for name in [".NET", "C++", "C#", "_schema_", "--flag", "cafe\u{301}"] {
            let query = format!("{name} 数据库运维");
            let content = format!("{name} 运维指南；数据库连接入口。");
            assert!(matches!(
                KeywordQuery::new(&query).matches(&content),
                Some(KeywordMatch::FullCoverage { .. })
            ));
        }
        for (query, content) in [
            (".NET 运维", "NET 协议的日常运维指南"),
            ("C++ 数据库运维", "C 数据库的日常运维指南"),
            ("C# 数据库运维", "C++ 数据库的日常运维指南"),
            ("_schema_ 数据库运维", "schema 数据库的日常运维指南"),
            ("--flag 数据库运维", "flag 数据库的日常运维指南"),
            ("cafe\u{301} 数据库运维", "cafe 数据库的日常运维指南"),
            ("cafe 数据库运维", "cafe\u{301} 数据库的日常运维指南"),
            ("\u{301} 数据库运维", "数据库的日常运维指南"),
            ("☁ 数据库运维", "数据库的日常运维指南"),
        ] {
            assert_eq!(KeywordQuery::new(query).matches(content), None, "{query}");
        }
    }
}
