#![forbid(unsafe_code)]
use crate::{Dashboard, messages::Gate, session::Session};
use serde_json::{Value, json};
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window,js_name=chromieProviderRequest)]
    fn send(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window,js_name=chromieCancelProvider)]
    fn cancel();
}
#[derive(Default)]
struct State {
    providers: Vec<Value>,
    gate: Gate,
    organization: Gate,
    loaded: bool,
    org_loaded: bool,
    dirty: bool,
    org_dirty: bool,
    provider_revision: i64,
    org_revision: i64,
    embedding: Option<String>,
    text: Option<String>,
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<State>>,
}
thread_local! {static CONTROLLER:RefCell<Option<Controller>>=const {RefCell::new(None)};}
type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<State>>);
fn active() -> Option<Active> {
    CONTROLLER.with_borrow(|c| {
        let c = c.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}
fn launch(operation: &str, payload: Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() || !ui.get_settings_page() || !ui.get_can_manage_system() {
        return;
    }
    let generation = if operation.starts_with("organization") {
        ui.set_organization_busy(true);
        ui.set_organization_error("".into());
        state.borrow_mut().organization.begin(operation)
    } else {
        ui.set_provider_busy(true);
        ui.set_provider_error("".into());
        state.borrow_mut().gate.begin(operation)
    };
    send(
        &session.borrow().space,
        operation,
        &payload.to_string(),
        generation,
    );
}
fn fill(ui: &Dashboard, state: &RefCell<State>, index: i32) {
    let mut s = state.borrow_mut();
    let row = usize::try_from(index).ok().and_then(|i| s.providers.get(i));
    if let Some(row) = row {
        ui.set_provider_summary(
            format!(
                "{}  ·  {}",
                row["base_url"].as_str().unwrap_or_default(),
                if row["has_api_key"] == true {
                    "Key 已保存"
                } else {
                    "无需 Key"
                }
            )
            .into(),
        );
    }
    ui.set_provider_name(
        row.and_then(|r| r["name"].as_str())
            .unwrap_or_default()
            .into(),
    );
    ui.set_provider_url(
        row.and_then(|r| r["base_url"].as_str())
            .unwrap_or_default()
            .into(),
    );
    ui.set_provider_has_key(row.is_some_and(|r| r["has_api_key"] == true));
    ui.set_provider_no_key(row.is_some_and(|r| r["has_api_key"] == false));
    ui.set_provider_key("".into());
    s.provider_revision = row.and_then(|r| r["revision"].as_i64()).unwrap_or(0);
    s.dirty = false;
}
pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    CONTROLLER.with_borrow_mut(|c| {
        *c = Some(Controller {
            ui: ui.as_weak(),
            session,
            state: Rc::new(RefCell::new(State::default())),
        })
    });
    ui.on_provider_select(|| {
        if let Some((ui, _, state)) = active() {
            ui.set_provider_creating(false);
            fill(&ui, &state, ui.get_provider_index());
            ui.set_provider_notice("".into());
        }
    });
    ui.on_provider_new(|| {
        if let Some((ui, _, state)) = active() {
            ui.set_provider_editing(true);
            if !ui.get_provider_creating() {
                ui.set_provider_creating(true);
                fill(&ui, &state, -1);
            }
            ui.set_provider_notice("".into());
        }
    });
    ui.on_provider_edit(|| {
        if let Some((ui, _, state)) = active() {
            ui.set_provider_creating(false);
            fill(&ui, &state, ui.get_provider_index());
            ui.set_provider_editing(true);
            ui.set_provider_notice("".into());
            ui.set_provider_error("".into());
        }
    });
    ui.on_provider_edited(|| {
        if let Some((ui, _, s)) = active() {
            s.borrow_mut().dirty = true;
            ui.set_provider_notice("".into());
            ui.set_provider_error("".into());
        }
    });
    ui.on_organization_edited(|| {
        if let Some((ui, _, s)) = active() {
            s.borrow_mut().org_dirty = true;
            ui.set_organization_dirty(true);
            ui.set_organization_notice("".into());
            ui.set_organization_error("".into());
        }
    });
    ui.on_provider_save(||{
        let Some((ui,_,s))=active()else{return};
        if ui.get_provider_name().trim().is_empty()||ui.get_provider_url().trim().is_empty(){ui.set_provider_error("请填写服务商名称和地址。".into());return}
        // 新增目标不能由下拉框的已有选择推断，否则会把草稿覆盖到已有连接。
        let id=if ui.get_provider_creating(){None}else{
            let id=usize::try_from(ui.get_provider_index()).ok().and_then(|i|s.borrow().providers.get(i).and_then(|r|r["provider_id"].as_str()).map(str::to_owned));
            if id.is_none(){ui.set_provider_error("请选择要编辑的服务商。".into());return}
            id
        };
        let revision=s.borrow().provider_revision;
        launch("save",json!({"provider_id":id,"expected_revision":revision,"name":ui.get_provider_name().trim(),"base_url":ui.get_provider_url().trim(),"api_key":if ui.get_provider_key().is_empty()||ui.get_provider_no_key(){None}else{Some(ui.get_provider_key().to_string())},"clear_api_key":ui.get_provider_no_key()}));
    });
    ui.on_organization_save(||{
        let Some((ui,_,s))=active()else{return};
        let (Ok(batch),Ok(interval),Ok(limit),Ok(hours))=(ui.get_organization_batch().parse::<u32>(),ui.get_organization_interval().parse::<u32>(),ui.get_organization_limit().parse::<u32>(),ui.get_maintenance_hours().parse::<u32>())else{ui.set_organization_error("运行参数须为整数。".into());return};
        if !(1..=10).contains(&batch)||!(10..=86400).contains(&interval)||!(1..=10000).contains(&limit)||!(1..=168).contains(&hours){ui.set_organization_error("每批1～10条，间隔10～86400秒，每日1～10000次，维护周期1～168小时。".into());return}
        let revision=s.borrow().org_revision;
        launch("organization-save",json!({"expected_revision":revision,"enabled":ui.get_organization_enabled(),"extraction_enabled":ui.get_extraction_enabled(),"maintenance_enabled":ui.get_maintenance_enabled(),"maintenance_interval_hours":hours,"batch_size":batch,"interval_seconds":interval,"daily_limit":limit}));
    });
    ui.on_organization_reload(|| {
        if let Some((ui, _, _)) = active() {
            ui.set_organization_notice("".into());
            launch("organization-reload", json!({}));
        }
    });
}
pub(crate) fn clear() {
    cancel();
    if let Some((ui, _, s)) = active() {
        let mut s = s.borrow_mut();
        s.gate.clear();
        s.organization.clear();
        s.loaded = false;
        s.org_loaded = false;
        s.dirty = false;
        s.org_dirty = false;
        s.embedding = None;
        s.text = None;
        s.providers.clear();
        ui.set_provider_key("".into());
        ui.set_provider_editing(false);
        ui.set_provider_creating(false);
        ui.set_provider_index(-1);
        ui.set_provider_summary("".into());
        ui.set_provider_busy(false);
        ui.set_organization_busy(false);
        ui.set_organization_loaded(false);
        ui.set_organization_dirty(false);
        ui.set_organization_saved_enabled(false);
        ui.set_extraction_saved_enabled(false);
        ui.set_maintenance_saved_enabled(false);
        ui.set_organization_enabled(false);
        ui.set_extraction_enabled(false);
        ui.set_maintenance_enabled(false);
        ui.set_maintenance_hours("24".into());
        ui.set_provider_notice("".into());
        ui.set_provider_error("".into());
        ui.set_organization_error("".into());
        ui.set_organization_notice("".into());
    }
}
pub(crate) fn load() {
    if let Some((ui, _, s)) = active() {
        if !s.borrow().loaded && !ui.get_provider_busy() {
            launch("read", json!({}));
        }
        if !s.borrow().org_loaded && !ui.get_organization_busy() {
            reload_organization();
        }
    }
}
fn apply_selections(ui: &Dashboard, s: &State) {
    if let Some(id) = &s.embedding
        && let Some(i) = s
            .providers
            .iter()
            .position(|r| r["provider_id"].as_str() == Some(id.as_str()))
    {
        ui.set_model_provider_index(i as i32);
    }
    if let Some(id) = &s.text
        && let Some(i) = s
            .providers
            .iter()
            .position(|r| r["provider_id"].as_str() == Some(id.as_str()))
    {
        ui.set_text_provider_index(i as i32);
    }
}
pub(crate) fn select_embedding(id: Option<&str>) {
    if let Some((ui, _, s)) = active() {
        s.borrow_mut().embedding = id.map(str::to_owned);
        apply_selections(&ui, &s.borrow());
    }
}
pub(crate) fn select_text(id: Option<&str>) {
    if let Some((ui, _, s)) = active() {
        s.borrow_mut().text = id.map(str::to_owned);
        apply_selections(&ui, &s.borrow());
    }
}
pub(crate) fn selected_embedding() -> Option<String> {
    let (ui, _, s) = active()?;
    let index = usize::try_from(ui.get_model_provider_index()).ok()?;
    s.borrow().providers.get(index)?["provider_id"]
        .as_str()
        .map(str::to_owned)
}
pub(crate) fn selected_text() -> Option<String> {
    let (ui, _, s) = active()?;
    let index = usize::try_from(ui.get_text_provider_index()).ok()?;
    s.borrow().providers.get(index)?["provider_id"]
        .as_str()
        .map(str::to_owned)
}
#[wasm_bindgen]
pub fn receive_provider(operation: String, generation: u32, status: u16, body: String) {
    let Some((ui, _, s)) = active() else { return };
    if !ui.get_unlocked() {
        return;
    }
    let organization = operation.starts_with("organization");
    let accepted = if organization {
        s.borrow_mut().organization.accept(generation)
    } else {
        s.borrow_mut().gate.accept(generation)
    };
    if accepted.as_deref() != Some(operation.as_str()) {
        return;
    }
    if organization {
        ui.set_organization_busy(false);
    } else {
        ui.set_provider_busy(false);
    }
    if status == 401 {
        crate::browser::end_session();
        return;
    }
    let value = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    if status != 200 {
        let message = value["message"]
            .as_str()
            .unwrap_or("配置请求失败，请重试。");
        if organization {
            ui.set_organization_error(message.into());
        } else {
            ui.set_provider_error(message.into());
        }
        return;
    }
    if organization {
        let mut state = s.borrow_mut();
        state.org_loaded = true;
        ui.set_organization_loaded(true);
        ui.set_organization_saved_enabled(value["enabled"] == true);
        ui.set_extraction_saved_enabled(value["extraction_enabled"] == true);
        ui.set_maintenance_saved_enabled(value["maintenance_enabled"] == true);
        // 进度变化不改设置版本；另一处保存的选择不能成为旧草稿的新写入基线。
        if operation == "organization-read"
            && state.org_dirty
            && state.org_revision != value["revision"].as_i64().unwrap_or(0)
        {
            ui.set_organization_error(
                "任务设置已在其他位置变更，草稿已保留；重新载入会放弃当前草稿。".into(),
            );
            return;
        }
        state.org_revision = value["revision"].as_i64().unwrap_or(0);
        if matches!(
            operation.as_str(),
            "organization-save" | "organization-reload"
        ) || !state.org_dirty
        {
            ui.set_organization_enabled(value["enabled"] == true);
            ui.set_extraction_enabled(value["extraction_enabled"] == true);
            ui.set_maintenance_enabled(value["maintenance_enabled"] == true);
            ui.set_maintenance_hours(
                value["maintenance_interval_hours"]
                    .as_u64()
                    .unwrap_or(24)
                    .to_string()
                    .into(),
            );
            ui.set_organization_batch(
                value["batch_size"]
                    .as_u64()
                    .unwrap_or(10)
                    .to_string()
                    .into(),
            );
            ui.set_organization_interval(
                value["interval_seconds"]
                    .as_u64()
                    .unwrap_or(60)
                    .to_string()
                    .into(),
            );
            ui.set_organization_limit(
                value["daily_limit"]
                    .as_u64()
                    .unwrap_or(100)
                    .to_string()
                    .into(),
            );
            state.org_dirty = false;
            ui.set_organization_dirty(false);
        }
        if operation == "organization-reload" {
            ui.set_organization_notice("已载入最新任务设置。".into());
        }
        if operation == "organization-save" {
            ui.set_organization_notice(
                if value["enabled"] != true
                    && value["extraction_enabled"] != true
                    && value["maintenance_enabled"] != true
                {
                    "自动任务已关闭，手动提炼和现有记忆继续可用。"
                } else if !ui.get_text_model_enabled() {
                    "任务选择已保存，等待文本模型可调用后自动运行。"
                } else {
                    "任务设置已保存，后台将按所选任务自动处理。"
                }
                .into(),
            );
        }
    } else {
        let mut state = s.borrow_mut();
        if state.loaded {
            state.embedding = usize::try_from(ui.get_model_provider_index())
                .ok()
                .and_then(|i| state.providers.get(i))
                .and_then(|r| r["provider_id"].as_str())
                .map(str::to_owned);
            state.text = usize::try_from(ui.get_text_provider_index())
                .ok()
                .and_then(|i| state.providers.get(i))
                .and_then(|r| r["provider_id"].as_str())
                .map(str::to_owned);
        }
        state.providers = value["providers"].as_array().cloned().unwrap_or_default();
        state.loaded = true;
        ui.set_provider_names(
            Rc::new(VecModel::from(
                state
                    .providers
                    .iter()
                    .map(|r| slint::SharedString::from(r["name"].as_str().unwrap_or("模型服务")))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        if operation == "save" && ui.get_provider_creating() {
            ui.set_provider_index(state.providers.len().saturating_sub(1) as i32);
        } else if ui.get_provider_index() < 0 && !state.providers.is_empty() {
            ui.set_provider_index(0);
        }
        apply_selections(&ui, &state);
        drop(state);
        if operation == "save" || !s.borrow().dirty {
            ui.set_provider_creating(s.borrow().providers.is_empty());
            fill(&ui, &s, ui.get_provider_index());
        }
        if operation == "save" {
            ui.set_provider_editing(false);
            ui.set_provider_notice("已保存，引用此服务商的模型共用该连接。".into());
            crate::model_browser::load();
            crate::text_browser::reload_settings();
        }
    }
}

pub(crate) fn reload_organization() {
    launch("organization-read", json!({}));
}
