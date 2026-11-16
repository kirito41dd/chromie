//! 记忆类型和关系的公开契约。引用跟随对象，派生依据固定到已读取的修订。
#[cfg(feature = "server")]
use crate::error::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    #[default]
    Note,
    Topic,
}

impl MemoryType {
    pub fn is_note(&self) -> bool {
        *self == Self::Note
    }
    #[cfg(feature = "server")]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Topic => "topic",
        }
    }
    #[cfg(feature = "server")]
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "note" => Ok(Self::Note),
            "topic" => Ok(Self::Topic),
            _ => Err(Error::unavailable()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Reference,
    DerivedFrom,
}

#[cfg(feature = "server")]
impl Relation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::DerivedFrom => "derived_from",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "reference" => Ok(Self::Reference),
            "derived_from" => Ok(Self::DerivedFrom),
            _ => Err(Error::unavailable()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub memory_id: Uuid,
    pub relation: Relation,
    /// derived_from 必须填写实际读取的当前修订；reference 省略，跟随对象当前内容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i32>,
}

#[cfg(feature = "server")]
impl Link {
    pub(crate) fn validate(&self) -> Result<()> {
        match (self.relation, self.revision) {
            (Relation::Reference, None) => Ok(()),
            (Relation::DerivedFrom, Some(revision)) if revision > 0 => Ok(()),
            _ => Err(Error::invalid(
                "普通引用不固定修订；派生依据必须指定已读取的正数修订",
            )),
        }
    }
}

#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Navigation {
    pub project_key: Option<String>,
    pub cursor: Option<Uuid>,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Neighbors {
    pub memory_id: Uuid,
    #[serde(default)]
    pub incoming: bool,
    pub cursor: Option<Uuid>,
    pub project_key: Option<String>,
    pub max_chars: Option<usize>,
}

#[cfg(feature = "server")]
pub(crate) fn title(content: &str) -> String {
    content
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("未命名记忆")
        .trim()
        .trim_start_matches('#')
        .trim()
        .chars()
        .take(80)
        .collect()
}
