#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::{
    AgentRow, Dashboard,
    access::{self, Agent, PairInfo, PairPhase, Pairing},
    session::Session,
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window,js_name=chromieAccess)]
    fn send_access(
        space: &str,
        operation: &str,
        id: &str,
        kind: &str,
        payload: &str,
        generation: u32,
    );
    #[wasm_bindgen(js_namespace=window,js_name=chromieCancelAccess)]
    fn cancel_access();
    #[wasm_bindgen(js_namespace=window,js_name=chromieCopyAccess)]
    fn copy_text(text: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window,js_name=chromieOrigin)]
    fn origin() -> String;
    #[wasm_bindgen(js_namespace=window,js_name=chromiePairingId)]
    fn pairing_id() -> String;
    #[wasm_bindgen(js_namespace=window,js_name=chromieDismissPairing)]
    fn dismiss_pairing();
    #[wasm_bindgen(js_namespace=window,js_name=chromieNow)]
    fn now() -> f64;
    #[wasm_bindgen(js_namespace=window,js_name=chromieUsesNushell)]
    fn uses_nushell() -> bool;
    #[wasm_bindgen(js_namespace=window,js_name=chromieSetShell)]
    fn save_shell(nushell: bool);
}
#[derive(Default)]
struct State {
    generation: u32,
    pending: Option<(String, String)>,
    agents: Vec<Agent>,
    pair: Pairing,
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<State>>,
    _timer: Timer,
}
thread_local! {static CONTROLLER:RefCell<Option<Controller>>=const {RefCell::new(None)};}
type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<State>>);
fn active() -> Option<Active> {
    CONTROLLER.with_borrow(|c| {
        let c = c.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}

fn templates(ui: &Dashboard) {
    let value = access::instructions(
        &origin(),
        &ui.get_space_id(),
        ui.get_access_codex(),
        ui.get_access_nushell(),
    );
    ui.set_access_install_command(value.install.into());
    ui.set_access_init_command(value.initialize.into());
    ui.set_access_mcp_config(value.manual_mcp.into());
    ui.set_access_hook_config(value.hook.into());
    ui.set_access_repair_command(value.repair.into());
    ui.set_access_path_command(value.path.into());
    ui.set_access_path_note(value.path_note.into());
    ui.set_access_agent_guidance(super::access::AGENT_GUIDANCE.into());
}
fn pair_ui(ui: &Dashboard, state: &State) {
    ui.set_access_pair_status(state.pair.stage().into());
    ui.set_access_pair_code(state.pair.code.clone().into());
    ui.set_access_pair_client(
        if state.pair.client == "codex" {
            "Codex"
        } else {
            "通用客户端"
        }
        .into(),
    );
    ui.set_access_pair_seconds(state.pair.seconds(now()));
    let name = state
        .pair
        .approved_agent
        .as_ref()
        .and_then(|id| state.agents.iter().find(|a| &a.agent_id == id))
        .map(|a| a.agent_name.as_str())
        .unwrap_or("所选接入");
    ui.set_access_pair_agent(name.into());
}
fn launch(operation: &str, kind: &str, payload: Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_access_page() || ui.get_access_busy() {
        return;
    }
    if matches!(operation, "reveal" | "rotate" | "download")
        && (!ui.get_access_advanced_open() || !state.borrow().pair.id.is_empty())
    {
        return;
    }
    let (generation, id) = {
        let mut state = state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.pending = Some((operation.into(), kind.into()));
        (
            state.generation,
            if operation.starts_with("pair-") {
                state.pair.id.clone()
            } else {
                ui.get_access_selected_id().to_string()
            },
        )
    };
    ui.set_access_busy(true);
    ui.set_access_error("".into());
    ui.set_access_notice("".into());
    send_access(
        &session.borrow().space,
        operation,
        &id,
        kind,
        &payload.to_string(),
        generation,
    );
}
fn selected(ui: &Dashboard, agent: &Agent, reset: bool) {
    let old_label = ui.get_access_selected_label();
    let preserve_draft = !reset && ui.get_access_selected_name() != old_label;
    if reset {
        ui.set_access_editing_name(false);
        ui.set_access_mcp_key("".into());
        ui.set_access_hook_key("".into());
        ui.set_access_confirm_kind("".into());
        ui.set_access_advanced_open(false);
    }
    ui.set_access_selected_id(agent.agent_id.clone().into());
    ui.set_access_selected_label(agent.agent_name.clone().into());
    if !preserve_draft {
        ui.set_access_selected_name(agent.agent_name.clone().into());
    }
    ui.set_access_selected_enabled(agent.enabled);
    ui.set_access_allow_secrets(agent.allow_secrets);
}
fn rows(ui: &Dashboard, state: &State) {
    ui.set_access_agents(ModelRc::new(VecModel::from(
        state
            .agents
            .iter()
            .map(|agent| AgentRow {
                id: agent.agent_id.clone().into(),
                name: agent.agent_name.clone().into(),
                enabled: agent.enabled,
                status: agent.status().into(),
                mcp: agent
                    .last_mcp_at
                    .as_deref()
                    .map(crate::browser::date)
                    .unwrap_or_else(|| "尚无请求".into())
                    .into(),
                capture: agent
                    .last_hook_at
                    .as_deref()
                    .map(crate::browser::date)
                    .unwrap_or_else(|| "尚未上传".into())
                    .into(),
            })
            .collect::<Vec<_>>(),
    )));
}
fn choose(id: &str) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if ui.get_access_busy() || state.borrow().pair.phase == PairPhase::Approved {
        return;
    }
    let agent = state
        .borrow()
        .agents
        .iter()
        .find(|a| a.agent_id == id)
        .cloned();
    let Some(agent) = agent else { return };
    if !state.borrow().pair.id.is_empty() && !agent.enabled {
        return;
    }
    let collapse = ui.get_access_selected_id() == id && state.borrow().pair.id.is_empty();
    hide_access_keys();
    ui.set_access_notice("".into());
    ui.set_access_error("".into());
    if collapse {
        ui.set_access_selected_id("".into());
    } else {
        selected(&ui, &agent, true);
    }
}
fn name(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 100 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    ui.set_access_nushell(uses_nushell());
    let state = Rc::new(RefCell::new(State::default()));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(1), || {
        if let Some((ui, _, state)) = active()
            && ui.get_unlocked()
            && ui.get_access_page()
        {
            let mut state = state.borrow_mut();
            state.pair.expire(now());
            pair_ui(&ui, &state);
        }
    });
    CONTROLLER.with_borrow_mut(|c| {
        *c = Some(Controller {
            ui: ui.as_weak(),
            session,
            state,
            _timer: timer,
        })
    });
    ui.on_access_refresh(load);
    ui.on_access_dismiss_notice(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_error("".into());
            ui.set_access_notice("".into());
        }
    });
    ui.on_access_choose_mode(|codex| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_codex(codex);
            templates(&ui);
        }
    });
    ui.on_access_choose_shell(|nushell| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_nushell(nushell);
            save_shell(nushell);
            templates(&ui);
        }
    });
    ui.on_access_create(|| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        if state.borrow().pair.phase == PairPhase::Approved {
            return;
        }
        if let Some(value) = name(&ui.get_access_new_name()) {
            launch("create", "", json!({"agent_name":value}));
        } else {
            ui.set_access_error("请输入简短的接入名称，例如工作电脑。".into());
        }
    });
    ui.on_access_select(|id| choose(&id));
    ui.on_access_edit_name(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_editing_name(true);
        }
    });
    ui.on_access_save_name(|| {
        let Some((ui, _, _)) = active() else { return };
        if let Some(value) = name(&ui.get_access_selected_name()) {
            if value != ui.get_access_selected_label().as_str() {
                launch("update", "name", json!({"agent_name":value}));
            }
        } else {
            ui.set_access_error("名称为空或过长，请检查后重试。".into());
        }
    });
    ui.on_access_cancel_name(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_selected_name(ui.get_access_selected_label());
            ui.set_access_editing_name(false);
        }
    });
    ui.on_access_toggle_enabled(|| {
        if let Some((ui, _, _)) = active() {
            launch(
                "update",
                "enabled",
                json!({"enabled":!ui.get_access_selected_enabled()}),
            );
        }
    });
    ui.on_access_toggle_secrets(|| {
        if let Some((ui, _, _)) = active() {
            launch(
                "update",
                "secrets",
                json!({"allow_secrets":!ui.get_access_allow_secrets()}),
            );
        }
    });
    ui.on_access_toggle_advanced(|| {
        if let Some((ui, _, _)) = active() {
            if ui.get_access_advanced_open() {
                hide_access_keys();
            } else {
                ui.set_access_advanced_open(true);
            }
        }
    });
    ui.on_access_toggle_key(|kind| {
        if let Some((ui, _, _)) = active() {
            if !ui.get_access_advanced_open() {
                return;
            }
            let shown = if kind == "mcp" {
                !ui.get_access_mcp_key().is_empty()
            } else {
                !ui.get_access_hook_key().is_empty()
            };
            if shown {
                if kind == "mcp" {
                    ui.set_access_mcp_key("".into())
                } else {
                    ui.set_access_hook_key("".into())
                }
            } else {
                launch("reveal", &kind, json!({}));
            }
        }
    });
    ui.on_access_copy_key(|kind| {
        if let Some((ui, _, state)) = active() {
            if !ui.get_access_advanced_open() {
                return;
            }
            let key = if kind == "mcp" {
                ui.get_access_mcp_key()
            } else {
                ui.get_access_hook_key()
            };
            if !key.is_empty() {
                copy_text(&key, state.borrow().generation);
            }
        }
    });
    ui.on_access_rotate_key(|kind| {
        if let Some((ui, _, _)) = active()
            && ui.get_access_advanced_open()
        {
            ui.set_access_confirm_kind(kind);
        }
    });
    ui.on_access_cancel_rotate(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_access_confirm_kind("".into());
        }
    });
    ui.on_access_confirm_rotate(|| {
        if let Some((ui, _, _)) = active() {
            launch("rotate", &ui.get_access_confirm_kind(), json!({}));
        }
    });
    ui.on_access_download(|| launch("download", "", json!({})));
    ui.on_access_copy_config(|kind| {
        if let Some((ui, _, state)) = active() {
            let text = match kind.as_str() {
                "install" => ui.get_access_install_command(),
                "init" => ui.get_access_init_command(),
                "repair" => ui.get_access_repair_command(),
                "path" => ui.get_access_path_command(),
                "hook" => ui.get_access_hook_config(),
                "guidance" => ui.get_access_agent_guidance(),
                _ => ui.get_access_mcp_config(),
            };
            copy_text(&text, state.borrow().generation);
        }
    });
    ui.on_access_approve_pair(|| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        if state
            .borrow()
            .pair
            .can_approve(ui.get_access_selected_enabled(), now())
            && !ui.get_access_selected_id().is_empty()
        {
            let code = state.borrow().pair.code.clone();
            launch(
                "pair-approve",
                "",
                json!({"agent_id":ui.get_access_selected_id().as_str(),"code":code}),
            );
        }
    });
    ui.on_access_dismiss_pair(|| {
        hide_access_keys();
        dismiss_pairing();
        if let Some((ui, _, _)) = active() {
            ui.invoke_reset_scroll();
        }
        if let Some((ui, _, state)) = active() {
            let selected = state.borrow().pair.approved_agent.clone();
            state.borrow_mut().pair = Pairing::default();
            pair_ui(&ui, &state.borrow());
            ui.set_access_selected_id("".into());
            if let Some(id) = selected {
                choose(&id);
            }
            load();
        }
    });
}

