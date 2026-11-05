//! 单次 embedding 请求的输入条数；独立于向量身份与同名模型共享并发。
use crate::{
    embedding::Profile,
    error::{Error, Result},
};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_BATCH_SIZE: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BatchSize(u32);

impl BatchSize {
    pub fn new(value: u32) -> Result<Self> {
        if !(1..=MAX_BATCH_SIZE).contains(&value) {
            return Err(Error::invalid("每批文本数须为 1～10 的整数"));
        }
        Ok(Self(value))
    }
    pub fn value(self) -> u32 {
        self.0
    }
    pub fn default_for(profile: &Profile) -> Self {
        Self(if profile.model == "text-embedding-v4" {
            10
        } else {
            1
        })
    }
    pub(crate) fn resolve(profile: &Profile, stored: Option<u32>) -> Result<Self> {
        stored
            .map(Self::new)
            .transpose()
            .map(|value| value.unwrap_or_else(|| Self::default_for(profile)))
    }
}
impl<'de> Deserialize<'de> for BatchSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
pub fn deserialize_update<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<BatchSize>>, D::Error> {
    Option::<BatchSize>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_and_unknown_model_defaults_prevent_silent_reuse() {
        for n in [0, 11, u32::MAX] {
            assert!(BatchSize::new(n).is_err());
        }
        let mut p = Profile {
            model: "text-embedding-v4".into(),
            model_version: String::new(),
            dimensions: 1024,
            encoding: "plain-v1".into(),
        };
        assert_eq!(BatchSize::resolve(&p, None).unwrap().value(), 10);
        p.model = "another-model".into();
        assert_eq!(BatchSize::resolve(&p, None).unwrap().value(), 1);
        assert_eq!(BatchSize::resolve(&p, Some(4)).unwrap().value(), 4);
    }
}
