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
        launch();
    });
    ui.on_extraction_close(clear);
    ui.on_extraction_refresh(launch);
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
fn launch() {
    let operation = "review";
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
            let payload = json!({"job_id":c.job});
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
    let sorted = crate::memory::recent_jobs(rows);
    let items = sorted
        .iter()
        .copied()
        .filter(|j| j["kind"] == "extract" && j["result"]["has_review"] == true)
        .take(20)
        .map(|j| {
            let counts = &j["result"]["counts"];
            ExtractionJob {
                id: j["job_id"].as_str().unwrap_or_default().into(),
                time: format_time(j["created_at"].as_str().unwrap_or_default()).into(),
                label: format!(
                    "新增 {} · 待 AI 复核 {} · 修订 {} · 跳过 {} · 暂缓 {}",
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
    let maintenance = sorted
        .into_iter()
        .filter(|j| {
            matches!(j["kind"].as_str(), Some("maintain" | "organize")) && j["state"] == "done"
        })
        .take(20)
        .map(|j| ExtractionJob {
            id: j["job_id"].as_str().unwrap_or_default().into(),
            time: format_time(j["created_at"].as_str().unwrap_or_default()).into(),
            label: format!(
                "{} · 主题 {} · 合并 {} · 自动修订 {} · 暂缓 {}",
                if j["kind"] == "maintain" {
                    "定期维护"
                } else {
                    "增量整理"
                },
                j["result"]["topic_changes"].as_array().map_or(0, Vec::len),
                j["result"]["merged_topics"].as_array().map_or(0, Vec::len),
                j["result"]["counts"]["applied"].as_u64().unwrap_or(0),
                j["result"]["counts"]["defer"].as_u64().unwrap_or(0)
            )
            .into(),
        })
        .collect::<Vec<_>>();
    ui.set_maintenance_jobs(Rc::new(VecModel::from(maintenance)).into());
}
pub(crate) fn receive(operation: &str, generation: u32, status: u16, body: &str) {
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
            let result = &value["result"];
            ui.set_extraction_note(
                if result["detail_expired"] == true {
                    "此记录的详细正文已超过保留范围，任务状态和统计仍保留。"
                } else if value["state"] == "done" {
                    if result["retrieval_partial"] == true {
                        "已自动处理。相关记忆检索范围有限；未通过证据校验的内容保持现状。"
                    } else {
                        "已自动处理。修订和主题更新已写入；证据不足的事项保留现状。"
                    }
                } else {
                    "尚未完成核对；请稍后刷新。"
                }
                .into(),
            );
            let mut rows = result["decisions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| {
                    let action = match (
                        d["action"].as_str().unwrap_or_default(),
                        d["resolution"].as_str(),
                    ) {
                        (_, Some("superseded")) => "方案已过期",
                        (_, Some("automatic")) => "已自动修订",
                        (_, Some("confirmed")) => "已修订",
                        (_, Some("rechecking")) => "AI 复核中",
                        (_, Some("rechecked")) => "AI 已重新核对",
                        (_, Some("deferred")) => "已暂缓",
                        ("create", _) => "已新增",
                        ("propose_revision", _) => "历史建议（等待 AI 复核）",
                        ("skip", _) => "已跳过",
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
                                    Some("agent_reported") => format!(
                                        "Agent 使用报告（非独立验证） · {}",
                                        e["occurred_at"].as_str().unwrap_or_default()
                                    ),
                                    _ => "来源消息".to_owned(),
                                },
                                e["quote"].as_str().unwrap_or_default(),
                                e["reference"]
                                    .as_str()
                                    .or_else(|| e["event_id"].as_str())
                                    .unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    ExtractionDecision {
                        action: action.into(),
                        reason: d["reason"].as_str().unwrap_or_default().into(),
                        body: d["content"].as_str().unwrap_or_default().into(),
                        evidence: evidence.into(),
                        previous: d["previous_content"].as_str().unwrap_or_default().into(),
                        proposed: d["replacement"]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                        applied: d["applied"] == true,
                    }
                })
                .collect::<Vec<_>>();
            for change in result["topic_changes"].as_array().into_iter().flatten() {
                rows.push(ExtractionDecision {
                    action: if change["action"] == "created" {
                        "已新增主题".into()
                    } else {
                        "已更新主题".into()
                    },
                    body: change["title"].as_str().unwrap_or_default().into(),
                    reason: format!(
                        "主题 ID：{}",
                        change["memory_id"].as_str().unwrap_or_default()
                    )
                    .into(),
                    ..Default::default()
                });
            }
            for merge in result["merged_topics"].as_array().into_iter().flatten() {
                rows.push(ExtractionDecision {
                    action: "已合并主题".into(),
                    body: merge["title"].as_str().unwrap_or_default().into(),
                    reason: format!(
                        "{} → {}；旧入口仍可跳转，历史保留。",
                        merge["from"].as_str().unwrap_or_default(),
                        merge["to"].as_str().unwrap_or_default()
                    )
                    .into(),
                    ..Default::default()
                });
            }
            for conflict in result["conflicts"].as_array().into_iter().flatten() {
                rows.push(ExtractionDecision {
                    action: "保留现状".into(),
                    reason: conflict["reason"].as_str().unwrap_or_default().into(),
                    ..Default::default()
                });
            }
            ui.set_extraction_decisions(Rc::new(VecModel::from(rows)).into());
        }
    });
    if expired {
        crate::browser::end_session();
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
