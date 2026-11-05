#![forbid(unsafe_code)]
use crate::{
    CapturedRow, ConversationRow, Dashboard,
    messages::{Conversation, Gate, Message, Page},
    session::Session,
};
use serde::Deserialize;
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window,js_name=chromieMessages)]
    fn send(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window,js_name=chromieCancelMessages)]
    fn cancel();
}
#[derive(Deserialize)]
struct Agent {
    agent_id: String,
    agent_name: String,
}
#[derive(Deserialize)]
struct Agents {
    agents: Vec<Agent>,
}
#[derive(Default)]
struct State {
    gate: Gate,
    agents: Vec<Agent>,
    filter: Option<String>,
    sessions: Vec<Conversation>,
    selected: Option<Conversation>,
    session_page: u64,
    session_snapshot: Option<String>,
    message_page: u64,
    message_snapshot: Option<String>,
    messages: Vec<Message>,
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<State>>,
}
thread_local! {static CONTROLLER:RefCell<Option<Controller>>=const{RefCell::new(None)};}
type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<State>>);
fn active() -> Option<Active> {
    CONTROLLER.with_borrow(|c| {
        let c = c.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}

pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    CONTROLLER.with_borrow_mut(|c| {
        *c = Some(Controller {
            ui: ui.as_weak(),
            session,
            state: Rc::new(RefCell::new(State::default())),
        })
    });
    ui.on_messages_refresh(|| {
        if active().is_some_and(|(_, _, state)| state.borrow().selected.is_some()) {
            conversation(true);
        } else {
            load();
        }
    });
    ui.on_messages_filter(|index| {
        if let Some((ui, _, state)) = active() {
            let mut state = state.borrow_mut();
            state.filter = if index > 0 {
                state
                    .agents
                    .get((index - 1) as usize)
                    .map(|a| a.agent_id.clone())
            } else {
                None
            };
            drop(state);
            ui.set_messages_agent_index(index);
            sessions(true);
        }
    });
    ui.on_messages_select(|index| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        let selected = usize::try_from(index)
            .ok()
            .and_then(|i| state.borrow().sessions.get(i).cloned());
        state.borrow_mut().selected = selected.clone();
        ui.set_messages_selected(if selected.is_some() { index } else { -1 });
        clear_thread(&ui);
        state.borrow_mut().message_page = 0;
        state.borrow_mut().message_snapshot = None;
        if let Some(selected) = selected {
            ui.set_messages_thread_title(
                format!(
                    "{} · {}",
                    selected.origin.agent_name,
                    selected.session_id.chars().take(12).collect::<String>()
                )
                .into(),
            );
            conversation(true);
        } else {
            state.borrow_mut().gate.clear();
            cancel();
            ui.set_messages_busy(false);
        }
    });
    ui.on_messages_turn_sessions(|direction| {
        if let Some((_, _, state)) = active() {
            let mut s = state.borrow_mut();
            s.session_page = if direction > 0 {
                s.session_page + 1
            } else {
                s.session_page.saturating_sub(1)
            };
            drop(s);
            sessions(false);
        }
    });
    ui.on_messages_turn_messages(|direction| {
        if let Some((_, _, state)) = active() {
            let mut s = state.borrow_mut();
            s.message_page = if direction > 0 {
                s.message_page + 1
            } else {
                s.message_page.saturating_sub(1)
            };
            drop(s);
            conversation(false);
        }
    });
    ui.on_messages_expand(|id| {
        if let Some((ui, _, state)) = active() {
            if ui.get_messages_expanded() == id {
                ui.set_messages_expanded("".into());
                ui.set_messages_full_body("".into());
                return;
            }
            if !state
                .borrow()
                .messages
                .iter()
                .any(|m| m.event_id == id.as_str())
            {
                return;
            }
            ui.set_messages_expanded(id.clone());
            ui.set_messages_full_body("正在读取完整正文…".into());
            ui.set_messages_full_note("".into());
            launch("detail", json!({"event_id":id.as_str()}));
        }
    });
}
fn clear_thread(ui: &Dashboard) {
    ui.set_messages_rows(ModelRc::new(VecModel::<CapturedRow>::default()));
    ui.set_messages_expanded("".into());
    ui.set_messages_full_body("".into());
    ui.set_messages_full_note("".into());
}
pub(crate) fn clear() {
    cancel();
    if let Some((ui, _, state)) = active() {
        let mut s = state.borrow_mut();
        s.gate.clear();
        s.sessions.clear();
        s.selected = None;
        s.messages.clear();
        s.agents.clear();
        s.filter = None;
        s.session_snapshot = None;
        s.message_snapshot = None;
        s.session_page = 0;
        s.message_page = 0;
        clear_thread(&ui);
        ui.set_messages_selected(-1);
        ui.set_messages_sessions(ModelRc::new(VecModel::<ConversationRow>::default()));
        ui.set_messages_agents(ModelRc::new(VecModel::from(vec!["全部 Agent".into()])));
        ui.set_messages_agent_index(0);
        ui.set_messages_busy(false);
        ui.set_messages_error("".into());
    }
}
pub(crate) fn load() {
    launch("agents", json!({}));
}
fn launch(operation: &str, payload: Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_messages_page() {
        return;
    }
    let generation = state.borrow_mut().gate.begin(operation);
    ui.set_messages_busy(true);
    ui.set_messages_error("".into());
    send(
        &session.borrow().space,
        operation,
        &payload.to_string(),
        generation,
    );
}
fn sessions(reset: bool) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    let mut s = state.borrow_mut();
    if reset {
        s.session_page = 0;
        s.session_snapshot = None;
    }
    s.selected = None;
    s.messages.clear();
    ui.set_messages_selected(-1);
    clear_thread(&ui);
    ui.invoke_reset_scroll();
    ui.set_messages_sessions(ModelRc::new(VecModel::<ConversationRow>::default()));
    let payload = json!({"agent_id":s.filter,"page":s.session_page,"as_of":s.session_snapshot});
    drop(s);
    launch("sessions", payload);
}
fn conversation(reset: bool) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    let mut s = state.borrow_mut();
    let Some(selected) = s.selected.clone() else {
        return;
    };
    if reset {
        s.message_page = 0;
        s.message_snapshot = None;
    }
    s.messages.clear();
    clear_thread(&ui);
    ui.invoke_reset_scroll();
    let payload = json!({"agent_id":selected.agent_id,"session_id":selected.session_id,"page":s.message_page,"as_of":s.message_snapshot});
    drop(s);
    launch("conversation", payload);
}
fn populate(operation: &str, body: &str, ui: &Dashboard, state: &RefCell<State>) -> Option<bool> {
    match operation {
        "agents" => {
            let data = serde_json::from_str::<Agents>(body).ok()?;
            let mut s = state.borrow_mut();
            s.agents = data.agents;
            let index = s
                .filter
                .as_ref()
                .and_then(|id| s.agents.iter().position(|a| &a.agent_id == id))
                .map(|i| i + 1)
                .unwrap_or(0);
            if index == 0 {
                s.filter = None;
            }
            ui.set_messages_agent_index(index as i32);
            let names = std::iter::once("全部 Agent".into())
                .chain(s.agents.iter().map(|a| a.agent_name.clone().into()))
                .collect::<Vec<_>>();
            ui.set_messages_agents(ModelRc::new(VecModel::from(names)));
            Some(true)
        }
        "sessions" => {
            let data = serde_json::from_str::<Page<Conversation>>(body).ok()?;
            let rows = data
                .items
                .iter()
                .map(|c| ConversationRow {
                    title: format!("{} · {} 条", c.origin.agent_name, c.message_count).into(),
                    preview: c
                        .preview
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into(),
                    meta: format!(
                        "{} · 会话 …{}",
                        crate::browser::date(&c.received_at),
                        c.session_id
                            .chars()
                            .rev()
                            .take(8)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    )
                    .into(),
                })
                .collect::<Vec<_>>();
            ui.set_messages_sessions(ModelRc::new(VecModel::from(rows)));
            ui.set_messages_session_page(data.page as i32);
            ui.set_messages_sessions_more(data.has_more);
            let mut s = state.borrow_mut();
            s.sessions = data.items;
            s.session_snapshot = Some(data.as_of);
            Some(false)
        }
        "conversation" => {
            let data = serde_json::from_str::<Page<Message>>(body).ok()?;
            let s = state.borrow();
            let selected = s.selected.as_ref()?;
            let rows=data.items.iter().map(|m|CapturedRow{id:m.event_id.clone().into(),role:if m.role=="user"{"用户"}else if m.role=="assistant"{"助手"}else{"其他"}.into(),user:m.role=="user",body:m.preview.clone().into(),time:crate::browser::date(&m.occurred_at).into(),truncated:m.truncated,
                metadata:format!("event_id: {}\nsession_id: {}\nAgent: {}\nagent_id: {}\nturn_id: {}\n项目: {}\n消息时间: {}\n服务接收时间: {}",m.event_id,selected.session_id,selected.origin.agent_name,selected.agent_id.as_deref().unwrap_or("历史记录"),m.turn_id.as_deref().unwrap_or("无"),m.project_key.as_deref().unwrap_or("无"),m.occurred_at,m.received_at).into()}).collect::<Vec<_>>();
            drop(s);
            ui.set_messages_rows(ModelRc::new(VecModel::from(rows)));
            ui.set_messages_message_page(data.page as i32);
            ui.set_messages_messages_more(data.has_more);
            let mut s = state.borrow_mut();
            s.message_snapshot = Some(data.as_of);
            s.messages = data.items;
            Some(false)
        }
        "detail" => {
            #[derive(Deserialize)]
            struct Detail {
                event_id: String,
                body: String,
                truncated: bool,
            }
            let data = serde_json::from_str::<Detail>(body).ok()?;
            if ui.get_messages_expanded() != data.event_id {
                return None;
            }
            ui.set_messages_full_body(data.body.into());
            ui.set_messages_full_note(
                if data.truncated {
                    "正文超过展示上限，仅显示前 60,000 字符"
                } else {
                    "完整正文 · 只读"
                }
                .into(),
            );
            Some(false)
        }
        _ => None,
    }
}
#[wasm_bindgen]
pub fn receive_messages(generation: u32, status: u16, body: String) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_messages_page() {
        return;
    }
    let Some(operation) = state.borrow_mut().gate.accept(generation) else {
        return;
    };
    ui.set_messages_busy(false);
    if status == 401 {
        crate::browser::end_session();
        ui.set_message("登录已失效，请重新登录。".into());
        return;
    }
    if status == 200
        && let Some(load_sessions) = populate(&operation, &body, &ui, &state)
    {
        if load_sessions {
            sessions(true);
        }
        return;
    }
    if operation == "detail" {
        ui.set_messages_expanded("".into());
        ui.set_messages_full_body("".into());
    }
    ui.set_messages_error(
        if status == 404 {
            "消息已不存在或已被遗忘，请刷新会话。"
        } else {
            "暂时无法读取采集消息，请刷新重试。"
        }
        .into(),
    );
}
