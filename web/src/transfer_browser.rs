#![cfg_attr(not(test), forbid(unsafe_code))]
use crate::{
    Dashboard, TransferRow,
    session::Session,
    transfer_access::{Access, Action, Status, validate},
    transfer_view::{Reply, error},
};
use serde_json::json;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window,js_name=chromieTransferRequest)]
    fn send_transfer(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window,js_name=chromieChooseTransfer)]
    fn choose_transfer();
    #[wasm_bindgen(js_namespace=window,js_name=chromieCancelTransfer)]
    fn cancel_transfer();
    #[wasm_bindgen(js_namespace=window,js_name=chromieNow)]
    fn now() -> f64;
}
#[derive(Default)]
struct State {
    generation: u32,
    pending: Option<Request>,
    access: Access,
    digest: String,
    page: usize,
    has_more: bool,
}
#[derive(Clone, Copy)]
enum Request {
    Status,
    Unlock,
    Lock,
    Action(Action),
}
impl Request {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "unlock-status",
            Self::Unlock => "unlock",
            Self::Lock => "lock",
            Self::Action(action) => action.name(),
        }
    }
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
    CONTROLLER.with_borrow(|v| {
        let c = v.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}
fn invalidate(ui: &Dashboard, state: &Rc<RefCell<State>>) {
    {
        let mut s = state.borrow_mut();
        s.digest.clear();
        s.page = 0;
        s.has_more = false;
    }
    ui.set_transfer_preview_ready(false);
    ui.set_transfer_summary("".into());
    ui.set_transfer_source("".into());
    ui.set_transfer_rows(Default::default());
    ui.set_transfer_stage("".into());
    ui.set_transfer_detail("".into());
    ui.set_transfer_confirm_label("确认夺舍".into());
    ui.set_transfer_page_note("".into());
    ui.set_transfer_previous(false);
    ui.set_transfer_next(false);
}
fn access_ui(ui: &Dashboard, state: &State) {
    let remaining = state.access.remaining(now());
    ui.set_transfer_authorized(remaining > 0);
    ui.set_transfer_access_note(if matches!(state.pending, Some(Request::Status)) {
        "正在检查迁移解锁状态…".into()
    } else if remaining > 0 {
        format!(
            "剩余 {:02}:{:02} · 刷新和切页可继续",
            remaining / 60,
            remaining % 60
        )
        .into()
    } else {
        "操作时验证空间密码，解锁后 10 分钟内可继续。".into()
    });
}
fn send_request(operation: Request, payload: serde_json::Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_transfer_page() || state.borrow().pending.is_some() {
        return;
    }
    let generation = {
        let mut s = state.borrow_mut();
        s.generation = s.generation.wrapping_add(1);
        s.pending = Some(operation);
        s.generation
    };
    ui.set_transfer_busy(true);
    if matches!(operation, Request::Status | Request::Lock) {
        ui.set_transfer_access_error("".into());
    }
    access_ui(&ui, &state.borrow());
    send_transfer(
        &session.borrow().space,
        operation.name(),
        &payload.to_string(),
        generation,
    );
}
fn open_unlock(action: Option<Action>) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_transfer_page() || state.borrow().pending.is_some() {
        return;
    }
    state.borrow_mut().access.defer(action);
    ui.set_transfer_password("".into());
    ui.set_transfer_auth_error("".into());
    ui.set_transfer_auth_action(action.map_or("解锁迁移", Action::unlock_label).into());
    ui.set_transfer_auth_open(true);
    ui.invoke_focus_transfer_password();
}
fn send(action: Action) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_transfer_page() || state.borrow().pending.is_some() {
        return;
    }
    ui.set_transfer_export_feedback(matches!(action, Action::Export));
    ui.set_transfer_error("".into());
    ui.set_transfer_notice("".into());
    let phrase = if matches!(action, Action::Export) {
        ui.get_transfer_export_phrase()
    } else {
        ui.get_transfer_import_phrase()
    };
    if let Some(message) = validate(
        action,
        !ui.get_transfer_file().is_empty(),
        &phrase,
        &ui.get_transfer_export_repeat(),
    ) {
        ui.set_transfer_error(message.into());
        return;
    }
    if matches!(action, Action::Confirm)
        && (!ui.get_transfer_preview_ready() || state.borrow().digest.is_empty())
    {
        return;
    }
    if state.borrow().access.remaining(now()) == 0 {
        open_unlock(Some(action));
        return;
    }
    if matches!(action, Action::Preview { bound: false, .. }) {
        invalidate(&ui, &state);
    }
    let mut payload = json!({"passphrase":phrase.as_str()});
    if let Action::Preview { page, bound } = action {
        payload["page"] = json!(page);
        if bound {
            payload["preview_digest"] = json!(state.borrow().digest);
        }
    } else if matches!(action, Action::Confirm) {
        payload["preview_digest"] = json!(state.borrow().digest);
    }
    send_request(Request::Action(action), payload);
}
pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    let state = Rc::new(RefCell::new(State::default()));
    let timer = Timer::default();
    let weak = ui.as_weak();
    let ticks = state.clone();
    timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        if let Some(ui) = weak.upgrade()
            && ui.get_unlocked()
            && ui.get_transfer_page()
        {
            access_ui(&ui, &ticks.borrow());
        }
    });
    CONTROLLER.with_borrow_mut(|v| {
        *v = Some(Controller {
            ui: ui.as_weak(),
            session,
            state,
            _timer: timer,
        })
    });
    ui.on_transfer_export(|| send(Action::Export));
    ui.on_transfer_preview(|| {
        send(Action::Preview {
            page: 0,
            bound: false,
        })
    });
    ui.on_transfer_confirm(|| send(Action::Confirm));
    ui.on_transfer_open_unlock(|| open_unlock(None));
    ui.on_transfer_lock(|| send_request(Request::Lock, json!({})));
    ui.on_transfer_cancel_unlock(|| {
        if let Some((ui, _, state)) = active()
            && state.borrow().pending.is_none()
        {
            state.borrow_mut().access.defer(None);
            ui.set_transfer_auth_open(false);
            ui.set_transfer_password("".into());
            ui.set_transfer_auth_error("".into());
            ui.invoke_return_transfer_focus();
        }
    });
    ui.on_transfer_auth_edited(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_transfer_auth_error("".into());
        }
    });
    ui.on_transfer_submit_unlock(|| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        if state.borrow().pending.is_some() || !ui.get_transfer_auth_open() {
            return;
        }
        let password = ui.get_transfer_password().to_string();
        if password.is_empty() || password.len() > 1024 {
            ui.set_transfer_auth_error("请输入当前空间密码。".into());
            return;
        }
        ui.set_transfer_password("".into());
        ui.set_transfer_auth_error("".into());
        send_request(Request::Unlock, json!({"space_password":password}));
    });
    ui.on_transfer_turn(|direction| {
        let Some((_, _, state)) = active() else {
            return;
        };
        let page = {
            let s = state.borrow();
            if s.digest.is_empty() {
                return;
            }
            match direction {
                -1 if s.page > 0 => s.page - 1,
                1 if s.has_more => s.page + 1,
                _ => return,
            }
        };
        send(Action::Preview { page, bound: true });
    });
    ui.on_transfer_edited(|| {
        if let Some((ui, _, state)) = active() {
            invalidate(&ui, &state);
        }
    });
    ui.on_transfer_choose(|| {
        if let Some((ui, _, state)) = active() {
            ui.set_transfer_export_feedback(false);
            ui.set_transfer_error("".into());
            ui.set_transfer_notice("".into());
            invalidate(&ui, &state);
            choose_transfer();
        }
    });
}
pub(crate) fn clear() {
    cancel_transfer();
    if let Some((ui, _, state)) = active() {
        {
            let mut s = state.borrow_mut();
            s.generation = s.generation.wrapping_add(1);
            s.pending = None;
        }
        invalidate(&ui, &state);
        ui.set_transfer_busy(false);
        ui.set_transfer_password("".into());
        state.borrow_mut().access = Access::default();
        ui.set_transfer_auth_open(false);
        ui.set_transfer_auth_error("".into());
        ui.set_transfer_access_error("".into());
        access_ui(&ui, &state.borrow());
        ui.set_transfer_export_phrase("".into());
        ui.set_transfer_export_repeat("".into());
        ui.set_transfer_import_phrase("".into());
        ui.set_transfer_file("".into());
        ui.set_transfer_error("".into());
        ui.set_transfer_notice("".into());
    }
}
pub(crate) fn load() {
    send_request(Request::Status, json!({}));
}
#[wasm_bindgen]
pub fn receive_transfer_file(name: String, size: f64) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_transfer_page() {
        return;
    }
    invalidate(&ui, &state);
    if !(1.0..=65.0 * 1024.0 * 1024.0).contains(&size) {
        ui.set_transfer_file("".into());
        ui.set_transfer_error("文件为空或超过 65 MiB 上限。".into());
        return;
    }
    ui.set_transfer_file(name.into());
    ui.set_transfer_error("".into());
}
#[wasm_bindgen]
pub fn receive_transfer(generation: u32, status: u16, text: String) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_transfer_page() || state.borrow().generation != generation {
        return;
    }
    let Some(operation) = state.borrow_mut().pending.take() else {
        return;
    };
    ui.set_transfer_busy(false);
    if !matches!(operation, Request::Action(_)) {
        if !(200..300).contains(&status) {
            if matches!(operation, Request::Unlock) {
                let code = serde_json::from_str::<serde_json::Value>(&text).ok();
                ui.set_transfer_auth_error(
                    if code
                        .as_ref()
                        .is_some_and(|v| v["code"] == "transfer_password_invalid")
                    {
                        "空间密码有误，请重新输入。"
                    } else if status == 401 {
                        "登录已失效，请重新登录空间。"
                    } else {
                        "解锁未完成，请检查连接后重试。"
                    }
                    .into(),
                );
                ui.invoke_focus_transfer_password();
            } else {
                ui.set_transfer_access_error(
                    if matches!(operation, Request::Lock) {
                        "锁定未完成，请重试。"
                    } else {
                        "无法检查解锁状态，点击操作时可重新验证。"
                    }
                    .into(),
                );
            }
            access_ui(&ui, &state.borrow());
            return;
        }
        let Some(status) = Status::parse(&text) else {
            if matches!(operation, Request::Unlock) {
                ui.set_transfer_auth_error("服务返回的解锁状态无效，请重试。".into());
            } else {
                ui.set_transfer_access_error("服务返回的解锁状态无效，请重试。".into());
            }
            return;
        };
        state.borrow_mut().access.receive(status, now());
        ui.set_transfer_access_error("".into());
        access_ui(&ui, &state.borrow());
        if matches!(operation, Request::Lock) {
            state.borrow_mut().access.defer(None);
        }
        if matches!(operation, Request::Unlock) {
            ui.set_transfer_auth_open(false);
            ui.set_transfer_password("".into());
            ui.set_transfer_auth_error("".into());
            let deferred = state.borrow_mut().access.take_deferred();
            ui.invoke_return_transfer_focus();
            if let Some(action) = deferred {
                send(action);
            }
        }
        return;
    }
    let Request::Action(operation) = operation else {
        return;
    };
    if !(200..300).contains(&status) {
        if status == 403
            && serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .is_some_and(|v| v["code"] == "transfer_locked")
        {
            state.borrow_mut().access.lock();
            access_ui(&ui, &state.borrow());
            open_unlock(Some(operation));
            return;
        }
        ui.set_transfer_error(error(status, &text).into());
        invalidate(&ui, &state);
        return;
    }
    if matches!(operation, Action::Export) {
        ui.set_transfer_notice("炼化包已下载，请与迁移口令分开妥善保管。".into());
        ui.set_transfer_export_phrase("".into());
        ui.set_transfer_export_repeat("".into());
        return;
    }
    let Some(reply) = Reply::parse(&text) else {
        ui.set_transfer_error("服务返回的预览格式无效，请重试。".into());
        invalidate(&ui, &state);
        return;
    };
    if let Action::Preview { page, .. } = operation
        && !reply.completed()
        && reply.page != page
    {
        ui.set_transfer_error("服务返回的预览页码无效，请重新预览。".into());
        invalidate(&ui, &state);
        return;
    }
    ui.set_transfer_source(format!("来自 {} · {}", reply.source_name, reply.source_space).into());
    ui.set_transfer_summary(reply.summary().into());
    let completed = reply.completed();
    ui.set_transfer_preview_ready(reply.can_confirm());
    ui.set_transfer_stage(reply.stage().into());
    ui.set_transfer_detail(reply.detail().into());
    ui.set_transfer_confirm_label(if completed {
        "已完成".into()
    } else if reply.new == 0 {
        "无新增项".into()
    } else {
        format!("确认夺舍 · {} 条", reply.new).into()
    });
    ui.set_transfer_page_note(reply.page_note().into());
    ui.set_transfer_previous(!completed && reply.page > 0);
    ui.set_transfer_next(!completed && reply.has_more);
    {
        let mut s = state.borrow_mut();
        s.page = reply.page;
        s.has_more = reply.has_more;
    }
    let rows = reply
        .items
        .into_iter()
        .map(|r| {
            let metadata = r.metadata();
            TransferRow {
                label: r.excerpt.into(),
                kind: if r.kind == "secret" {
                    "Secret · 仅公开说明"
                } else {
                    "记忆"
                }
                .into(),
                status: match r.status.as_str() {
                    "new" => "新增",
                    "existing" => "跳过",
                    _ => "冲突",
                }
                .into(),
                reason: format!("{} · {}", r.reason, metadata).into(),
                conflict: r.status == "conflict",
            }
        })
        .collect::<Vec<_>>();
    ui.set_transfer_rows(Rc::new(slint::VecModel::from(rows)).into());
    state.borrow_mut().digest = reply.preview_digest;
    if completed {
        ui.set_transfer_notice("".into());
        ui.set_transfer_password("".into());
        ui.set_transfer_import_phrase("".into());
        cancel_transfer();
        ui.set_transfer_file("".into());
    }
}
