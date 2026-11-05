#![cfg_attr(not(test), forbid(unsafe_code))]
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Export,
    Preview { page: usize, bound: bool },
    Confirm,
}
impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Preview { .. } => "preview",
            Self::Confirm => "confirm",
        }
    }
    pub fn unlock_label(self) -> &'static str {
        match self {
            Self::Export => "解锁并炼化",
            Self::Preview { .. } => "解锁并预览",
            Self::Confirm => "解锁并确认夺舍",
        }
    }
}

/// 只决定当前表单是否完整，不把另一个表单里的密码变成隐藏前置条件。
pub(crate) fn validate(
    action: Action,
    has_file: bool,
    phrase: &str,
    repeated: &str,
) -> Option<&'static str> {
    if matches!(action, Action::Preview { .. } | Action::Confirm) && !has_file {
        return Some("请先选择迁移包。");
    }
    if phrase.len() < 8 {
        return Some("迁移口令至少 8 字节，英文或数字需至少 8 位。");
    }
    if matches!(action, Action::Export) && phrase != repeated {
        return Some("两次迁移口令不一致，请检查后重试。");
    }
    None
}

#[derive(Deserialize)]
pub(crate) struct Status {
    pub unlocked: bool,
    pub expires_in: u32,
}
impl Status {
    pub fn parse(text: &str) -> Option<Self> {
        let status: Self = serde_json::from_str(text).ok()?;
        (status.expires_in <= 600 && status.unlocked == (status.expires_in > 0)).then_some(status)
    }
}

/// 浏览器只显示有界的状态快照；授权以服务端为准，倒计时不会续期。
#[derive(Default)]
pub(crate) struct Access {
    deadline_ms: f64,
    deferred: Option<Action>,
}
impl Access {
    pub fn receive(&mut self, status: Status, now: f64) {
        self.deadline_ms = if now.is_finite() {
            now + f64::from(status.expires_in) * 1000.0
        } else {
            0.0
        };
    }
    pub fn remaining(&self, now: f64) -> u32 {
        if !now.is_finite() {
            return 0;
        }
        ((self.deadline_ms - now) / 1000.0).ceil().clamp(0.0, 600.0) as u32
    }
    pub fn defer(&mut self, action: Option<Action>) {
        self.deferred = action;
    }
    pub fn take_deferred(&mut self) -> Option<Action> {
        self.deferred.take()
    }
    pub fn lock(&mut self) {
        self.deadline_ms = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_form_needs_no_space_password_and_errors_are_local() {
        let preview = Action::Preview {
            page: 0,
            bound: false,
        };
        assert_eq!(validate(preview, true, "12345678", ""), None);
        assert_eq!(
            validate(Action::Export, false, "12345678", "12345678"),
            None
        );
        assert!(
            validate(preview, false, "12345678", "")
                .unwrap()
                .contains("选择迁移包")
        );
        assert!(
            validate(preview, true, "1234567", "")
                .unwrap()
                .contains("至少")
        );
        assert!(
            validate(Action::Export, false, "12345678", "87654321")
                .unwrap()
                .contains("不一致")
        );
    }
    #[test]
    fn cancellation_discards_deferred_write_and_expiry_does_not_slide() {
        let mut access = Access::default();
        access.receive(
            Status::parse(r#"{"unlocked":true,"expires_in":600}"#).unwrap(),
            1000.0,
        );
        access.defer(Some(Action::Confirm));
        access.defer(None);
        assert_eq!(access.take_deferred(), None);
        assert_eq!(access.remaining(600999.0), 1);
        assert_eq!(access.remaining(601000.0), 0);
        access.defer(Some(Action::Confirm));
        assert_eq!(access.take_deferred(), Some(Action::Confirm));
        assert_eq!(access.take_deferred(), None);
        assert!(Status::parse(r#"{"unlocked":true,"expires_in":0}"#).is_none());
        assert!(Status::parse(r#"{"unlocked":true,"expires_in":601}"#).is_none());
    }
}
