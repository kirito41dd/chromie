use crate::{
    error::{Error, Result},
    types::SpaceId,
};
use axum::http::{HeaderMap, header};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::agent_key::KeyKind;

/// 仅由成功鉴权产生；不包含原始密钥，也不允许从请求正文反序列化。
#[derive(Clone)]
pub struct AgentAccess {
    pub(crate) space: SpaceId,
    pub(crate) id: Uuid,
    pub(crate) kind: KeyKind,
    pub(crate) hash: Vec<u8>,
    pub(crate) revision: i32,
    pub(crate) client_type: String,
}

impl AgentAccess {
    pub fn agent_id(&self) -> Uuid {
        self.id
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgent {
    pub agent_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAgent {
    pub agent_name: Option<String>,
    pub enabled: Option<bool>,
    pub allow_secrets: Option<bool>,
}

#[derive(Serialize)]
pub struct AgentRecord {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub client_type: String,
    pub enabled: bool,
    pub allow_secrets: bool,
    pub created_at: String,
    pub last_mcp_at: Option<String>,
    pub last_hook_at: Option<String>,
}

pub(crate) fn bearer(headers: &HeaderMap) -> Result<Zeroizing<String>> {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || headers.contains_key("x-chromie-password")
    {
        return Err(Error::agent_unauthorized());
    }
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() <= 128 && !v.contains(char::is_whitespace))
        .ok_or_else(Error::agent_unauthorized)?;
    Ok(Zeroizing::new(value.to_owned()))
}
