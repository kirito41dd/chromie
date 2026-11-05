#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::{
    Dashboard, MemoryRow,
    session::{Operation, Session},
    space_key::SpaceKey,
};
use serde::Deserialize;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = chromieRequest)]
    fn send_request(
        space: &str,
        operation: &str,
        password: &str,
        generation: u32,
        remember: bool,
        order: &str,
    );
    #[wasm_bindgen(js_namespace = window, js_name = chromieSavedPassword)]
    fn saved_password(space: &str) -> String;
    #[wasm_bindgen(js_namespace = window, js_name = chromieForgetPassword)]
    fn forget_password() -> bool;
    #[wasm_bindgen(js_namespace = window, js_name = chromieNavigate)]
    fn navigate_browser(
        space: &str,
        recall_page: bool,
        replace: bool,
        access_page: bool,
        messages_page: bool,
        settings_page: bool,
        transfer_page: bool,
    );
    #[wasm_bindgen(js_namespace = window, js_name = chromieCancel)]
    fn cancel();
    #[wasm_bindgen(js_namespace = window, js_name = chromieReady)]
    fn ready();
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    can_manage_system: bool,
    #[serde(default)]
    embedding: serde_json::Value,
    #[serde(default)]
    order: String,
    space_id: String,
    space_name: String,
    updated_at: String,
    memory_count: u64,
    #[serde(default)]
    secret_count: Option<u64>,
    event_count: u64,
    last_capture_at: Option<String>,
    pending_count: u64,
    retrying_count: u64,
    last_index_at: Option<String>,
    index_available: bool,
    memories: Vec<RecentMemory>,
}

#[derive(Deserialize)]
struct RecentMemory {
    #[serde(default)]
    usage: Option<crate::memory::Usage>,
    #[serde(default)]
    origin: crate::memory::Origin,
    #[serde(default)]
    memory_id: String,
    #[serde(flatten)]
    secret: crate::memory::SecretMetadata,
    revision: i32,
    preview: String,
    // 兼容旧服务的短预览；新版看板同时返回当前修订的完整公开正文。
    #[serde(default)]
    content: Option<String>,
    project_key: Option<String>,
    updated_at: String,
    source_count: u64,
    index_pending: bool,
}

struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    _timer: Timer,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}

pub(crate) fn date(value: &str) -> String {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|time| {
            let format =
                time::format_description::parse_borrowed::<3>("[month]-[day] [hour]:[minute] UTC")
                    .ok()?;
            time.format(&format).ok()
        })
        .unwrap_or_else(|| "时间未知".into())
}

fn clear(ui: &Dashboard) {
    ui.set_fresh(false);
    ui.set_memories(ModelRc::new(VecModel::<MemoryRow>::default()));
    ui.set_memory_count("—".into());
    ui.set_secret_summary("当前有效记忆".into());
    ui.set_event_count("—".into());
    ui.set_pending_count("—".into());
    ui.set_capture_note("等待更新".into());
    ui.set_index_note("等待更新".into());
    ui.set_index_state("检索状态待更新".into());
    ui.set_index_ok(false);
    ui.set_update_label(SharedString::default());
}

fn request(ui: &Dashboard, session: &RefCell<Session>) {
    // 先结束可变借用再进入 JS；同步失败的回调也可安全回到 Rust。
    let next = session.borrow_mut().request();
    if let Some(next) = next {
        ui.set_loading(true);
        send_request(
            &next.space,
            next.operation.as_str(),
            &next.password,
            next.generation,
            next.operation == Operation::Login && ui.get_remember_password(),
            crate::memory::order(ui.get_memory_order()),
        );
    }
}

fn prefill_password(ui: &Dashboard) {
    let password = SpaceKey::parse(ui.get_space_id().trim())
        .map(|space| saved_password(space.as_str()))
        .unwrap_or_default();
    ui.set_remember_password(!password.is_empty());
    ui.set_password(password.into());
}

