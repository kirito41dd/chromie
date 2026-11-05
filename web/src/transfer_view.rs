#![cfg_attr(not(test), forbid(unsafe_code))]
use serde::Deserialize;
#[derive(Deserialize)]
pub(crate) struct Reply {
    pub source_space: String,
    pub source_name: String,
    pub new: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub total: usize,
    #[serde(default)]
    pub imported: Option<usize>,
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub preview_digest: String,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub has_more: bool,
}
#[derive(Deserialize)]
pub(crate) struct Item {
    pub kind: String,
    pub excerpt: String,
    pub status: String,
    pub reason: String,
    pub project_key: Option<String>,
    pub contract: Option<serde_json::Value>,
}
impl Item {
    pub fn metadata(&self) -> String {
        let scope = self
            .project_key
            .as_ref()
            .map(|p| format!("项目 · {p}"))
            .unwrap_or_else(|| "空间通用".into());
        let Some(contract) = &self.contract else {
            return scope;
        };
        let format = match contract["format"].as_str() {
            Some("text") => match contract["kind"].as_str() {
                Some("password") => "文本密码",
                Some("api_token") => "文本 Token",
                _ => "文本",
            },
            Some("json") => "JSON",
            Some("binary") => "二进制",
            _ => "未知格式",
        };
        let fields = contract["fields"].as_object().map(|fields| {
            fields
                .iter()
                .map(|(role, field)| {
                    format!(
                        "{} → {} ({})",
                        role,
                        field["pointer"].as_str().unwrap_or("?"),
                        field["type"].as_str().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join("；")
        });
        format!(
            "{scope} · {format}{}",
            fields
                .map(|v| format!("\n字段用途：{v}"))
                .unwrap_or_default()
        )
    }
}
impl Reply {
    pub fn parse(text: &str) -> Option<Self> {
        let reply: Self = serde_json::from_str(text).ok()?;
        if reply.total > 10000
            || reply.items.len() > 100
            || reply
                .new
                .checked_add(reply.skipped)?
                .checked_add(reply.conflicts)?
                != reply.total
        {
            return None;
        }
        if !reply.completed()
            && (reply.preview_digest.len() != 64
                || !reply.preview_digest.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return None;
        }
        if !reply.completed()
            && let Some(size) = reply.page_size
        {
            if !(1..=100).contains(&size) {
                return None;
            }
            let start = reply.page.checked_mul(size)?;
            if start > reply.total
                || (start == reply.total && reply.page != 0)
                || reply.items.len() != (reply.total - start).min(size)
                || reply.has_more != (start + reply.items.len() < reply.total)
            {
                return None;
            }
        }
        Some(reply)
    }
    pub fn completed(&self) -> bool {
        self.imported.is_some() || self.replayed
    }
    pub fn can_confirm(&self) -> bool {
        !self.completed() && self.new > 0
    }
    pub fn stage(&self) -> &'static str {
        if self.completed() {
            "夺舍已完成"
        } else if self.new == 0 {
            "无需夺舍"
        } else {
            "等待确认 · 尚未导入"
        }
    }
    pub fn detail(&self) -> String {
        if self.completed() {
            if self.replayed {
                "此包已处理，未重复写入。"
            } else {
                "索引将在后台自动建立。"
            }
            .into()
        } else if self.total == 0 {
            "迁移包为空，当前空间没有变化。".into()
        } else if self.new == 0 {
            "没有可新增项。相同记忆已跳过，冲突内容保留现状；当前空间没有变化。".into()
        } else {
            format!(
                "确认后导入整个包中的 {} 条新增项，不只是当前页。跳过和冲突项保持现状。",
                self.new
            )
        }
    }
    pub fn page_note(&self) -> String {
        match self.page_size.filter(|_| !self.completed()) {
            Some(size) if self.total > 0 => format!(
                "第 {} / {} 页 · 共 {} 条",
                self.page + 1,
                self.total.div_ceil(size),
                self.total
            ),
            _ => String::new(),
        }
    }
    pub fn summary(&self) -> String {
        format!(
            "{} {} 条 · 跳过 {} 条 · 冲突 {} 条{}",
            if self.completed() {
                "已迁入"
            } else {
                "可新增"
            },
            self.imported.unwrap_or(self.new),
            self.skipped,
            self.conflicts,
            if self.truncated && !self.completed() && self.page_size.is_none() {
                "（此服务仅返回前 100 条）"
            } else {
                ""
            }
        )
    }
}
pub(crate) fn error(status: u16, text: &str) -> &'static str {
    let code = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v["code"].as_str().map(str::to_owned));
    match code.as_deref() {
        Some("transfer_decryption_failed") => "迁移口令不匹配，或迁移包已损坏／截断。",
        Some("transfer_invalid_package") => "迁移包格式或版本不受支持，请确认文件完整。",
        Some("transfer_preview_changed") => "目标内容已变化，请重新预览后再确认。",
        Some("transfer_package_conflict") => "同一迁移包标识对应了不同内容，已拒绝导入。",
        Some("transfer_busy") => "另一项迁移正在处理，请稍后重试。",
        _ => match status {
            401 => "登录态或空间密码无效，请检查后重试。",
            413 => "文件超过 65 MiB 上限。",
            0 => "请求中断或超时，请重试；重复导入会自动跳过。",
            _ => "迁移未完成，请检查文件、口令和服务状态后重试。",
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_and_receipt_have_distinct_confirmation_state() {
        let text = format!(
            r#"{{"source_space":"work","source_name":"虚构","new":1,"skipped":2,"conflicts":1,"total":4,"preview_digest":"{}"}}"#,
            "a".repeat(64)
        );
        let preview = Reply::parse(&text).unwrap();
        assert!(!preview.completed());
        assert!(preview.summary().contains("冲突 1"));
        assert!(Reply::parse(&text.replace("\"new\":1", "\"new\":2")).is_none());
        let replay =
            Reply::parse(&text.replace("\"new\":1", "\"replayed\":true,\"new\":1")).unwrap();
        assert!(replay.completed());
        assert_eq!(
            error(409, r#"{"code":"transfer_preview_changed"}"#),
            "目标内容已变化，请重新预览后再确认。"
        );
    }
    #[test]
    fn metadata_explains_scope_and_contract_without_secret_values() {
        let item:Item = serde_json::from_value(serde_json::json!({"kind":"secret","excerpt":"虚构数据库","status":"new","reason":"新记忆","project_key":"soul-demo",
            "contract":{"format":"json","version":1,"fields":{"password":{"pointer":"/mysql/password","type":"string"}}}})).unwrap();
        assert!(item.metadata().contains("项目 · soul-demo · JSON"));
        assert!(
            item.metadata()
                .contains("password → /mysql/password (string)")
        );
    }
    #[test]
    fn paged_preview_distinguishes_no_changes_from_ready_and_keeps_full_package_counts() {
        let item = serde_json::json!({"kind":"memory","excerpt":"虚构记忆","status":"existing","reason":"相同记忆已存在","project_key":null,"contract":null});
        let mut value = serde_json::json!({"source_space":"work","source_name":"虚构","new":0,"skipped":29,"conflicts":0,"total":29,
            "preview_digest":"a".repeat(64),"page":0,"page_size":5,"has_more":true,"items":vec![item.clone();5]});
        let none = Reply::parse(&value.to_string()).unwrap();
        assert!(!none.can_confirm());
        assert_eq!(none.stage(), "无需夺舍");
        assert!(none.detail().contains("当前空间没有变化"));
        assert_eq!(none.page_note(), "第 1 / 6 页 · 共 29 条");
        value["new"] = 1.into();
        value["skipped"] = 28.into();
        let ready = Reply::parse(&value.to_string()).unwrap();
        assert!(
            ready.can_confirm(),
            "即便当前页全部跳过，全包有新增时也可确认"
        );
        assert!(ready.detail().contains("整个包中的 1 条"));
        value["page"] = 5.into();
        value["has_more"] = false.into();
        value["items"] = serde_json::json!(vec![item; 4]);
        assert_eq!(
            Reply::parse(&value.to_string()).unwrap().page_note(),
            "第 6 / 6 页 · 共 29 条"
        );
        value["page"] = 6.into();
        assert!(Reply::parse(&value.to_string()).is_none());
    }
}
