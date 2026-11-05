//! 客户端与服务端共用的接入密钥用途；不包含服务端实现。
#[cfg(feature = "server")]
use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Mcp,
    Hook,
}

impl KeyKind {
    #[cfg(feature = "server")]
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "mcp" => Ok(Self::Mcp),
            "hook" => Ok(Self::Hook),
            _ => Err(Error::invalid("密钥用途须为 mcp 或 hook")),
        }
    }
    #[cfg(feature = "server")]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Hook => "hook",
        }
    }
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Mcp => "cm_mcp_",
            Self::Hook => "cm_hook_",
        }
    }
}
