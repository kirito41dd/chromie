#![cfg_attr(not(test), forbid(unsafe_code))]

use serde::Deserialize;
use slint::{Model, ModelRc, VecModel};

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub agent_recall_count: u64,
    pub evaluated_count: u64,
    pub useful_count: u64,
    pub web_recall_count: u64,
}

impl Usage {
    pub fn label(&self) -> String {
        if self.evaluated_count > 0 {
            format!(
                "召回 {} · 有用 {} / 已评 {}",
                self.agent_recall_count, self.useful_count, self.evaluated_count
            )
        } else if self.agent_recall_count > 0 {
            format!("召回 {} · 未评价", self.agent_recall_count)
        } else {
            "召回 0".into()
        }
    }

    pub fn detail(&self) -> String {
        let evaluation = if self.evaluated_count == 0 {
            "当前修订尚未评价".into()
        } else {
            format!(
                "当前修订已评价 {} 次，其中 {} 次标记有用",
                self.evaluated_count, self.useful_count
            )
        };
        format!(
            "Agent 累计召回 {} 次（含历史修订）；{evaluation}。",
            self.agent_recall_count
        )
    }
}

pub fn order(index: i32) -> &'static str {
    match index {
        1 => "popular",
        2 => "useful",
        _ => "recent",
    }
}

/// 未改变的卡片保留原控件，避免看板轮询重建选区和焦点。
pub(crate) fn reconcile_rows(
    current: &ModelRc<crate::MemoryRow>,
    rows: Vec<crate::MemoryRow>,
) -> Option<ModelRc<crate::MemoryRow>> {
    let same_ids = current.row_count() == rows.len()
        && rows.iter().enumerate().all(|(i, row)| {
            current
                .row_data(i)
                .is_some_and(|old| old.memory_id == row.memory_id)
        });
    if same_ids
        && let Some(model) = current
            .as_any()
            .downcast_ref::<VecModel<crate::MemoryRow>>()
    {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
        None
    } else {
        Some(ModelRc::new(VecModel::from(rows)))
    }
}

/// 只接受服务端类型与密文存在标记，不根据正文或引用文字推断 Secret。
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct SecretMetadata {
    pub kind: String,
    pub secret_ref: Option<String>,
    pub secret_stored: Option<bool>,
}

impl SecretMetadata {
    pub fn is_secret(&self) -> bool {
        self.kind == "secret_ref"
    }

    pub fn stored(&self) -> bool {
        self.is_secret() && self.secret_stored == Some(true)
    }

    pub fn status(&self) -> &'static str {
        if !self.is_secret() {
            return "";
        }
        match self.secret_stored {
            Some(true) => "密文已保存",
            Some(false) => "未发现密文",
            None => "存储状态未提供",
        }
    }

    pub fn reference(&self) -> &str {
        if self.is_secret() {
            self.secret_ref.as_deref().unwrap_or_default()
        } else {
            ""
        }
    }
}

pub fn secret_summary(count: Option<u64>) -> String {
    count
        .map(|count| format!("含 {count} 项 Secret"))
        .unwrap_or_else(|| "当前有效记忆".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usefulness_uses_evaluations_as_denominator_and_unknown_stays_unknown() {
        let unknown = Usage {
            agent_recall_count: 10,
            ..Default::default()
        };
        assert!(unknown.label().contains("未评价"));
        assert!(unknown.detail().contains("当前修订尚未评价"));
        let assessed = Usage {
            agent_recall_count: 10,
            evaluated_count: 6,
            useful_count: 4,
            web_recall_count: 9,
        };
        assert_eq!(assessed.label(), "召回 10 · 有用 4 / 已评 6");
        assert!(
            assessed
                .detail()
                .contains("当前修订已评价 6 次，其中 4 次标记有用")
        );
        assert_eq!(order(0), "recent");
        assert_eq!(order(1), "popular");
        assert_eq!(order(2), "useful");
    }

    #[test]
    fn dashboard_refresh_keeps_unchanged_rows_and_updates_only_changed_content() {
        let first = crate::MemoryRow {
            memory_id: "fictional-first".into(),
            content: "虚构正文，保留选区。".into(),
            ..Default::default()
        };
        let second = crate::MemoryRow {
            memory_id: "fictional-second".into(),
            content: "另一条虚构正文。".into(),
            ..Default::default()
        };
        let model = ModelRc::new(VecModel::from(vec![first.clone(), second.clone()]));
        assert!(reconcile_rows(&model, vec![first.clone(), second.clone()]).is_none());
        let mut updated = second.clone();
        updated.pending = true;
        assert!(reconcile_rows(&model, vec![first.clone(), updated.clone()]).is_none());
        assert_eq!(model.row_data(0), Some(first.clone()));
        assert_eq!(model.row_data(1), Some(updated.clone()));
        let reordered = reconcile_rows(&model, vec![updated, first]).unwrap();
        assert_eq!(reordered.row_data(0).unwrap().memory_id, second.memory_id);
        let removed = reconcile_rows(&reordered, vec![]).unwrap();
        assert_eq!(removed.row_count(), 0);
    }

    #[test]
    fn ordinary_notes_about_secrets_are_not_encrypted_secret_records() {
        let normal: SecretMetadata = serde_json::from_str(
            r#"{"kind":"memory","content":"凭据以 Secret 保存","secret_ref":"secret://demo/example"}"#,
        ).unwrap();
        assert!(!normal.is_secret());
        assert_eq!(normal.reference(), "");
        assert_eq!(normal.status(), "");
        assert_eq!(secret_summary(None), "当前有效记忆");
        assert_eq!(secret_summary(Some(0)), "含 0 项 Secret");
        assert_eq!(secret_summary(Some(1)), "含 1 项 Secret");
    }

    #[test]
    fn stored_unavailable_and_older_secret_metadata_have_distinct_statuses() {
        let mut metadata: SecretMetadata = serde_json::from_str(
            r#"{"kind":"secret_ref","secret_stored":true,"secret_ref":"secret://demo/example"}"#,
        )
        .unwrap();
        assert!(metadata.is_secret() && metadata.stored());
        assert_eq!(metadata.status(), "密文已保存");
        assert_eq!(metadata.reference(), "secret://demo/example");
        metadata.secret_stored = Some(false);
        assert!(!metadata.stored());
        assert_eq!(metadata.status(), "未发现密文");
        metadata.secret_stored = None;
        assert_eq!(metadata.status(), "存储状态未提供");
    }
}

#[derive(serde::Deserialize)]
pub struct Origin {
    pub agent_name: String,
}
impl Default for Origin {
    fn default() -> Self {
        Self {
            agent_name: "历史记录".into(),
        }
    }
}
