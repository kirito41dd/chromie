#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::shell_commands::Shell;
use serde::Deserialize;
use serde_json::json;

#[cfg(target_arch = "wasm32")]
pub const AGENT_GUIDANCE: &str = include_str!("../agent-guidance.txt");

pub struct Instructions {
    pub install: String,
    pub initialize: String,
    pub manual_mcp: String,
    pub hook: String,
    pub repair: String,
    pub path: String,
    pub path_note: String,
}

pub fn instructions(origin: &str, space: &str, codex: bool, nushell: bool) -> Instructions {
    let shell = if nushell { Shell::Nushell } else { Shell::Bash };
    Instructions {
        install:shell.install(origin),
        initialize:format!("{} --reconnect",shell.initialize(origin,space,codex)),
        manual_mcp:serde_json::to_string_pretty(&json!({"transport":"streamable-http","url":format!("{origin}/spaces/{space}/mcp"),"headers":{"Authorization":"Bearer 填入此接入的MCP密钥"}})).unwrap_or_default(),
        hook:shell.capture(codex),repair:shell.repair(codex),path:shell.path_line().into(),path_note:shell.path_note().into(),
    }
}

#[derive(Clone, Deserialize)]
pub struct Agent {
    pub agent_id: String,
    pub agent_name: String,
    pub enabled: bool,
    pub allow_secrets: bool,
    pub last_mcp_at: Option<String>,
    pub last_hook_at: Option<String>,
}