#[wasm_bindgen]
pub fn start(
    initial_space: String,
    recall_page: bool,
    access_page: bool,
    messages_page: bool,
    settings_page: bool,
    transfer_page: bool,
) -> Result<(), JsValue> {
    let ui = Dashboard::new().map_err(|_| JsValue::from_str("无法创建看板界面"))?;
    ui.set_space_id(initial_space.into());
    ui.set_recall_page(recall_page);
    ui.set_access_page(access_page);
    ui.set_messages_page(messages_page);
    ui.set_settings_page(settings_page);
    ui.set_transfer_page(transfer_page);
    let session = Rc::new(RefCell::new(Session::default()));
    crate::selection::install(&ui);
    crate::model_browser::install(&ui, session.clone());
    crate::transfer_browser::install(&ui, session.clone());
    crate::access_browser::install(&ui, session.clone());
    crate::messages_browser::install(&ui, session.clone());
    crate::recall_browser::install(&ui, session.clone());
    crate::secret_browser::install(&ui, session.clone());
    ui.on_show_dashboard(|| navigate(false, false, false, false, false));
    ui.on_show_recall(|| navigate(true, false, false, false, false));
    ui.on_show_access(|| navigate(false, true, false, false, false));
    ui.on_show_messages(|| navigate(false, false, true, false, false));
    ui.on_show_transfer(|| navigate(false, false, false, false, true));
    ui.on_show_settings(|| navigate(false, false, false, true, false));
    let weak = ui.as_weak();
    ui.on_space_edited(move || {
        if let Some(ui) = weak.upgrade() {
            prefill_password(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_remember_changed(move || {
        if let Some(ui) = weak.upgrade()
            && !ui.get_remember_password()
            && !forget_password()
        {
            ui.set_remember_password(true);
            ui.set_message("无法清除已保存的密码，请检查浏览器站点存储权限。".into());
        }
    });
    let weak = ui.as_weak();
    let state = session.clone();
    ui.on_unlock(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_loading() {
            return;
        }
        let Some(space) = SpaceKey::parse(ui.get_space_id().trim()) else {
            ui.set_message("空间标识使用小写英文和连字符，例如 work-notes。".into());
            return;
        };
        let password = ui.get_password().to_string();
        if password.is_empty() || password.len() > 1024 {
            ui.set_message("请输入有效的空间密码。".into());
            return;
        }
        ui.set_space_id(space.as_str().into());
        state
            .borrow_mut()
            .unlock(space.as_str().to_owned(), password);
        ui.set_password(SharedString::default());
        ui.set_message(SharedString::default());
        request(&ui, &state);
    });
    let weak = ui.as_weak();
    let state = session.clone();
    ui.on_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            request(&ui, &state);
        }
    });
    let weak = ui.as_weak();
    let state = session.clone();
    ui.on_change_memory_order(move |index| {
        if let Some(ui) = weak.upgrade()
            && ui.get_unlocked()
            && !ui.get_loading()
            && (0..=2).contains(&index)
            && index != ui.get_memory_order()
        {
            ui.set_memory_order(index);
            crate::selection::clear();
            ui.set_memories(ModelRc::new(VecModel::<MemoryRow>::default()));
            request(&ui, &state);
        }
    });
    ui.on_lock(|| {
        crate::secret_browser::close();
        let next = CONTROLLER.with_borrow(|current| {
            current
                .as_ref()
                .and_then(|c| c.session.borrow_mut().logout())
        });
        if let Some(next) = next {
            CONTROLLER.with_borrow(|current| {
                if let Some(ui) = current.as_ref().and_then(|c| c.ui.upgrade()) {
                    ui.set_loading(true);
                }
            });
            send_request(
                &next.space,
                next.operation.as_str(),
                &next.password,
                next.generation,
                false,
                "recent",
            );
        }
    });
    let timer = Timer::default();
    let weak = ui.as_weak();
    let state = session.clone();
    timer.start(TimerMode::Repeated, Duration::from_secs(15), move || {
        if let Some(ui) = weak.upgrade()
            && !ui.get_recall_page()
            && !ui.get_access_page()
            && !ui.get_messages_page()
            && !ui.get_settings_page()
            && !ui.get_transfer_page()
        {
            request(&ui, &state);
        }
    });
    CONTROLLER.with_borrow_mut(|current| {
        *current = Some(Controller {
            ui: ui.as_weak(),
            session,
            _timer: timer,
        });
    });
    ui.show()
        .map_err(|_| JsValue::from_str("无法显示看板界面"))?;
    // Winit 在 Web 上通过特定 JS 异常交接事件循环，先通知页面初始化已成功。
    ready();
    restore_session();
    slint::run_event_loop().map_err(|_| JsValue::from_str("无法启动看板界面"))
}

