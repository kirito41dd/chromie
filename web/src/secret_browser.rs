#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::{Dashboard, secret_view::SecretView, session::Session};
use slint::{ComponentHandle, SharedString};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = chromieRevealSecret)]
    fn request_secret(space: &str, memory_id: &str, password: &str, generation: u32);
    #[wasm_bindgen(js_namespace = window, js_name = chromieCancelSecret)]
    fn cancel_secret();
    #[wasm_bindgen(js_namespace = window, js_name = chromieSecretVerified)]
    fn secret_verified(space: &str) -> bool;
    #[wasm_bindgen(js_namespace = window, js_name = chromieCopySecret)]
    fn copy_secret(value: &str, generation: u32);
}

struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<SecretView>>,
}
thread_local! {
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}
type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<SecretView>>);
fn active() -> Option<Active> {
    CONTROLLER.with_borrow(|current| {
        let c = current.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}

pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    CONTROLLER.with_borrow_mut(|current| {
        *current = Some(Controller {
            ui: ui.as_weak(),
            session,
            state: Rc::new(RefCell::new(SecretView::default())),
        })
    });
    ui.on_show_secret(|id, label| {
        close();
        let Some((ui, session, state)) = active() else {
            return;
        };
        if !ui.get_unlocked() || !state.borrow_mut().open(&session.borrow().space, &id) {
            return;
        }
        ui.set_secret_label(label);
        ui.set_secret_verified(secret_verified(&session.borrow().space));
        ui.set_secret_open(true);
        focus_action(&ui);
    });
    ui.on_close_secret(close);
    ui.on_copy_secret(|formatted| {
        let Some((ui, _, state)) = active() else {
            return;
        };
        if !ui.get_unlocked() || !ui.get_secret_open() || !ui.get_secret_revealed() {
            return;
        }
        let Some(generation) = state.borrow().visible_generation() else {
            return;
        };
        ui.set_secret_copy_notice(SharedString::default());
        let value = if formatted && ui.get_secret_json() {
            ui.get_secret_formatted_value()
        } else {
            ui.get_secret_value()
        };
        copy_secret(&value, generation);
    });
    ui.on_toggle_secret(|| {
        let Some((ui, _, _)) = active() else {
            return;
        };
        if ui.get_secret_revealed() {
            hide_secret();
        } else {
            verify();
        }
    });
}

fn clear_value(ui: &Dashboard) {
    crate::selection::clear();
    ui.set_secret_revealed(false);
    ui.set_secret_json(false);
    ui.set_secret_lines(slint::ModelRc::default());
    ui.set_secret_value(SharedString::default());
    ui.set_secret_formatted_value(SharedString::default());
    ui.set_secret_password(SharedString::default());
    ui.set_secret_loading(false);
    ui.set_secret_error(SharedString::default());
    ui.set_secret_copy_notice(SharedString::default());
}

#[wasm_bindgen]
pub fn receive_secret_copy(generation: u32, success: bool) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked()
        || !ui.get_secret_open()
        || state.borrow().visible_generation() != Some(generation)
    {
        return;
    }
    ui.set_secret_copy_notice(
        if success {
            "已复制全部内容"
        } else {
            "复制失败，请选中文本后复制"
        }
        .into(),
    );
}

fn focus_action(ui: &Dashboard) {
    if ui.get_secret_verified() {
        ui.invoke_focus_secret_toggle();
    } else {
        ui.invoke_focus_secret_password();
    }
}

#[wasm_bindgen]
pub fn receive_secret_verification(verified: bool) {
    if let Some((ui, _, _)) = active() {
        ui.set_secret_verified(verified);
        if ui.get_secret_open() && !ui.get_secret_revealed() && !ui.get_secret_loading() {
            focus_action(&ui);
        }
    }
}

pub(crate) fn close() {
    if let Some((ui, _, state)) = active() {
        state.borrow_mut().close();
        let was_open = ui.get_secret_open();
        ui.set_secret_open(false);
        clear_value(&ui);
        ui.set_secret_label(SharedString::default());
        if was_open && ui.get_unlocked() {
            ui.invoke_return_secret_focus();
        }
    }
    cancel_secret();
}

#[wasm_bindgen]
pub fn hide_secret() {
    if let Some((ui, session, state)) = active() {
        state.borrow_mut().hide();
        clear_value(&ui);
        ui.set_secret_verified(secret_verified(&session.borrow().space));
        if ui.get_secret_open() && ui.get_unlocked() {
            focus_action(&ui);
        }
    }
    cancel_secret();
}

fn verify() {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_secret_open() || ui.get_secret_loading() {
        return;
    }
    let password = ui.get_secret_password().to_string();
    ui.set_secret_password(SharedString::default());
    let verified = secret_verified(&session.borrow().space);
    ui.set_secret_verified(verified);
    let Some(next) = state.borrow_mut().request(password, verified) else {
        ui.set_secret_error("请输入当前空间密码。".into());
        focus_action(&ui);
        return;
    };
    ui.set_secret_error(SharedString::default());
    ui.set_secret_loading(true);
    request_secret(
        &next.space,
        &next.memory_id,
        &next.password,
        next.generation,
    );
}

#[wasm_bindgen]
pub fn receive_secret(generation: u32, status: u16, body: String, unsupported: bool) -> bool {
    let Some((ui, session, state)) = active() else {
        return false;
    };
    if !ui.get_unlocked() || !ui.get_secret_open() {
        return false;
    }
    let success = status == 200 && !unsupported && !body.is_empty() && body.len() <= 60000;
    if !state
        .borrow_mut()
        .finish(generation, &session.borrow().space, success)
    {
        return false;
    }
    ui.set_secret_loading(false);
    if success {
        let rows = crate::secret_format::json_lines(&body);
        ui.set_secret_json(rows.is_some());
        if let Some(rows) = rows {
            ui.set_secret_formatted_value(crate::secret_format::formatted_text(&rows).into());
            ui.set_secret_lines(slint::ModelRc::new(slint::VecModel::from(
                rows.into_iter()
                    .map(|line| crate::SecretLine {
                        key: line.key.into(),
                        value: line.value.into(),
                        kind: line.kind,
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        ui.set_secret_value(body.into());
        ui.set_secret_revealed(true);
        ui.set_secret_error(SharedString::default());
        ui.invoke_focus_secret_toggle();
        true
    } else {
        clear_value(&ui);
        ui.set_secret_error(
            match status {
                401 => "空间密码不正确，请重试。",
                404 => "这条 Secret 不存在或已被遗忘。",
                429 => "验证过于频繁，请稍后重试。",
                200 if unsupported => "该 Secret 不是可显示的文本，请通过本地文件取用。",
                _ => "暂时无法读取 Secret，请稍后重试。",
            }
            .into(),
        );
        focus_action(&ui);
        false
    }
}
