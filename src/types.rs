use crate::error::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct SpaceId(pub(crate) Uuid);

impl SpaceId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
    pub fn bytes(self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }
}

#[derive(Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    #[default]
    Global,
    Project {
        project_key: String,
    },
}

impl Scope {
    pub fn project(&self) -> Result<Option<String>> {
        match self {
            Self::Global => Ok(None),
            Self::Project { project_key } => {
                bounded(project_key, 128)?;
                Ok(Some(project_key.clone()))
            }
        }
    }
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remember {
    pub content: String,
    /// 本次写操作的 UUID（如 01900000-0000-7000-8000-000000000000）；重试复用，不使用项目名或自定义文字。
    pub request_id: Uuid,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub source_refs: Vec<Uuid>,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Correct {
    pub memory_id: Uuid,
    pub expected_revision: i32,
    /// 本次写操作的 UUID（如 01900000-0000-7000-8000-000000000000）；重试复用，不使用项目名或自定义文字。
    pub request_id: Uuid,
    pub reason: String,
    pub content: Option<String>,
    pub scope: Option<Scope>,
    #[schemars(with = "Option<Vec<Uuid>>")]
    pub source_refs: Option<Vec<String>>,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Forget {
    pub memory_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub expected_revision: Option<i32>,
    /// 本次写操作的 UUID（如 01900000-0000-7000-8000-000000000000）；重试复用，不使用项目名或自定义文字。
    pub request_id: Uuid,
}

#[derive(Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Current,
    History,
    Evidence,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMemory {
    /// 仅填写记忆 UUID；secret://空间/UUID 是引用，须先核对空间并提取末段 UUID，不能把整个 URI 填入此字段。
    pub memory_id: Option<Uuid>,
    /// 仅填写原始来源事件 UUID，不填写网址或 Secret 引用。
    pub event_id: Option<Uuid>,
    #[serde(default)]
    pub view: View,
    pub cursor: Option<String>,
    /// 返回 JSON 的字符预算，1024～8000，默认 3000；不是 token 数。
    #[schemars(range(min = 1024, max = 8000))]
    pub max_chars: Option<usize>,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recall {
    /// 检索当前空间已保存记忆，最多 256 个 UTF-8 字节；启用模型后自动结合语义召回，否则使用关键词。
    pub query: String,
    pub project_key: Option<String>,
    /// 返回 JSON 的字符预算，1024～8000，默认 3000；不是 token 数。
    #[schemars(range(min = 1024, max = 8000))]
    pub max_chars: Option<usize>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: Uuid,
    pub client_type: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub role: String,
    pub body: String,
    pub project_key: Option<String>,
    pub occurred_at: String,
}

// 仅供独立 HTTP 的公开元数据；Secret 原文不进入可序列化的请求结构。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretCreate {
    pub label: String,
    /// 本次写操作的 UUID（如 01900000-0000-7000-8000-000000000000）；重试复用，不使用项目名或自定义文字。
    pub request_id: Uuid,
    pub project_key: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::secret_contract::query"
    )]
    pub contract: Option<crate::secret_contract::SecretContract>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretUpdate {
    /// 本次写操作的 UUID（如 01900000-0000-7000-8000-000000000000）；重试复用，不使用项目名或自定义文字。
    pub request_id: Uuid,
    pub expected_revision: i32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::secret_contract::query"
    )]
    pub contract: Option<crate::secret_contract::SecretContract>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretContractUpdate {
    pub request_id: Uuid,
    pub expected_revision: i32,
    pub contract: crate::secret_contract::SecretContract,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretReadQuery {
    pub expected_revision: Option<i32>,
    pub purpose: Option<String>,
}

pub fn bounded(value: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum {
        return Err(Error::invalid("字段为空或超过长度限制"));
    }
    Ok(())
}

pub fn output_budget(value: Option<usize>) -> Result<usize> {
    let value = value.unwrap_or(3000);
    if !(1024..=8000).contains(&value) {
        return Err(Error::invalid("max_chars 必须在 1024 至 8000 之间"));
    }
    Ok(value)
}

pub fn uuid_from_bytes(value: &[u8]) -> Result<Uuid> {
    Uuid::from_slice(value).map_err(|_| Error::unavailable())
}

pub fn now() -> time::PrimitiveDateTime {
    let current = time::OffsetDateTime::now_utc();
    time::PrimitiveDateTime::new(current.date(), current.time())
}

pub fn timestamp(value: time::PrimitiveDateTime) -> Result<String> {
    value
        .assume_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error::unavailable())
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn mcp_schemas_explain_uuid_identifiers_and_reject_human_readable_request_keys() {
        for schema in [
            schemars::schema_for!(Remember),
            schemars::schema_for!(Correct),
            schemars::schema_for!(Forget),
        ] {
            let value = schema.to_value();
            assert_eq!(value["properties"]["request_id"]["format"], "uuid");
            assert!(
                value["properties"]["request_id"]["description"]
                    .as_str()
                    .unwrap()
                    .contains("UUID")
            );
        }
        for schema in [
            schemars::schema_for!(GetMemory),
            schemars::schema_for!(Recall),
        ] {
            let value = schema.to_value();
            assert_eq!(value["properties"]["max_chars"]["minimum"], 1024);
            assert_eq!(value["properties"]["max_chars"]["maximum"], 8000);
        }
        let read = schemars::schema_for!(GetMemory).to_value();
        assert_eq!(read["properties"]["memory_id"]["format"], "uuid");
        assert_eq!(read["properties"]["event_id"]["format"], "uuid");
        let create = schemars::schema_for!(Remember).to_value();
        assert_eq!(
            create["properties"]["source_refs"]["items"]["format"],
            "uuid"
        );
        let correct = schemars::schema_for!(Correct).to_value();
        assert_eq!(
            correct["properties"]["source_refs"]["items"]["format"],
            "uuid"
        );
        assert!(
            serde_json::from_value::<Remember>(serde_json::json!({
                "content":"虚构记忆", "request_id":"bronze-trail-demo-product"
            }))
            .is_err()
        );
    }
}
