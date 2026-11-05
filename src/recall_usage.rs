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
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallEvaluation {
    pub recall_id: Uuid,
    /// 本次召回中实际做过判断的 ID；未评价的不要填写。
    #[schemars(length(max = 10))]
    pub evaluated_memory_ids: Vec<Uuid>,
    /// 对当前任务有帮助的 ID，必须是 evaluated_memory_ids 的子集。
    #[schemars(length(max = 10))]
    pub useful_memory_ids: Vec<Uuid>,
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
}
