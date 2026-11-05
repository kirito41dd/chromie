//! 相关性阈值不参与向量身份；自动值只适用于已经评测的模型配置。
use crate::{
    embedding::Profile,
    error::{Error, Result},
};
use serde::{Deserialize, Deserializer, Serialize};

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
}
