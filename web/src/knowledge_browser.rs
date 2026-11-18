#![forbid(unsafe_code)]
//! 主题入口和关联阅读共享登录态；响应代际隔离分页、返回与切换空间后的旧请求。
use crate::{Dashboard, KnowledgeRow, messages::Gate, recall::RecallHit, session::Session};
use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window, js_name=chromieKnowledge)]
    fn send(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window, js_name=chromieCancelKnowledge)]
    fn cancel();
}

#[derive(Default)]
struct State {
    nav: Gate,
    reader: Gate,
    loaded: bool,
    nav_cursor: Option<String>,
    selected: String,
    recall_hit: Option<RecallHit>,
    stack: Vec<String>,
    body: String,
    outgoing_cursor: Option<String>,
    incoming_cursor: Option<String>,
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<State>>,
}
thread_local! { static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) }; }
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
    ui.on_knowledge_read(|id| open(id.as_str(), true));
    ui.on_read_recall_full(|id, revision| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        if ui.get_recall_page() && ui.get_unlocked() {
            state.borrow_mut().recall_hit = Some(RecallHit {
                memory_id: id.to_string(),
                revision,
            });
            open(id.as_str(), true);
        }
    });
    ui.on_knowledge_close(close);
    ui.on_knowledge_go_back(|| {
        let Some((_, _, state)) = active() else {
            return;
        };
        let id = state.borrow_mut().stack.pop();
        if let Some(id) = id {
            open(&id, false);
        }
    });
    ui.on_knowledge_more(neighbors);
    ui.on_knowledge_refresh(|| load(true));
    ui.on_knowledge_nav_next(|| navigation(false));
}

pub(crate) fn clear() {
    cancel();
    if let Some((ui, _, state)) = active() {
        let mut s = state.borrow_mut();
        s.nav.clear();
        s.reader.clear();
        s.loaded = false;
        s.selected.clear();
        s.recall_hit = None;
        s.stack.clear();
        s.body.clear();
        s.nav_cursor = None;
        s.outgoing_cursor = None;
        s.incoming_cursor = None;
        ui.set_knowledge_open(false);
        ui.set_knowledge_busy(false);
        ui.set_knowledge_nav_busy(false);
        ui.set_knowledge_title("".into());
        ui.set_knowledge_body("".into());
        ui.set_knowledge_note("".into());
        ui.set_knowledge_error("".into());
        ui.set_knowledge_nav_error("".into());
        ui.set_knowledge_nav_more(false);
        ui.set_knowledge_topics(ModelRc::new(VecModel::default()));
        ui.set_knowledge_outgoing(ModelRc::new(VecModel::default()));
        ui.set_knowledge_incoming(ModelRc::new(VecModel::default()));
    }
}

fn close() {
    if let Some((ui, _, state)) = active() {
        let mut s = state.borrow_mut();
        s.reader.clear();
        s.selected.clear();
        s.recall_hit = None;
        s.stack.clear();
        s.body.clear();
        ui.set_knowledge_open(false);
        ui.set_knowledge_body("".into());
        ui.set_knowledge_title("".into());
        ui.invoke_knowledge_return_focus();
    }
}

pub(crate) fn load(force: bool) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked()
        || ui.get_settings_page()
        || ui.get_messages_page()
        || ui.get_access_page()
        || ui.get_recall_page()
        || ui.get_transfer_page()
    {
        return;
    }
    if force || !state.borrow().loaded {
        navigation(true);
    }
}

fn launch(operation: &str, payload: Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() {
        return;
    }
    let generation = if operation == "navigation" {
        ui.set_knowledge_nav_busy(true);
        ui.set_knowledge_nav_error("".into());
        state.borrow_mut().nav.begin(operation)
    } else {
        if !ui.get_knowledge_open() {
            return;
        }
        ui.set_knowledge_busy(true);
        ui.set_knowledge_error("".into());
        state.borrow_mut().reader.begin(operation)
    };
    send(
        &session.borrow().space,
        operation,
        &payload.to_string(),
        generation,
    );
}

fn navigation(reset: bool) {
    if let Some((ui, _, state)) = active() {
        if reset {
            state.borrow_mut().nav_cursor = None;
            ui.set_knowledge_topics(ModelRc::new(VecModel::default()));
        }
        let cursor = state.borrow().nav_cursor.clone();
        launch("navigation", json!({"cursor":cursor,"max_chars":8000}));
    }
}

fn open(id: &str, push: bool) {
    if let Some((ui, _, state)) = active() {
        let mut s = state.borrow_mut();
        if push && !s.selected.is_empty() && s.selected != id {
            let previous = s.selected.clone();
            s.stack.push(previous);
        }
        s.selected = id.into();
        s.body.clear();
        s.outgoing_cursor = None;
        s.incoming_cursor = None;
        ui.set_knowledge_back(!s.stack.is_empty());
        ui.set_knowledge_open(true);
        ui.set_knowledge_title("正在读取记忆…".into());
        ui.set_knowledge_body("".into());
        ui.set_knowledge_note("".into());
        ui.set_knowledge_outgoing(ModelRc::new(VecModel::default()));
        ui.set_knowledge_incoming(ModelRc::new(VecModel::default()));
        ui.set_knowledge_outgoing_more(false);
        ui.set_knowledge_incoming_more(false);
        drop(s);
        ui.invoke_knowledge_focus();
        launch("memory", json!({"memory_id":id,"max_chars":8000}));
    }
}

fn neighbors(incoming: bool) {
    let Some((_, _, state)) = active() else {
        return;
    };
    let s = state.borrow();
    let payload = json!({"memory_id":s.selected,"incoming":incoming,"cursor":if incoming { &s.incoming_cursor } else { &s.outgoing_cursor },"max_chars":8000});
    drop(s);
    launch(if incoming { "incoming" } else { "outgoing" }, payload);
}