fn navigate(
    recall_page: bool,
    access_page: bool,
    messages_page: bool,
    settings_page: bool,
    transfer_page: bool,
) {
    // 地址切换会清理旧请求；先完成导航，再加载目标页，避免新列表请求被误取消。
    CONTROLLER.with_borrow(|current| {
        if let Some(controller) = current
            && let Some(ui) = controller.ui.upgrade()
            && ui.get_unlocked()
        {
            navigate_browser(
                &controller.session.borrow().space,
                recall_page,
                false,
                access_page,
                messages_page,
                settings_page,
                transfer_page,
            );
        }
    });
    set_pages(
        recall_page,
        access_page,
        messages_page,
        settings_page,
        transfer_page,
    );
}

#[wasm_bindgen]
pub fn set_page(recall_page: bool) {
    set_pages(recall_page, false, false, false, false);
}

#[wasm_bindgen]
pub fn set_access_page(access_page: bool) {
    set_pages(false, access_page, false, false, false);
}

#[wasm_bindgen]
pub fn set_messages_page(messages_page: bool) {
    set_pages(false, false, messages_page, false, false);
}

#[wasm_bindgen]
pub fn set_settings_page(settings_page: bool) {
    set_pages(false, false, false, settings_page, false);
}

#[wasm_bindgen]
pub fn set_transfer_page(transfer_page: bool) {
    set_pages(false, false, false, false, transfer_page);
}
fn set_pages(
    recall_page: bool,
    access_page: bool,
    messages_page: bool,
    settings_page: bool,
    transfer_page: bool,
) {
    let active = CONTROLLER
        .with_borrow(|current| current.as_ref().map(|c| (c.ui.clone(), c.session.clone())));
    let Some((weak, session)) = active else {
        return;
    };
    let Some(ui) = weak.upgrade() else {
        return;
    };
    if ui.get_recall_page() == recall_page
        && ui.get_access_page() == access_page
        && ui.get_messages_page() == messages_page
        && ui.get_settings_page() == settings_page
        && ui.get_transfer_page() == transfer_page
    {
        if access_page {
            crate::access_browser::route_changed();
        }
        return;
    }
    crate::secret_browser::close();
    crate::model_browser::clear();
    crate::transfer_browser::clear();
    crate::recall_browser::clear();
    crate::access_browser::clear();
    crate::messages_browser::clear();
    ui.set_recall_query(SharedString::default());
    ui.set_debug_open(false);
    ui.set_recall_page(recall_page);
    ui.set_access_page(access_page);
    ui.set_messages_page(messages_page);
    ui.set_settings_page(settings_page);
    ui.set_transfer_page(transfer_page);
    ui.invoke_reset_scroll();
    if access_page && ui.get_unlocked() {
        crate::access_browser::load();
    }
    if messages_page && ui.get_unlocked() {
        crate::messages_browser::load();
    }
    if settings_page && ui.get_unlocked() {
        crate::model_browser::load();
    }
    if transfer_page && ui.get_unlocked() {
        crate::transfer_browser::load();
    }
    if !recall_page
        && !access_page
        && !messages_page
        && !settings_page
        && !transfer_page
        && ui.get_unlocked()
    {
        request(&ui, &session);
    }
}

#[wasm_bindgen]
pub fn restore_session() {
    let active = CONTROLLER
        .with_borrow(|current| current.as_ref().map(|c| (c.ui.clone(), c.session.clone())));
    let Some((weak, state)) = active else {
        return;
    };
    let Some(ui) = weak.upgrade() else {
        return;
    };
    if ui.get_unlocked() || ui.get_loading() {
        return;
    }
    prefill_password(&ui);
    if let Some(space) = SpaceKey::parse(ui.get_space_id().trim()) {
        ui.set_space_id(space.as_str().into());
        state.borrow_mut().restore(space.as_str().to_owned());
        request(&ui, &state);
    }
}

