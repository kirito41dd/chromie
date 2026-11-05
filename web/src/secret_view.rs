#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::space_key::SpaceKey;

pub struct Target {
    pub space: String,
    pub memory_id: String,
}

pub struct Request {
    pub space: String,
    pub memory_id: String,
    pub password: String,
    pub generation: u32,
}

/// 查看状态不持有凭据原文；关闭或隐藏都会使在途响应失效。
#[derive(Default)]
pub struct SecretView {
    target: Option<Target>,
    generation: u32,
    pending: bool,
    visible: bool,
}

impl SecretView {
    pub fn open(&mut self, space: &str, memory_id: &str) -> bool {
        self.close();
        if SpaceKey::parse(space).is_none() || !valid_memory_id(memory_id) {
            return false;
        }
        self.target = Some(Target {
            space: space.into(),
            memory_id: memory_id.into(),
        });
        true
    }

    pub fn request(&mut self, password: String, verified: bool) -> Option<Request> {
        let target = self.target.as_ref()?;
        if self.pending
            || self.visible
            || (password.is_empty() && !verified)
            || password.len() > 1024
            || password.chars().any(char::is_control)
        {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending = true;
        Some(Request {
            space: target.space.clone(),
            memory_id: target.memory_id.clone(),
            password,
            generation: self.generation,
        })
    }

    pub fn finish(&mut self, generation: u32, space: &str, success: bool) -> bool {
        if !self.pending
            || self.generation != generation
            || self
                .target
                .as_ref()
                .is_none_or(|target| target.space != space)
        {
            return false;
        }
        self.pending = false;
        self.visible = success;
        true
    }

    pub fn hide(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = false;
        self.visible = false;
    }

    /// 只有仍在显示的这一代原文可以主动复制，隐藏后的异步提示一律失效。
    pub fn visible_generation(&self) -> Option<u32> {
        self.visible.then_some(self.generation)
    }

    pub fn close(&mut self) {
        self.hide();
        self.target = None;
    }
}

fn valid_memory_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "01900000-0000-7000-8000-000000000000";

    #[test]
    fn copying_requires_a_visible_value_and_old_copy_results_expire_with_it() {
        let mut state = SecretView::default();
        assert!(state.open("demo", ID));
        assert!(state.visible_generation().is_none());
        let request = state.request("虚构验密".into(), false).unwrap();
        assert!(state.visible_generation().is_none());
        assert!(state.finish(request.generation, "demo", true));
        assert_eq!(state.visible_generation(), Some(request.generation));
        state.hide();
        assert!(state.visible_generation().is_none());
        let next = state.request("虚构验密".into(), false).unwrap();
        assert!(state.finish(next.generation, "demo", true));
        assert_ne!(state.visible_generation(), Some(request.generation));
        state.close();
        assert!(state.visible_generation().is_none());
    }

    #[test]
    fn only_verified_reveals_can_omit_password_and_failed_passwords_can_retry() {
        let mut state = SecretView::default();
        assert!(state.open("demo", ID));
        assert!(!state.visible);
        assert!(state.request(String::new(), false).is_none());
        let denied = state.request("虚构错误密码".into(), false).unwrap();
        assert!(state.request("重复点击".into(), false).is_none());
        assert!(state.finish(denied.generation, "demo", false));
        assert!(!state.visible);
        let accepted = state.request("虚构有效密码".into(), false).unwrap();
        assert_eq!(accepted.space, "demo");
        assert_eq!(accepted.memory_id, ID);
        assert_eq!(accepted.password, "虚构有效密码");
        assert!(state.finish(accepted.generation, "demo", true));
        assert!(state.request("重复显示".into(), true).is_none());
        state.hide();
        assert!(state.request(String::new(), false).is_none());
        let reused = state.request(String::new(), true).unwrap();
        assert!(reused.password.is_empty());
        assert!(state.finish(reused.generation, "demo", true));
        state.hide();
        assert!(state.request("重新输入".into(), false).is_some());
    }

    #[test]
    fn hiding_closing_and_switching_targets_discard_late_plaintext() {
        let mut state = SecretView::default();
        assert!(state.open("demo", ID));
        let first = state.request("虚构密码".into(), false).unwrap();
        state.hide();
        assert!(!state.finish(first.generation, "demo", true));
        let second = state.request("虚构密码".into(), false).unwrap();
        assert!(!state.finish(second.generation, "other", true));
        state.close();
        assert!(!state.finish(second.generation, "demo", true));
        assert!(state.request("虚构密码".into(), true).is_none());
        assert!(state.open("other", ID));
        let third = state.request("虚构密码".into(), false).unwrap();
        assert!(!state.finish(second.generation, "other", true));
        assert!(state.finish(third.generation, "other", true));
        assert!(!state.open("../demo", ID));
        assert!(!state.open("demo", "../secret"));
        assert!(!state.visible);
    }
}