fn rows(value: &Value, topic: bool) -> Vec<KnowledgeRow> {
    value["memories"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let stale = item["knowledge"]["needs_review"] == true
                || item["relations"]
                    .as_array()
                    .is_some_and(|rels| rels.iter().any(|r| r["needs_review"] == true));
            let derived = item["relations"]
                .as_array()
                .is_some_and(|rels| rels.iter().any(|r| r["relation"] == "derived_from"));
            let mut note = if topic {
                "主题入口"
            } else if derived {
                "派生依据"
            } else {
                "引用"
            }
            .to_owned();
            note.push_str(&format!(" · 修订 {}", item["revision"]));
            if stale {
                note.push_str(" · 依据已变化，需核对");
            }
            KnowledgeRow {
                id: item["memory_id"].as_str().unwrap_or_default().into(),
                title: item["title"].as_str().unwrap_or("未命名记忆").into(),
                note: note.into(),
                needs_review: stale,
            }
        })
        .collect()
}

fn append(model: ModelRc<KnowledgeRow>, rows: Vec<KnowledgeRow>) -> ModelRc<KnowledgeRow> {
    let mut all: Vec<_> = model.iter().collect();
    all.extend(rows);
    ModelRc::new(VecModel::from(all))
}

#[wasm_bindgen]
pub fn receive_knowledge(operation: String, generation: u32, status: u16, body: String) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() {
        return;
    }
    let accepted = if operation == "navigation" {
        state.borrow_mut().nav.accept(generation)
    } else {
        state.borrow_mut().reader.accept(generation)
    };
    if accepted.as_deref() != Some(operation.as_str()) {
        return;
    }
    if operation == "navigation" {
        ui.set_knowledge_nav_busy(false);
    } else {
        ui.set_knowledge_busy(false);
    }
    if status == 401 {
        crate::browser::end_session();
        return;
    }
    let parsed = serde_json::from_str::<Value>(&body);
    if status != 200 || parsed.is_err() {
        if operation == "navigation" {
            ui.set_knowledge_nav_error("主题暂时无法读取，请刷新重试。".into());
        } else {
            if operation == "memory" {
                state.borrow_mut().body.clear();
                ui.set_knowledge_body("".into());
                ui.set_knowledge_title("记忆读取失败".into());
            }
            ui.set_knowledge_error(
                if status == 404 {
                    "这条记忆已不存在或已被遗忘。"
                } else {
                    "关联阅读暂时失败，请关闭后重试。"
                }
                .into(),
            );
        }
        return;
    }
    let Ok(value) = parsed else {
        return;
    };
    match operation.as_str() {
        "navigation" => {
            state.borrow_mut().loaded = true;
            state.borrow_mut().nav_cursor = value["next_cursor"].as_str().map(str::to_owned);
            ui.set_knowledge_nav_more(value["next_cursor"].is_string());
            ui.set_knowledge_topics(append(ui.get_knowledge_topics(), rows(&value, true)));
        }
        "memory" => {
            if value["hidden"] == true {
                state.borrow_mut().body.clear();
                ui.set_knowledge_body("".into());
                ui.set_knowledge_title("记忆已被遗忘".into());
                ui.set_knowledge_error("这条记忆已被遗忘。".into());
                return;
            }
            if let Some(target) = value["redirect_to"].as_str() {
                open(target, false);
                return;
            }
            let content = value["content"].as_str().unwrap_or_default();
            let mut s = state.borrow_mut();
            s.body.push_str(content);
            ui.set_knowledge_body(s.body.clone().into());
            ui.set_knowledge_title(
                s.body
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("记忆")
                    .chars()
                    .take(90)
                    .collect::<String>()
                    .into(),
            );
            let recall_note = s
                .recall_hit
                .as_ref()
                .filter(|_| s.stack.is_empty())
                .map(|hit| {
                    hit.reading_note(
                        &s.selected,
                        value["revision"].as_i64().unwrap_or_default(),
                        value["is_current"] != false,
                    )
                })
                .unwrap_or_default();
            ui.set_knowledge_note(
                format!(
                    "{} · 修订 {}{recall_note}{}",
                    if value["memory_type"] == "topic" {
                        "主题"
                    } else {
                        "记忆"
                    },
                    value["revision"],
                    if value["pending_revisions"].as_u64().unwrap_or(0) > 0 {
                        " · 历史建议待 AI 复核，使用前核对来源"
                    } else if value["knowledge"]["needs_review"] == true {
                        " · 依据已变化，需要核对"
                    } else {
                        ""
                    }
                )
                .into(),
            );
            let cursor = value["next_cursor"].as_str().map(str::to_owned);
            let id = s.selected.clone();
            drop(s);
            if let Some(cursor) = cursor {
                launch(
                    "memory",
                    json!({"memory_id":id,"cursor":cursor,"max_chars":8000}),
                );
            } else {
                neighbors(false);
            }
        }
        "outgoing" | "incoming" => {
            let incoming = operation == "incoming";
            let cursor = value["next_cursor"].as_str().map(str::to_owned);
            if incoming {
                state.borrow_mut().incoming_cursor = cursor.clone();
                ui.set_knowledge_incoming_more(cursor.is_some());
                ui.set_knowledge_incoming(append(ui.get_knowledge_incoming(), rows(&value, false)));
            } else {
                state.borrow_mut().outgoing_cursor = cursor.clone();
                ui.set_knowledge_outgoing_more(cursor.is_some());
                ui.set_knowledge_outgoing(append(ui.get_knowledge_outgoing(), rows(&value, false)));
                if ui.get_knowledge_incoming().row_count() == 0 {
                    neighbors(true);
                }
            }
        }
        _ => (),
    }
}
