#![cfg_attr(not(test), forbid(unsafe_code))]

use crate::{
    Dashboard, RecallOverview, RecallRow, RecallStage,
    recall::{RecallState, Request, Results, debug_request, debug_response},
    session::Session,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = chromieRecall)]
    fn send_recall(space: &str, query: &str, generation: u32);
    #[wasm_bindgen(js_namespace = window, js_name = chromieCancelRecall)]
    fn cancel_recall();
}

struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<RecallState>>,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}

type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<RecallState>>);

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
            state: Rc::new(RefCell::new(RecallState::default())),
        });
    });
    ui.on_recall_edited(clear);
    ui.on_clear_recall(|| {
        clear();
        if let Some((ui, _, _)) = active() {
            ui.set_recall_query(SharedString::default());
        }
    });
    ui.on_recall(|| {
        let Some((ui, session, state)) = active() else {
            return;
        };
        if !ui.get_unlocked() || !ui.get_recall_page() {
            return;
        }
        let next = state.borrow_mut().submit(&ui.get_recall_query());
        if let Some(next) = next {
            ui.set_recall_query(next.query.clone().into());
            ui.set_recall_results(ModelRc::new(VecModel::<RecallRow>::default()));
            ui.set_recall_note(SharedString::default());
            launch(&ui, &session, next);
        } else {
            clear();
            ui.set_recall_done(true);
            ui.set_recall_error("请输入关键词，最多 256 字节。".into());
        }
    });
}

fn launch(ui: &Dashboard, session: &RefCell<Session>, next: Request) {
    let space = session.borrow().space.clone();
    ui.set_debug_open(false);
    ui.set_recall_overview(RecallOverview::default());
    ui.set_recall_stages(ModelRc::new(VecModel::<RecallStage>::default()));
    ui.set_recall_done(true);
    ui.set_recall_loading(true);
    ui.set_recall_error(SharedString::default());
    ui.set_debug_request(debug_request(&space, &next.query).into());
    ui.set_debug_response(SharedString::default());
    ui.set_debug_meta("请求已发送，等待响应。".into());
    send_recall(&space, &next.query, next.generation);
}

pub(crate) fn clear() {
    crate::secret_browser::close();
    if let Some((ui, _, state)) = active() {
        state.borrow_mut().clear();
        ui.set_debug_open(false);
        ui.set_recall_overview(RecallOverview::default());
        ui.set_recall_stages(ModelRc::new(VecModel::<RecallStage>::default()));
        ui.set_recall_done(false);
        ui.set_recall_loading(false);
        ui.set_recall_results(ModelRc::new(VecModel::<RecallRow>::default()));
        ui.set_recall_error(SharedString::default());
        ui.set_recall_note(SharedString::default());
        ui.set_debug_request(SharedString::default());
        ui.set_debug_response(SharedString::default());
        ui.set_debug_meta(SharedString::default());
    }
    cancel_recall();
}

#[wasm_bindgen]
pub fn receive_recall(
    generation: u32,
    status: u16,
    body: String,
    elapsed_ms: u32,
    requested_at: String,
) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !state.borrow_mut().finish(generation) || !ui.get_unlocked() || !ui.get_recall_page() {
        return;
    }
    ui.set_recall_loading(false);
    let status_label = if status == 0 {
        "未收到 HTTP 响应".into()
    } else {
        format!("HTTP {status}")
    };
    let meta = format!(
        "{status_label} · 请求往返耗时 {elapsed_ms} ms\n浏览器请求时间（UTC）：{requested_at}"
    );
    ui.set_debug_meta(meta.clone().into());
    ui.set_debug_response(debug_response(&body).into());
    if status == 401 || status == 404 {
        crate::browser::end_session();
        ui.set_message("登录已失效，请重新登录空间。".into());
        return;
    }
    let result = (status == 200).then(|| Results::parse(&body)).flatten();
    let Some(result) = result else {
        ui.set_recall_results(ModelRc::new(VecModel::<RecallRow>::default()));
        ui.set_recall_note(SharedString::default());
        ui.set_recall_error(
            if status == 400 {
                "关键词无效，请调整后重试。"
            } else {
                "召回暂时失败，请稍后重试。"
            }
            .into(),
        );
        return;
    };
    ui.set_recall_note(result.note().into());
    let mut overview = result
        .debug
        .as_ref()
        .map(|debug| debug.overview())
        .unwrap_or_else(|| RecallOverview {
            total_time: "未提供".into(),
            ..Default::default()
        });
    overview.client_time = format!("{elapsed_ms} ms").into();
    overview.index_available = result.index_available;
    overview.limited = !result.index_available || result.partial || result.truncated;
    ui.set_recall_overview(overview);
    ui.set_recall_stages(ModelRc::new(VecModel::from(
        result
            .debug
            .as_ref()
            .map(|debug| debug.stages())
            .unwrap_or_default(),
    )));
    ui.set_recall_error(SharedString::default());
    ui.set_recall_results(ModelRc::new(VecModel::from(
        result
            .memories
            .iter()
            .map(|card| {
                let (path, score) = result
                    .debug
                    .as_ref()
                    .map(|debug| debug.hit_labels(&card.memory_id))
                    .unwrap_or_else(|| ("服务端未提供命中路径。".into(), "未提供分数。".into()));
                let scope = card
                    .project_key
                    .as_ref()
                    .map(|project| format!("项目 · {project}"))
                    .unwrap_or_else(|| "空间通用".into());
                let usage = result
                    .debug
                    .as_ref()
                    .and_then(|debug| debug.usage(&card.memory_id));
                RecallRow {
                    agent_name: card.origin.agent_name.clone().into(),
                    pending: card.processing_state != "index_ready",
                    usage_detail: usage.map(|usage| usage.detail()).unwrap_or_default().into(),
                    usage: usage
                        .map(|usage| usage.label())
                        .unwrap_or_else(|| "统计暂不可用".into())
                        .into(),
                    web_usage: usage
                        .map(|usage| format!("Web 调试召回 {} 次", usage.web_recall_count))
                        .unwrap_or_default()
                        .into(),
                    is_secret: card.secret.is_secret(),
                    secret_stored: card.secret.stored(),
                    secret_status: card.secret.status().into(),
                    secret_ref: card.secret.reference().into(),
                    view_key: format!("{generation}:{}", card.memory_id).into(),
                    memory_id: card.memory_id.clone().into(),
                    path: path.into(),
                    score: score.into(),
                    content: card.content.clone().into(),
                    scope: scope.into(),
                    time: crate::browser::date(&card.updated_at).into(),
                    detail: format!(
                        "{} · 修订 {} · {} 条来源 · {}",
                        card.origin.agent_name,
                        card.revision,
                        card.source_refs.len(),
                        if card.processing_state == "index_ready" {
                            "已同步"
                        } else {
                            "待同步"
                        }
                    )
                    .into(),
                }
            })
            .collect::<Vec<_>>(),
    )));
}
