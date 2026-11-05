//! 只含公开结构的 Secret 契约。解析错误不携带原文或字段值。
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 客户端与服务端一致的 Secret 原文字节上限。
pub const MAX_SECRET_BYTES: usize = 60000;

pub const MAX_CONTRACT_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretContract {
    Unknown {
        version: u8,
    },
    Text {
        version: u8,
        kind: TextKind,
    },
    Json {
        version: u8,
        fields: BTreeMap<String, JsonField>,
    },
    Binary {
        version: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextKind {
    Password,
    ApiToken,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonField {
    pub pointer: String,
    #[serde(rename = "type")]
    pub value_type: JsonType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    Null,
}

impl Default for SecretContract {
    fn default() -> Self {
        Self::Unknown { version: 1 }
    }
}

impl SecretContract {
    pub fn format(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "unknown",
            Self::Text { .. } => "text",
            Self::Json { .. } => "json",
            Self::Binary { .. } => "binary",
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CONTRACT_BYTES {
            return Err(Error::invalid("Secret 契约超过 4 KiB"));
        }
        let contract: Self = serde_json::from_slice(bytes)
            .map_err(|_| Error::invalid("Secret 契约 JSON 无效，未输出输入内容"))?;
        contract.validate()?;
        Ok(contract)
    }
    pub fn from_storage(value: Option<&Value>) -> Result<Self> {
        match value {
            None => Ok(Self::default()),
            Some(value) => {
                Self::parse(&serde_json::to_vec(value).map_err(|_| Error::unavailable())?)
            }
        }
    }
    pub fn value(&self) -> Result<Value> {
        self.validate()?;
        serde_json::to_value(self).map_err(|_| Error::unavailable())
    }
    pub fn validate(&self) -> Result<()> {
        let version = match self {
            Self::Unknown { version }
            | Self::Text { version, .. }
            | Self::Json { version, .. }
            | Self::Binary { version } => *version,
        };
        if version != 1 {
            return Err(Error::invalid("Secret 契约版本不受支持"));
        }
        if serde_json::to_vec(self)
            .map_err(|_| Error::unavailable())?
            .len()
            > MAX_CONTRACT_BYTES
        {
            return Err(Error::invalid("Secret 契约超过 4 KiB"));
        }
        if let Self::Json { fields, .. } = self {
            if fields.is_empty() || fields.len() > 16 {
                return Err(Error::invalid("JSON 契约须声明 1～16 个字段"));
            }
            for (role, field) in fields {
                if role.is_empty()
                    || role.len() > 64
                    || !role
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    return Err(Error::invalid(
                        "字段用途须为不超过 64 字节的小写英文、数字或下划线",
                    ));
                }
                if field.pointer.len() > 256
                    || (!field.pointer.is_empty() && !field.pointer.starts_with('/'))
                    || field.pointer.chars().any(char::is_control)
                {
                    return Err(Error::invalid("JSON 字段路径无效或超过 256 字节"));
                }
                let mut chars = field.pointer.chars();
                while let Some(c) = chars.next() {
                    if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                        return Err(Error::invalid("JSON 字段路径的转义无效"));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn validate_payload(&self, bytes: &[u8]) -> Result<()> {
        self.validate()?;
        match self {
            Self::Text { .. } => {
                std::str::from_utf8(bytes).map_err(|_| mismatch())?;
            }
            Self::Json { fields, .. } => {
                let value: Value = serde_json::from_slice(bytes).map_err(|_| mismatch())?;
                for field in fields.values() {
                    let actual = value.pointer(&field.pointer).ok_or_else(mismatch)?;
                    if !field.value_type.matches(actual) {
                        return Err(mismatch());
                    }
                }
            }
            Self::Unknown { .. } | Self::Binary { .. } => {}
        }
        Ok(())
    }
}

fn mismatch() -> Error {
    Error {
        code: "secret_contract_mismatch",
        message: "Secret 编码或声明字段与契约不匹配，未输出原文或值",
        status: reqwest::StatusCode::BAD_REQUEST,
    }
}

impl JsonType {
    fn matches(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Self::String, Value::String(_))
                | (Self::Number, Value::Number(_))
                | (Self::Boolean, Value::Bool(_))
                | (Self::Object, Value::Object(_))
                | (Self::Array, Value::Array(_))
                | (Self::Null, Value::Null)
        )
    }
    fn of(value: &Value) -> Self {
        match value {
            Value::String(_) => Self::String,
            Value::Number(_) => Self::Number,
            Value::Bool(_) => Self::Boolean,
            Value::Object(_) => Self::Object,
            Value::Array(_) => Self::Array,
            Value::Null => Self::Null,
        }
    }
}

/// 显式本地检查只产生结构清单；调用方只能写私有文件，不能自动发给模型。
pub fn inspect_json(bytes: &[u8]) -> Result<Value> {
    fn visit(value: &Value, pointer: String, depth: usize, out: &mut Vec<Value>) -> Result<()> {
        if depth > 16 || out.len() >= 256 || pointer.len() > 1024 {
            return Err(Error::invalid("本地结构清单超过层级或字段上限"));
        }
        out.push(serde_json::json!({"pointer":pointer,"type":JsonType::of(value)}));
        match value {
            Value::Object(fields) => {
                for (key, item) in fields {
                    visit(
                        item,
                        format!("{}/{}", pointer, key.replace('~', "~0").replace('/', "~1")),
                        depth + 1,
                        out,
                    )?;
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    visit(item, format!("{pointer}/{index}"), depth + 1, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| mismatch())?;
    let mut fields = Vec::new();
    visit(&value, String::new(), 0, &mut fields)?;
    let result = serde_json::json!({"format":"json","fields":fields,"notice":"仅供本地审阅；选择允许公开的路径并填写用途后才能形成契约"});
    if result.to_string().len() > 60000 {
        return Err(Error::invalid("本地结构清单超过大小上限"));
    }
    Ok(result)
}

/// URL query 中的契约是字符串，正文仍保持二进制；缺省保持旧客户端语义。
pub mod query {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        value: &Option<SecretContract>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer
                .serialize_some(&serde_json::to_string(value).map_err(serde::ser::Error::custom)?),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<SecretContract>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| {
                SecretContract::parse(text.as_bytes())
                    .map_err(|_| serde::de::Error::custom("Secret 契约无效"))
            })
            .transpose()
    }
}