#[wasm_bindgen]
pub fn resize_viewport(width: f32) {
    if !width.is_finite() || width <= 0.0 {
        return;
    }
    CONTROLLER.with_borrow(|current| {
        if let Some(controller) = current
            && let Some(ui) = controller.ui.upgrade()
        {
            ui.set_viewport_width(width);
        }
    });
}

#[wasm_bindgen]
pub fn end_session() {
    crate::model_browser::clear();
    crate::transfer_browser::clear();
    crate::secret_browser::close();
    crate::recall_browser::clear();
    crate::access_browser::clear();
    crate::messages_browser::clear();
    CONTROLLER.with_borrow(|current| {
        let Some(controller) = current else {
            return;
        };
        controller.session.borrow_mut().lock();
        if let Some(ui) = controller.ui.upgrade() {
            clear(&ui);
            ui.set_unlocked(false);
            ui.set_can_manage_system(false);
            ui.set_memory_order(0);
            ui.set_loading(false);
            ui.set_space_name(SharedString::default());
            ui.set_password(SharedString::default());
            ui.set_message(SharedString::default());
            ui.set_recall_query(SharedString::default());
            ui.set_debug_open(false);
            ui.invoke_reset_scroll();
        }
    });
    cancel();
}

#[wasm_bindgen]
pub fn receive_snapshot(generation: u32, status: u16, body: String, storage_failed: bool) {
    // JS 可能在 fetch 失败或取消时同步回调，不在外层持有 Controller 借用。
    let active = CONTROLLER.with_borrow(|current| {
        current
            .as_ref()
            .map(|controller| (controller.ui.clone(), controller.session.clone()))
    });
    let Some((weak, state)) = active else {
        return;
    };
    let Some(operation) = state.borrow_mut().finish(generation) else {
        return;
    };
    let Some(ui) = weak.upgrade() else {
        return;
    };
    ui.set_loading(false);
    if operation == Operation::Logout {
        if status == 204 {
            end_session();
            prefill_password(&ui);
        } else {
            ui.set_fresh(false);
            ui.set_message("退出未完成，请连接服务后重试。".into());
        }
        return;
    }
    if status == 401 || status == 404 {
        let restoring = state.borrow().restoring;
        end_session();
        if restoring && status == 401 {
            prefill_password(&ui);
            return;
        }
        ui.set_message(
            if status == 401 {
                "密码不正确或已变更，请重新输入。"
            } else {
                "空间不存在，请检查地址。"
            }
            .into(),
        );
        return;
    }
    let snapshot = if status == 200 {
        serde_json::from_str::<Snapshot>(&body).ok()
    } else {
        None
    };
    let Some(snapshot) = snapshot else {
        if operation == Operation::Login {
            end_session();
        }
        clear(&ui);
        ui.set_message(
            if operation == Operation::Login {
                "暂时无法读取空间数据，请重试登录。"
            } else {
                "暂时无法读取空间数据，正在自动重试。"
            }
            .into(),
        );
        return;
    };
    if snapshot.space_id != state.borrow().space {
        end_session();
        ui.set_message("空间校验失败，请重新打开看板。".into());
        return;
    }
    let expected_order = crate::memory::order(ui.get_memory_order());
    if snapshot.order != expected_order
        && !(snapshot.order.is_empty() && expected_order == "recent")
    {
        clear(&ui);
        ui.set_message("收到的记忆视图与当前选择不一致，请重试。".into());
        return;
    }
    state.borrow_mut().authenticated();
    ui.set_space_id(snapshot.space_id.into());
    navigate_browser(
        &state.borrow().space,
        ui.get_recall_page(),
        true,
        ui.get_access_page(),
        ui.get_messages_page(),
        ui.get_settings_page(),
        ui.get_transfer_page(),
    );
    let first_snapshot = !ui.get_unlocked();
    ui.set_unlocked(true);
    ui.set_can_manage_system(snapshot.can_manage_system);
    if first_snapshot {
        if ui.get_messages_page() {
            crate::messages_browser::load();
        }
        if ui.get_access_page() {
            crate::access_browser::load();
        }
        if ui.get_settings_page() {
            crate::model_browser::load();
        }
        if ui.get_transfer_page() {
            crate::transfer_browser::load();
        }
        ui.invoke_reset_scroll();
    }
    ui.set_fresh(true);
    ui.set_message(
        if storage_failed {
            "已登录，但无法更新浏览器保存的密码。"
        } else {
            ""
        }
        .into(),
    );
    ui.set_space_name(snapshot.space_name.into());
    ui.set_memory_count(snapshot.memory_count.to_string().into());
    ui.set_secret_summary(crate::memory::secret_summary(snapshot.secret_count).into());
    ui.set_event_count(snapshot.event_count.to_string().into());
    ui.set_pending_count(snapshot.pending_count.to_string().into());
    ui.set_capture_note(
        snapshot
            .last_capture_at
            .map(|value| format!("最近收到 · {}", date(&value)))
            .unwrap_or_else(|| "尚未收到采集消息".into())
            .into(),
    );
    ui.set_index_note(
        if snapshot.retrying_count > 0 {
            format!("{} 项正在重试", snapshot.retrying_count)
        } else {
            snapshot
                .last_index_at
                .map(|value| format!("最近同步 · {}", date(&value)))
                .unwrap_or_else(|| "尚无同步记录".into())
        }
        .into(),
    );
    ui.set_index_ok(snapshot.index_available);
    ui.set_index_state(
        if snapshot.index_available {
            "检索可用"
        } else {
            "检索暂不可用，记忆仍已保存"
        }
        .into(),
    );
    let semantic = match snapshot
        .embedding
        .get("state")
        .and_then(serde_json::Value::as_str)
    {
        Some("ready") => "语义索引已就绪",
        Some("building") => "正在准备语义检索",
        Some("updating") => "语义索引更新中",
        Some("unavailable") => "语义服务存在异常，关键词检索可用",
        _ => "关键词检索 · 未启用语义模型",
    };
    let coverage = &snapshot.embedding["progress"];
    let semantic_note = if let (Some(ready), Some(total)) =
        (coverage["ready"].as_u64(), coverage["total"].as_u64())
    {
        format!("{semantic} · {ready}/{total} 条")
    } else {
        semantic.to_owned()
    };
    ui.set_index_note(format!("{}\n{semantic_note}", ui.get_index_note()).into());
    ui.set_update_label(format!("最后更新 · {}", date(&snapshot.updated_at)).into());
    let rows = snapshot
        .memories
        .into_iter()
        .map(|memory| {
            let content = memory.content.unwrap_or_else(|| memory.preview.clone());
            let preview = if content.len() > memory.preview.len() {
                format!("{}…", memory.preview)
            } else {
                memory.preview
            };
            MemoryRow {
                agent_name: memory.origin.agent_name.clone().into(),
                usage_detail: memory
                    .usage
                    .as_ref()
                    .map(|usage| usage.detail())
                    .unwrap_or_default()
                    .into(),
                usage: memory
                    .usage
                    .as_ref()
                    .map(|usage| usage.label())
                    .unwrap_or_else(|| "统计暂不可用".into())
                    .into(),
                memory_id: memory.memory_id.into(),
                is_secret: memory.secret.is_secret(),
                secret_stored: memory.secret.stored(),
                secret_status: memory.secret.status().into(),
                content: content.into(),
                preview: preview.into(),
                scope: memory
                    .project_key
                    .map(|project| format!("项目 · {project}"))
                    .unwrap_or_else(|| "空间通用".into())
                    .into(),
                time: date(&memory.updated_at).into(),
                detail: format!(
                    "{}  ·  修订 {}  ·  {}",
                    memory.origin.agent_name,
                    memory.revision,
                    if memory.source_count == 0 {
                        "未关联来源".into()
                    } else {
                        format!("关联 {} 条来源", memory.source_count)
                    }
                )
                .into(),
                pending: memory.index_pending,
            }
        })
        .collect::<Vec<_>>();
    if let Some(model) = crate::memory::reconcile_rows(&ui.get_memories(), rows) {
        ui.set_memories(model);
    }
}