pub(crate) fn load() {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_access_page() || ui.get_access_busy() {
        return;
    }
    templates(&ui);
    let id = pairing_id();
    {
        let mut state = state.borrow_mut();
        if id != state.pair.id {
            ui.invoke_reset_scroll();
            ui.set_access_selected_id("".into());
            ui.set_access_advanced_open(false);
            state.pair = if id.is_empty() {
                Pairing::default()
            } else {
                Pairing::begin(id)
            };
        } else if !id.is_empty() && state.pair.phase != PairPhase::Approved {
            state.pair = Pairing::begin(id);
        }
        pair_ui(&ui, &state);
    }
    launch("list", "", json!({}));
}

pub(crate) fn route_changed() {
    if let Some((_, _, state)) = active() {
        let changed = state.borrow().pair.id != pairing_id();
        if changed {
            hide_access_keys();
            load();
        }
    }
}
#[wasm_bindgen]
pub fn resume_access() {
    if let Some((ui, _, _)) = active()
        && ui.get_unlocked()
        && ui.get_access_page()
    {
        load();
    }
}
#[wasm_bindgen]
pub fn hide_access_keys() {
    if let Some((ui, _, state)) = active() {
        let mut state = state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.pending = None;
        ui.set_access_mcp_key("".into());
        ui.set_access_hook_key("".into());
        ui.set_access_confirm_kind("".into());
        ui.set_access_advanced_open(false);
        ui.set_access_busy(false);
    }
    cancel_access();
}
pub(crate) fn clear() {
    hide_access_keys();
    if let Some((ui, _, state)) = active() {
        state.borrow_mut().agents.clear();
        state.borrow_mut().pair = Pairing::default();
        ui.set_access_agents(ModelRc::new(VecModel::<AgentRow>::default()));
        ui.set_access_loaded(false);
        ui.set_access_editing_name(false);
        ui.set_access_selected_id("".into());
        ui.set_access_selected_label("".into());
        ui.set_access_selected_name("".into());
        ui.set_access_new_name("".into());
        ui.set_access_error("".into());
        ui.set_access_notice("".into());
        pair_ui(&ui, &state.borrow());
    }
}
#[wasm_bindgen]
pub fn receive_access_notice(generation: u32, success: bool) {
    if let Some((ui, _, state)) = active()
        && ui.get_unlocked()
        && ui.get_access_page()
        && state.borrow().generation == generation
    {
        if success {
            ui.set_access_notice("已复制。".into());
        } else {
            ui.set_access_error("复制失败，可以直接选中命令复制。".into());
        }
    }
}
#[wasm_bindgen]
pub fn receive_access(generation: u32, status: u16, body: String) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_access_page() || state.borrow().generation != generation {
        return;
    }
    let Some((operation, kind)) = state.borrow_mut().pending.take() else {
        return;
    };
    ui.set_access_busy(false);
    if status == 401 {
        crate::browser::end_session();
        ui.set_message("登录已失效，请重新登录空间。".into());
        return;
    }
    let value = (body.len() <= 160000)
        .then(|| serde_json::from_str::<Value>(&body).ok())
        .flatten();
    let Some(value) = value.filter(|_| (200..=299).contains(&status)) else {
        if operation == "pair-approve" && matches!(status, 404 | 409) {
            load();
            return;
        }
        if operation.starts_with("pair-")
            || (!state.borrow().pair.id.is_empty() && operation == "list")
        {
            state.borrow_mut().pair.fail(matches!(status, 404 | 410));
            pair_ui(&ui, &state.borrow());
            if matches!(status, 404 | 410) {
                return;
            }
        }
        ui.set_access_error(
            match (operation.as_str(), status) {
                ("create" | "update", 409) => "这个名称已被使用，请换一个名称。",
                ("create" | "update", 400) => "名称或设置无效，请检查后重试。",
                ("pair-approve", 400) => "确认信息已变化，请重新读取请求。",
                (_, 0) => "网络连接中断，请重试。",
                _ => "操作未完成，请重试。",
            }
            .into(),
        );
        return;
    };
    match operation.as_str() {
        "pair-info" => {
            let accepted = serde_json::from_value::<PairInfo>(value)
                .ok()
                .is_some_and(|info| state.borrow_mut().pair.receive(info, now()));
            if !accepted {
                state.borrow_mut().pair.fail(false);
            }
            pair_ui(&ui, &state.borrow());
        }
        "pair-approve" => {
            state
                .borrow_mut()
                .pair
                .approved(ui.get_access_selected_id().to_string());
            pair_ui(&ui, &state.borrow());
        }
        "list" => {
            let Some(agents) = value
                .get("agents")
                .cloned()
                .and_then(|v| serde_json::from_value::<Vec<Agent>>(v).ok())
            else {
                ui.set_access_error("接入列表格式无效，请重试。".into());
                state.borrow_mut().pair.fail(false);
                pair_ui(&ui, &state.borrow());
                return;
            };
            state.borrow_mut().agents = agents;
            ui.set_access_loaded(true);
            rows(&ui, &state.borrow());
            let existing = state
                .borrow()
                .agents
                .iter()
                .find(|a| a.agent_id == ui.get_access_selected_id().as_str())
                .cloned();
            if let Some(agent) = existing {
                selected(&ui, &agent, false);
            } else {
                ui.set_access_selected_id("".into());
            }
            if state.borrow().pair.phase == PairPhase::Loading {
                launch("pair-info", "", json!({}));
            }
        }
        "reveal" => {
            if ui.get_access_advanced_open()
                && let Some(key) = value["key"].as_str()
            {
                if kind == "mcp" {
                    ui.set_access_mcp_key(key.into());
                } else {
                    ui.set_access_hook_key(key.into());
                }
            }
        }
        "download" => {
            ui.set_access_notice("已导出含密钥的配置，请保存在私有目录并设置权限 0600。".into())
        }
        "rotate" => {
            ui.set_access_mcp_key("".into());
            ui.set_access_hook_key("".into());
            ui.set_access_confirm_kind("".into());
            ui.set_access_notice("密钥已重置，请重新运行 CLI 初始化更新本机配置。".into());
        }
        "create" | "update" => {
            if let Some(agent) = value
                .get("agent")
                .cloned()
                .and_then(|v| serde_json::from_value::<Agent>(v).ok())
            {
                let reset = operation == "create";
                {
                    let mut state = state.borrow_mut();
                    if let Some(row) = state
                        .agents
                        .iter_mut()
                        .find(|a| a.agent_id == agent.agent_id)
                    {
                        *row = agent.clone();
                    } else {
                        state.agents.insert(0, agent.clone());
                    }
                }
                selected(&ui, &agent, reset);
                if kind == "name" {
                    ui.set_access_selected_name(agent.agent_name.into());
                    ui.set_access_editing_name(false);
                }
                if reset {
                    ui.set_access_new_name("".into());
                }
                rows(&ui, &state.borrow());
                ui.set_access_notice(
                    if reset {
                        "接入已创建。"
                    } else {
                        "已保存。"
                    }
                    .into(),
                );
            }
        }
        _ => {}
    }
}
