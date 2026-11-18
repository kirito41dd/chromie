use crate::error::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOrder {
    #[default]
    Recent,
    Popular,
    Useful,
}

#[derive(Default, Debug, Serialize)]
pub struct MemoryUsage {
    pub agent_recall_count: u64,
    pub evaluated_count: u64,
    pub useful_count: u64,
    pub web_recall_count: u64,
    pub last_recalled_at: Option<String>,
    pub evaluated_revision: i32,
    pub used_count: u64,
    pub succeeded_count: u64,
    pub outdated_count: u64,
    pub environment_blocked_count: u64,
    pub unverified_count: u64,
}

/// 仅表示来源 Agent 的使用报告，不等于独立验证，也不决定该记忆的事实真伪。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UseOutcome {
    Succeeded,
    ContentOutdated,
    EnvironmentBlocked,
    Unverified,
}
impl UseOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::ContentOutdated => "content_outdated",
            Self::EnvironmentBlocked => "environment_blocked",
            Self::Unverified => "unverified",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "content_outdated" => Ok(Self::ContentOutdated),
            "environment_blocked" => Ok(Self::EnvironmentBlocked),
            "unverified" => Ok(Self::Unverified),
            _ => Err(Error::unavailable()),
        }
    }
}
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryUse {
    pub memory_id: Uuid,
    pub outcome: UseOutcome,
    /// 公开简短原因；不附日志、正文或凭据。修订与 Agent 身份由原召回记录确定。
    pub reason: Option<String>,
}

/// 表示本次任务与已返回内容的关系，仅用于汇总召回质量。
#[derive(Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeReason {
    /// 对象、项目或环境与任务不符。
    ScopeMismatch,
    /// 需要的结果类型不同，例如需要凭据引用却只找到普通说明。
    WrongResultType,
    /// 对象相关但缺少回答本次问题的细节。
    MissingDetail,
}

impl NegativeReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ScopeMismatch => "scope_mismatch",
            Self::WrongResultType => "wrong_result_type",
            Self::MissingDetail => "missing_detail",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NegativeFeedback {
    pub memory_id: Uuid,
    pub reason: NegativeReason,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallEvaluation {
    pub recall_id: Uuid,
    /// 本次召回中实际做过判断的 ID；未评价的不要填写。
    #[schemars(length(max = 10))]
    #[serde(default)]
    pub evaluated_memory_ids: Vec<Uuid>,
    /// 对当前任务有帮助的 ID，必须是 evaluated_memory_ids 的子集。
    #[schemars(length(max = 10))]
    #[serde(default)]
    pub useful_memory_ids: Vec<Uuid>,
    /// 可选。仅对本次已评价且无用的条目给出一个原因；不确定时省略。
    #[serde(default)]
    #[schemars(length(max = 10))]
    pub negative_reasons: Vec<NegativeFeedback>,
    /// 只报告确实使用的条目，与“有用”判断独立；任务结束时报告最终观察。
    #[serde(default)]
    #[schemars(length(max = 10))]
    pub usage: Vec<MemoryUse>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRecall {
    /// 合并当前任务的评价；一次最多 20 次召回，不提交正文、查询或凭据。
    #[schemars(length(min = 1, max = 20))]
    pub recalls: Vec<RecallEvaluation>,
}

impl FeedbackRecall {
    pub fn validate(&self) -> Result<()> {
        if self.recalls.is_empty() || self.recalls.len() > 20 {
            return Err(Error::invalid("一次反馈需要包含 1～20 次召回"));
        }
        let mut recalls = HashSet::new();
        for item in &self.recalls {
            let evaluated: HashSet<_> = item.evaluated_memory_ids.iter().collect();
            let useful: HashSet<_> = item.useful_memory_ids.iter().collect();
            if !recalls.insert(item.recall_id)
                || item.evaluated_memory_ids.len() > 10
                || item.useful_memory_ids.len() > 10
                || evaluated.len() != item.evaluated_memory_ids.len()
                || useful.len() != item.useful_memory_ids.len()
                || !useful.is_subset(&evaluated)
            {
                return Err(Error::invalid(
                    "评价 ID 必须唯一，每次最多 10 条，有用条目须属于已评价条目",
                ));
            }
            let mut explained = HashSet::new();
            if item.negative_reasons.len() > 10
                || item.negative_reasons.iter().any(|entry| {
                    !evaluated.contains(&entry.memory_id)
                        || useful.contains(&entry.memory_id)
                        || !explained.insert(entry.memory_id)
                })
            {
                return Err(Error::invalid(
                    "无用原因仅可填写本次已评价且无用的条目，每条一个原因，最多 10 条",
                ));
            }
            let mut used = HashSet::new();
            if item.usage.len() > 10 {
                return Err(Error::invalid("每次最多报告10条实际使用记录"));
            }
            for usage in &item.usage {
                if !used.insert(usage.memory_id)
                    || usage.reason.as_ref().is_some_and(|r| {
                        r.trim().is_empty() || r.len() > 512 || r.chars().any(|c| c.is_control())
                    })
                {
                    return Err(Error::invalid(
                        "使用条目须唯一，原因须为不含控制字符的1～512字节公开说明",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_is_not_negative_and_useful_must_have_been_evaluated() {
        let run = Uuid::now_v7();
        let id = Uuid::now_v7();
        let request = |evaluated, useful| {
            serde_json::from_value::<FeedbackRecall>(json!({
            "recalls":[{"recall_id":run,"evaluated_memory_ids":evaluated,"useful_memory_ids":useful}]
        })).unwrap()
        };
        assert!(request(json!([]), json!([])).validate().is_ok());
        assert!(request(json!([id]), json!([])).validate().is_ok());
        assert!(request(json!([id]), json!([id])).validate().is_ok());
        assert!(request(json!([]), json!([id])).validate().is_err());
        assert!(request(json!([id, id]), json!([])).validate().is_err());
        assert!(request(json!([id]), json!([id, id])).validate().is_err());
        let excessive = (0..11).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
        assert!(request(json!(excessive), json!([])).validate().is_err());
        assert!(
            serde_json::from_value::<FeedbackRecall>(json!({"recalls":[],"agent_id":id})).is_err()
        );
    }
    #[test]
    fn reasons_only_explain_explicit_negative_evaluations() {
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let run = Uuid::now_v7();
        let request = |evaluated, useful, reasons| {
            serde_json::from_value::<FeedbackRecall>(json!({
                "recalls":[{"recall_id":run,"evaluated_memory_ids":evaluated,"useful_memory_ids":useful,"negative_reasons":reasons}]
            }))
        };
        for reason in ["scope_mismatch", "wrong_result_type", "missing_detail"] {
            let entry = json!({"memory_id":id,"reason":reason});
            assert!(
                request(json!([id]), json!([]), json!([entry]))
                    .unwrap()
                    .validate()
                    .is_ok()
            );
            assert!(
                request(json!([other]), json!([]), json!([entry]))
                    .unwrap()
                    .validate()
                    .is_err()
            );
            assert!(
                request(json!([id]), json!([id]), json!([entry]))
                    .unwrap()
                    .validate()
                    .is_err()
            );
            assert!(
                request(json!([id]), json!([]), json!([entry, entry]))
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(
            request(
                json!([id]),
                json!([]),
                json!([{"memory_id":id,"reason":"free text"}])
            )
            .is_err()
        );
    }
}
