//! 取用审计的公开元数据；不能承载命令、输出或凭据原文。
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub fn validate_purpose(value: Option<&str>) -> Result<()> {
    if value
        .is_some_and(|v| v.trim().is_empty() || v.len() > 256 || v.chars().any(char::is_control))
    {
        return Err(Error::invalid(
            "公开用途须为 1～256 字节单行文本，不能填写凭据原文",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Succeeded,
    Failed,
    TimedOut,
    Interrupted,
    SpawnFailed,
}

impl ExecutionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
            Self::SpawnFailed => "spawn_failed",
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReport {
    pub outcome: ExecutionOutcome,
    pub exit_code: Option<i32>,
}

impl ExecutionReport {
    pub fn validate(&self) -> Result<()> {
        let valid = match self.outcome {
            ExecutionOutcome::Succeeded => self.exit_code == Some(0),
            ExecutionOutcome::Failed => self.exit_code.is_none_or(|n| n != 0),
            _ => self.exit_code.is_none(),
        };
        if !valid {
            return Err(Error::invalid("执行状态与退出码不一致"));
        }
        Ok(())
    }
}
