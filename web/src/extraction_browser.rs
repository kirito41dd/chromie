#![forbid(unsafe_code)]
use crate::{Dashboard, ExtractionDecision, ExtractionJob, messages::Gate, session::Session};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window, js_name=chromieTextModel)]
    fn send(space: &str, operation: &str, payload: &str, generation: u32);
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    gate: Gate,
    job: String,
}
thread_local! { static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) }; }
pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    CONTROLLER.with_borrow_mut(|c| {
        *c = Some(Controller {
            ui: ui.as_weak(),
            session,
            gate: Gate::default(),
            job: String::new(),
        })
    });
    ui.on_extraction_read(|id| {
        clear();
        CONTROLLER.with_borrow_mut(|c| {
            if let Some(c) = c {
                c.job = id.to_string();
                if let Some(ui) = c.ui.upgrade() {
                    ui.set_extraction_open(true);
                    ui.invoke_extraction_focus_close();
                }
            }
        });
        launch("review", None);
    });
    ui.on_extraction_close(clear);
    ui.on_extraction_refresh(|| launch("review", None));
    ui.on_extraction_apply(|index| launch("apply-revision", Some(index)));
}
pub(crate) fn clear() {
    CONTROLLER.with_borrow_mut(|c| {
        if let Some(c) = c {
            c.gate.clear();
            c.job.clear();
            if let Some(ui) = c.ui.upgrade() {
                ui.set_extraction_open(false);
                ui.set_extraction_busy(false);
                ui.set_extraction_decisions(ModelRc::default());
                ui.set_extraction_note("".into());
                ui.set_extraction_error("".into());
            }
        }
    });
}
fn launch(operation: &str, index: Option<i32>) {
    CONTROLLER.with_borrow_mut(|c| {
        if let Some(c) = c {
            let Some(ui) = c.ui.upgrade() else {
                return;
            };
            if !ui.get_unlocked()
                || !ui.get_extraction_open()
                || ui.get_extraction_busy()
                || c.job.is_empty()
            {
                return;
            }
            ui.set_extraction_busy(true);
            ui.set_extraction_error("".into());
            let payload = if let Some(index) = index {
                json!({"job_id":c.job,"candidate_index":index})
            } else {
                json!({"job_id":c.job})
            };
            send(
                &c.session.borrow().space,
                operation,
                &payload.to_string(),
                c.gate.begin(operation),
            );
        }
    });
}
pub(crate) fn jobs(ui: &Dashboard, rows: &[Value]) {
    let mut sorted = rows.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|j| j["result"]["pending_revisions"].as_u64().unwrap_or(0) == 0);
    let items = sorted
        .into_iter()
        .filter(|j| j["kind"] == "extract" && j["result"]["has_review"] == true)
        .take(20)
        .map(|j| {
            let counts = &j["result"]["counts"];
            ExtractionJob {
                id: j["job_id"].as_str().unwrap_or_default().into(),
                time: format_time(j["created_at"].as_str().unwrap_or_default()).into(),
                label: format!(
                    "新增 {} · 待修订 {} · 已采用 {} · 跳过 {} · 暂缓 {}",
                    counts["create"],
                    j["result"]["pending_revisions"].as_u64().unwrap_or(0),
                    counts["applied"].as_u64().unwrap_or(0),
                    counts["skip"],
                    counts["defer"]
                )
                .into(),
            }
        })
        .collect::<Vec<_>>();
    ui.set_extraction_jobs(Rc::new(VecModel::from(items)).into());
}
pub(crate) fn receive(operation: &str, generation: u32, status: u16, body: &str) {
    let mut refresh = false;
    let mut expired = false;
    CONTROLLER.with_borrow_mut(|c| {
        if let Some(c) = c {
            let Some(ui) = c.ui.upgrade() else {
                return;
            };
            if !ui.get_unlocked()
                || !ui.get_extraction_open()
                || c.gate.accept(generation).as_deref() != Some(operation)
            {
                return;
            }
            ui.set_extraction_busy(false);
            let value = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
            if status == 401 {
                expired = true;
                return;
            }
            if status != 200 {
                ui.set_extraction_error(
                    value["message"]
                        .as_str()
                        .unwrap_or("请求失败，请刷新查看；旧修订变化时需重新提炼。")
                        .into(),
                );
                return;
            }
            if operation == "apply-revision" {
                refresh = true;
                return;
            }
            let result = &value["result"];
            ui.set_extraction_note(
                if value["state"] == "done" {
                    if result["retrieval_partial"] == true {
                        "已完成。相关记忆检索范围有限；修订确认时仍会检查当前版本。"
                    } else {
                        "已完成。修订仅在确认后写入，保留历史版本。"
                    }
                } else {
                    "尚未完成核对；请稍后刷新。"
                }
                .into(),
            );
            let rows = result["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| {
                    let action = match d["action"].as_str().unwrap_or_default() {
                        "create" => "已新增",
                        "propose_revision" => "建议修订",
                        "skip" => "已跳过",
                        _ => "暂缓",
                    };
                    let evidence = d
                        .get("evidence_details")
                        .unwrap_or(&d["evidence"])
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|e| {
                            format!(
                                "{}\n{}\n{}",
                                match e["role"].as_str() {
                                    Some("user") => format!(
                                        "用户 · {}",
                                        e["occurred_at"].as_str().unwrap_or_default()
                                    ),
                                    Some("assistant") => format!(
                                        "助手 · {}",
                                        e["occurred_at"].as_str().unwrap_or_default()
                                    ),
                                    _ => "来源消息".to_owned(),
                                },
                                e["quote"].as_str().unwrap_or_default(),
                                e["event_id"].as_str().unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    ExtractionDecision {
                        index: d["candidate_index"].as_i64().unwrap_or(0) as i32,
                        action: action.into(),
                        reason: d["reason"].as_str().unwrap_or_default().into(),
                        body: d["content"].as_str().unwrap_or_default().into(),
                        evidence: evidence.into(),
                        previous: d["previous_content"].as_str().unwrap_or_default().into(),
                        proposed: d["replacement"]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                        can_apply: d["action"] == "propose_revision" && d["applied"] != true,
                        applied: d["applied"] == true,
                    }
                })
                .collect::<Vec<_>>();
            ui.set_extraction_decisions(Rc::new(VecModel::from(rows)).into());
        }
    });
    if expired {
        crate::browser::end_session();
    }
    if refresh {
        launch("review", None);
        crate::knowledge_browser::load(true);
        crate::text_browser::refresh_memories();
    }
}

fn format_time(value: &str) -> String {
    let mut parts = value.split([' ', 'T']);
    let date = parts.next().unwrap_or_default();
    let mut time = parts.next().unwrap_or_default().split(':');
    let hour = time.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
    let minute = time.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
    format!("{date} {hour:02}:{minute:02} UTC")
}
