#![cfg_attr(not(test), forbid(unsafe_code))]
use crate::{
    Dashboard,
    model_settings::{ConnectionTest, Settings, error_label, parse_batch_size, parse_similarity},
    session::Session,
};
use serde_json::json;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window,js_name=chromieModelRequest)]
    fn send_model(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window,js_name=chromieCancelModel)]
    fn cancel_model();
}
#[derive(Default)]
struct State {
    generation: u32,
    pending: Option<(String, bool)>,
    settings: Settings,
    edit_revision: i64,
    dirty: bool,
    confirmation: Option<Confirmation>,
}
// 确认绑定点击时的配置快照，轮询与后续表单变化不能改变用户确认的操作。
enum Confirmation {
    Save(String),
    Disable(i64),
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
    CONTROLLER.with_borrow(|value| {
        let c = value.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}

fn send(operation: &str, payload: String, fill: bool) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_settings_page() || !ui.get_can_manage_system() {
        return;
    }
    // 自动刷新的读取可以被保存替换；否则点击恰好撞上轮询会丢失提交。
    if let Some((pending, _)) = &state.borrow().pending
        && (pending != "read" || operation == "read")
    {
        return;
    }
    let generation = {
        let mut state = state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.pending = Some((operation.into(), fill));
        state.generation
    };
    ui.set_model_busy(operation != "read");
    ui.set_model_testing(operation == "test");
    send_model(&session.borrow().space, operation, &payload, generation);
}
pub(crate) fn load() {
    send("read", "{}".into(), true);
}

fn render(ui: &Dashboard, value: &Settings, fill: bool) {
    let usage = value.usage.clone().unwrap_or_default();
    ui.set_model_usage_available(usage.available);
    ui.set_model_usage_today(usage.today.view());
    ui.set_model_usage_week(usage.seven_days.view());
    ui.set_model_usage_total(usage.all_time.view());
    ui.set_model_usage_note(usage.note().into());
    ui.set_model_usage_days(std::rc::Rc::new(slint::VecModel::from(usage.rows())).into());
    ui.set_model_loaded(true);
    ui.set_model_enabled(value.enabled);
    ui.set_model_active_label(if !value.enabled {
        "语义检索未启用".into()
    } else if let Some(active) = &value.active {
        format!(
            "当前生效 · {} · {} 维",
            active.profile.model, active.profile.dimensions
        )
        .into()
    } else {
        "正在准备首次语义索引，期间使用关键词检索".into()
    });
    ui.set_model_pending_label(
        value
            .pending
            .as_ref()
            .map(|g| {
                format!(
                    "正在准备 · {} · {} 维 · {}",
                    g.profile.model,
                    g.profile.dimensions,
                    if g.scan_complete {
                        "追平变更"
                    } else {
                        "扫描并生成向量"
                    }
                )
            })
            .unwrap_or_default()
            .into(),
    );
    if let Some(target) = value.target() {
        ui.set_model_progress(target.progress.label().into());
        ui.set_model_ratio(target.progress.ratio());
        ui.set_model_has_key(target.has_api_key);
        if fill {
            ui.set_model_url(target.base_url.clone().into());
            ui.set_model_name(target.profile.model.clone().into());
            ui.set_model_version(target.profile.model_version.clone().into());
            ui.set_model_dimensions(target.profile.dimensions.to_string().into());
            ui.set_model_concurrency(target.max_concurrency.to_string().into());
            ui.set_model_batch_size(
                target
                    .batch_size_override
                    .map(|n| n.to_string())
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_model_similarity(
                target
                    .similarity_threshold_override
                    .map(|n| n.to_string())
                    .unwrap_or_default()
                    .into(),
            );
        }
    } else {
        ui.set_model_progress("".into());
        ui.set_model_ratio(0.0);
        ui.set_model_has_key(false);
    }
    if fill {
        ui.set_model_key("".into());
        ui.set_model_clear_key(false);
    }
}

fn ask_confirmation(
    ui: &Dashboard,
    state: &RefCell<State>,
    title: &str,
    action: &str,
    description: String,
    intent: Confirmation,
) {
    state.borrow_mut().confirmation = Some(intent);
    ui.set_model_confirm_title(title.into());
    ui.set_model_confirm_action(action.into());
    ui.set_model_confirm(description.into());
    ui.invoke_focus_model_confirm();
}

fn submit(operation: &str) {
    let Some((ui, _session, state)) = active() else {
        return;
    };
    let dimensions = match ui.get_model_dimensions().parse::<u32>() {
        Ok(n) if (1..=8192).contains(&n) => n,
        _ => {
            ui.set_model_error("维度须为 1～8192 的整数。".into());
            return;
        }
    };
    let max_concurrency = match ui.get_model_concurrency().parse::<u32>() {
        Ok(value) if (1..=64).contains(&value) => value,
        _ => {
            ui.set_model_error("最大并发须为 1～64 的整数。".into());
            return;
        }
    };
    let batch_size = match parse_batch_size(ui.get_model_batch_size().as_str()) {
        Ok(value) => value,
        Err(message) => {
            ui.set_model_error(message.into());
            return;
        }
    };
    let similarity_threshold = match parse_similarity(ui.get_model_similarity().as_str()) {
        Ok(value) => value,
        Err(message) => {
            ui.set_model_error(message.into());
            return;
        }
    };
    let changed = state.borrow().settings.target().is_some_and(|target| {
        target.profile.model != ui.get_model_name().as_str()
            || target.profile.model_version != ui.get_model_version().as_str()
            || target.profile.dimensions != dimensions
    });
    let needs_confirmation = operation == "save"
        && (changed
            || (!state.borrow().settings.enabled && state.borrow().settings.target().is_some()));
    let payload=json!({"expected_revision":state.borrow().edit_revision,"base_url":ui.get_model_url().as_str(),"model":ui.get_model_name().as_str(),"model_version":ui.get_model_version().as_str(),"dimensions":dimensions,"max_concurrency":max_concurrency,"batch_size":batch_size,"similarity_threshold":similarity_threshold,"api_key":if ui.get_model_key().is_empty(){None}else{Some(ui.get_model_key().to_string())},"clear_api_key":ui.get_model_clear_key(),"confirm_rebuild":needs_confirmation,"force_rebuild":false}).to_string();
    if needs_confirmation {
        let count = state
            .borrow()
            .settings
            .target()
            .map(|g| g.progress.total)
            .unwrap_or(0);
        ask_confirmation(
            &ui,
            &state,
            "更换模型并重建？",
            "确认并重建",
            rebuild_description(count),
            Confirmation::Save(payload),
        );
        return;
    }
    // 测试后仍可直接保存刚输入的 Key；只保留当前表单内存，不写浏览器存储。
    if operation == "save" {
        ui.set_model_key("".into());
    }
    ui.set_model_error("".into());
    ui.set_model_notice("".into());
    send(operation, payload, operation == "save");
}

fn rebuild_description(count: u64) -> String {
    format!(
        "将为全部空间的 {count} 条有效记忆重新生成向量，会产生模型调用费用。\n\n重建期间保留现有检索，完成后自动切换。如有正在进行的重建，将替换该任务。"
    )
}

pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    let state = Rc::new(RefCell::new(State::default()));
    ui.on_model_save(|| submit("save"));
    ui.on_model_confirm_save(|| {
        if let Some((ui, _, state)) = active() {
            let intent = state.borrow_mut().confirmation.take();
            ui.set_model_confirm("".into());
            ui.set_model_error("".into());
            ui.set_model_notice("".into());
            match intent {
                Some(Confirmation::Save(payload)) => {
                    ui.set_model_key("".into());
                    send("save", payload, true);
                }
                Some(Confirmation::Disable(revision)) => send(
                    "disable",
                    json!({"expected_revision":revision}).to_string(),
                    true,
                ),
                None => {}
            }
        }
    });
    ui.on_model_test(|| submit("test"));
    ui.on_model_edited(|| {
        if let Some((ui, _, state)) = active() {
            state.borrow_mut().dirty = true;
            state.borrow_mut().confirmation = None;
            ui.set_model_confirm("".into());
            ui.set_model_notice("".into());
        }
    });
    ui.on_model_cancel_confirm(|| {
        if let Some((ui, _, state)) = active() {
            state.borrow_mut().confirmation = None;
            ui.set_model_confirm("".into());
        }
    });
    ui.on_model_rebuild(|| {
        if let Some((ui, _, state)) = active() {
            let settings = state.borrow().settings.clone();
            if let Some(target) = settings.target() {
                let payload = json!({"expected_revision":settings.revision,"base_url":target.base_url,"model":target.profile.model,"model_version":target.profile.model_version,"dimensions":target.profile.dimensions,"max_concurrency":target.max_concurrency,"api_key":null,"clear_api_key":false,"confirm_rebuild":true,"force_rebuild":true}).to_string();
                ask_confirmation(&ui, &state, "重建全部语义索引？", "确认重建", rebuild_description(target.progress.total), Confirmation::Save(payload));
            }
        }
    });
    ui.on_model_disable(|| {
        if let Some((ui, _, state)) = active() {
            let revision = state.borrow().settings.revision;
            ask_confirmation(&ui, &state, "停用语义检索？", "确认停用", "全部空间将切回关键词检索，正在进行的索引构建会停止。\n\n记忆、采集和 Secret 保留。再次启用时，需要重新构建语义索引。".into(), Confirmation::Disable(revision));
        }
    });
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(3), || {
        if let Some((ui, _, state)) = active()
            && ui.get_settings_page()
            && ui.get_unlocked()
            && ui.get_can_manage_system()
            && state.borrow().pending.is_none()
        {
            let fill = !state.borrow().dirty;
            send("read", "{}".into(), fill);
        }
    });
    CONTROLLER.with_borrow_mut(|current| {
        *current = Some(Controller {
            ui: ui.as_weak(),
            session,
            state,
            _timer: timer,
        })
    });
}

