#![cfg_attr(not(test), forbid(unsafe_code))]
use serde::Deserialize;

#[derive(Clone, Default, Deserialize)]
pub struct Origin {
    pub agent_name: String,
}
#[derive(Clone, Deserialize)]
pub struct Conversation {
    pub agent_id: Option<String>,
    pub session_id: String,
    pub origin: Origin,
    pub preview: String,
    pub message_count: i64,
    pub received_at: String,
}
#[derive(Clone, Deserialize)]
pub struct Message {
    pub event_id: String,
    pub role: String,
    pub preview: String,
    pub truncated: bool,
    pub occurred_at: String,
    pub received_at: String,
    pub turn_id: Option<String>,
    pub project_key: Option<String>,
}
#[derive(Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: u64,
    pub has_more: bool,
    pub as_of: String,
}

/// 同一页面只有一个在途读取；切会话、筛选、离开页面都会作废旧响应。
#[derive(Default)]
pub struct Gate {
    generation: u32,
    pending: Option<(u32, String)>,
}
impl Gate {
    pub fn begin(&mut self, kind: &str) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, kind.into()));
        self.generation
    }
    pub fn accept(&mut self, generation: u32) -> Option<String> {
        if self.pending.as_ref().is_some_and(|p| p.0 == generation) {
            self.pending.take().map(|p| p.1)
        } else {
            None
        }
    }
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switching_conversations_or_leaving_cannot_restore_old_plaintext() {
        let mut gate = Gate::default();
        let old = gate.begin("detail");
        let next = gate.begin("conversation");
        assert!(gate.accept(old).is_none());
        assert_eq!(gate.accept(next).as_deref(), Some("conversation"));
        assert!(gate.accept(next).is_none());
        let old = gate.begin("detail");
        gate.clear();
        assert!(gate.accept(old).is_none());
    }
}