impl Agent {
    pub fn status(&self) -> &'static str {
        if !self.enabled {
            "已停用"
        } else if self.last_mcp_at.is_some() || self.last_hook_at.is_some() {
            "有请求记录"
        } else {
            "等待连接"
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum PairPhase {
    #[default]
    None,
    Loading,
    Ready,
    Approved,
    Ended,
    Failed,
}

#[derive(Default)]
pub struct Pairing {
    pub id: String,
    pub phase: PairPhase,
    pub code: String,
    pub client: String,
    pub approved_agent: Option<String>,
    deadline_ms: f64,
}

#[derive(Deserialize)]
pub struct PairInfo {
    pub pairing_id: String,
    pub code: String,
    pub client: String,
    pub approved: bool,
    pub expires_in: u32,
    pub agent_id: Option<String>,
}

impl Pairing {
    pub fn begin(id: String) -> Self {
        Self {
            id,
            phase: PairPhase::Loading,
            ..Self::default()
        }
    }
    pub fn receive(&mut self, info: PairInfo, now: f64) -> bool {
        if self.id != info.pairing_id
            || self.phase != PairPhase::Loading
            || !now.is_finite()
            || info.code.len() != 12
            || !info.code.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(info.client.as_str(), "codex" | "generic")
            || info.expires_in > 300
            || (info.approved && info.agent_id.is_none())
        {
            return false;
        }
        self.code = info.code;
        self.client = info.client;
        self.approved_agent = info.agent_id;
        self.deadline_ms = now + f64::from(info.expires_in) * 1000.0;
        self.phase = if info.approved {
            PairPhase::Approved
        } else if info.expires_in == 0 {
            PairPhase::Ended
        } else {
            PairPhase::Ready
        };
        true
    }
    pub fn seconds(&self, now: f64) -> i32 {
        if !now.is_finite() {
            return 0;
        }
        ((self.deadline_ms - now) / 1000.0).ceil().clamp(0.0, 300.0) as i32
    }
    pub fn expire(&mut self, now: f64) {
        if self.phase == PairPhase::Ready && self.seconds(now) == 0 {
            self.phase = PairPhase::Ended;
        }
    }
    pub fn can_approve(&self, enabled: bool, now: f64) -> bool {
        self.phase == PairPhase::Ready && enabled && self.seconds(now) > 0
    }
    pub fn approved(&mut self, agent: String) {
        if matches!(self.phase, PairPhase::Ready | PairPhase::Ended) {
            self.phase = PairPhase::Approved;
            self.approved_agent = Some(agent);
        }
    }
    pub fn fail(&mut self, ended: bool) {
        if !matches!(self.phase, PairPhase::None | PairPhase::Approved) {
            self.phase = if ended {
                PairPhase::Ended
            } else {
                PairPhase::Failed
            };
        }
    }
    pub fn stage(&self) -> &'static str {
        match self.phase {
            PairPhase::None => "",
            PairPhase::Loading => "loading",
            PairPhase::Ready => "ready",
            PairPhase::Approved => "approved",
            PairPhase::Ended => "ended",
            PairPhase::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> PairInfo {
        PairInfo {
            pairing_id: "public-request".into(),
            code: "012345abcdef".into(),
            client: "codex".into(),
            approved: false,
            expires_in: 20,
            agent_id: None,
        }
    }
    #[test]
    fn instructions_do_not_depend_on_agent_selection_or_disclose_credentials() {
        let codex = instructions("https://memory.example", "work-notes", true, false);
        let generic = instructions("https://memory.example", "work-notes", false, false);
        assert_eq!(codex.install, generic.install);
        assert!(
            codex.initialize.contains("--agent codex")
                && generic.initialize.contains("--agent generic")
        );
        assert!(!codex.initialize.contains("mcp_servers") && !codex.initialize.contains("plugin"));
        assert!(
            generic.hook.contains("/generic/client.json") && codex.repair.contains("hook install")
        );
        assert!(!codex.install.contains("cm_mcp_") && !generic.initialize.contains("cm_hook_"));
    }
    #[test]
    fn a_background_time_jump_expires_confirmation_without_waiting_for_timer_ticks() {
        let mut pair = Pairing::begin("public-request".into());
        assert!(!pair.can_approve(true, 1000.0));
        assert!(pair.receive(info(), 1000.0));
        assert_eq!(pair.seconds(1000.0), 20);
        assert!(pair.can_approve(true, 20999.0));
        assert!(!pair.can_approve(false, 1001.0));
        assert!(!pair.can_approve(true, 21000.0));
        pair.expire(90000.0);
        assert_eq!(pair.phase, PairPhase::Ended);
    }
    #[test]
    fn late_or_malformed_previews_cannot_replace_the_current_confirmation() {
        let mut pair = Pairing::begin("different-request".into());
        assert!(!pair.receive(info(), 0.0));
        let mut pair = Pairing::begin("public-request".into());
        let mut invalid = info();
        invalid.expires_in = 301;
        assert!(!pair.receive(invalid, 0.0));
        assert!(!pair.receive(info(), f64::NAN));
        assert!(pair.receive(info(), 0.0));
        pair.approved("chosen-agent".into());
        pair.fail(true);
        pair.expire(90000.0);
        assert_eq!(pair.phase, PairPhase::Approved);
        assert!(!pair.receive(info(), 0.0));
        assert_eq!(pair.approved_agent.as_deref(), Some("chosen-agent"));
    }

    #[test]
    fn server_approval_acknowledged_after_local_expiry_still_reports_the_actual_grant() {
        let mut pair = Pairing::begin("public-request".into());
        assert!(pair.receive(info(), 0.0));
        pair.expire(21000.0);
        pair.approved("confirmed-agent".into());
        assert_eq!(pair.phase, PairPhase::Approved);
        assert_eq!(pair.approved_agent.as_deref(), Some("confirmed-agent"));
        let mut normal = Pairing::default();
        normal.fail(false);
        assert_eq!(normal.phase, PairPhase::None);
    }
    #[test]
    fn enabling_an_agent_is_not_evidence_of_client_requests() {
        let mut agent = Agent {
            agent_id: "id".into(),
            agent_name: "name".into(),
            enabled: true,
            allow_secrets: false,
            last_mcp_at: None,
            last_hook_at: None,
        };
        assert_eq!(agent.status(), "等待连接");
        agent.last_hook_at = Some("synthetic-upload-time".into());
        assert_eq!(agent.status(), "有请求记录");
        agent.enabled = false;
        assert_eq!(agent.status(), "已停用");
    }
}