pub(crate) fn clear() {
    if let Some((ui, _, state)) = active() {
        let generation = state.borrow().generation.wrapping_add(1);
        *state.borrow_mut() = State {
            generation,
            ..Default::default()
        };
        ui.set_model_key("".into());
        ui.set_model_busy(false);
        ui.set_model_testing(false);
        ui.set_model_loaded(false);
        ui.set_model_usage_available(false);
        ui.set_model_usage_days(
            std::rc::Rc::new(slint::VecModel::from(Vec::<crate::UsageDay>::new())).into(),
        );
        ui.set_model_confirm("".into());
        ui.set_model_error("".into());
        ui.set_model_notice("".into());
    }
    cancel_model();
}

#[wasm_bindgen]
pub fn receive_model(generation: u32, status: u16, body: String) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if generation != state.borrow().generation {
        return;
    }
    let Some((operation, fill)) = state.borrow_mut().pending.take() else {
        return;
    };
    ui.set_model_busy(false);
    ui.set_model_testing(false);
    if status == 401 {
        crate::browser::end_session();
        return;
    }
    if status != 200 {
        let code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["code"].as_str().map(str::to_owned));
        ui.set_model_error(
            if status == 403 {
                "当前空间没有系统管理权限。"
            } else {
                error_label(code.as_deref().unwrap_or("unknown"))
            }
            .into(),
        );
        return;
    }
    if operation == "test" {
        match serde_json::from_str::<ConnectionTest>(&body) {
            Ok(result) => ui.set_model_notice(result.label().into()),
            Err(_) => ui.set_model_error("连接测试响应无效，请重试。".into()),
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Settings>(&body) else {
        ui.set_model_error("模型配置响应无效，请刷新重试。".into());
        return;
    };
    // 发出读取之后用户仍可编辑，迟到的轮询响应只能更新进度。
    let fill = fill && (operation != "read" || !state.borrow().dirty);
    render(&ui, &value, fill);
    let mut state = state.borrow_mut();
    if fill {
        state.edit_revision = value.revision;
        state.dirty = false;
    }
    state.settings = value;
    if operation != "read" {
        ui.set_model_confirm("".into());
        ui.set_model_notice(
            if operation == "disable" {
                "已停用语义检索，关键词检索继续可用。"
            } else {
                "配置已保存，后台会按当前设置处理记忆。"
            }
            .into(),
        );
    }
}
