#![forbid(unsafe_code)]
use crate::{Dashboard, messages::Gate, session::Session};
use serde_json::{Value, json};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=window, js_name=chromieTextModel)]
    fn send(space: &str, operation: &str, payload: &str, generation: u32);
    #[wasm_bindgen(js_namespace=window, js_name=chromieCancelTextModel)]
    fn cancel();
    #[wasm_bindgen(js_namespace=window, js_name=chromieRequestId)]
    fn request_id() -> String;
}
#[derive(Default)]
struct State {
    config: Gate,
    jobs: Gate,
    loaded: bool,
    dirty: bool,
    jobs_loading: bool,
    revision: i64,
    last_done: String,
    baseline: Option<Value>,
}
struct Controller {
    ui: slint::Weak<Dashboard>,
    session: Rc<RefCell<Session>>,
    state: Rc<RefCell<State>>,
    _timer: Timer,
}
type Active = (Dashboard, Rc<RefCell<Session>>, Rc<RefCell<State>>);
thread_local! { static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) }; }
fn active() -> Option<Active> {
    CONTROLLER.with_borrow(|c| {
        let c = c.as_ref()?;
        Some((c.ui.upgrade()?, c.session.clone(), c.state.clone()))
    })
}

pub(crate) fn install(ui: &Dashboard, session: Rc<RefCell<Session>>) {
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(5), || load(false));
    CONTROLLER.with_borrow_mut(|c| {
        *c = Some(Controller {
            ui: ui.as_weak(),
            session,
            state: Rc::new(RefCell::new(State::default())),
            _timer: timer,
        })
    });
    ui.on_text_model_edited(|| {
        if let Some((ui, _, state)) = active() {
            state.borrow_mut().dirty = true;
            ui.set_text_model_notice("".into());
            ui.set_text_model_error("".into());
        }
    });
    ui.on_text_model_save(|| configure("save"));
    ui.on_text_model_test(|| configure("test"));
    ui.on_text_model_reload(|| {
        if let Some((_, _, state)) = active() {
            state.borrow_mut().dirty = false;
            launch("reload", json!({}));
        }
    });
    ui.on_text_model_disable(|| {
        if let Some((_, _, state)) = active() {
            let revision = state.borrow().revision;
            launch("disable", json!({"expected_revision":revision}));
        }
    });
    ui.on_text_extract(queue_extract);
    ui.on_text_jobs_refresh(|| load(true));
}

pub(crate) fn clear() {
    crate::extraction_browser::clear();
    cancel();
    if let Some((ui, _, state)) = active() {
        let mut s = state.borrow_mut();
        s.config.clear();
        s.jobs.clear();
        s.loaded = false;
        s.dirty = false;
        s.jobs_loading = false;
        s.last_done.clear();
        s.baseline = None;

        ui.set_text_model_busy(false);
        ui.set_text_model_error("".into());
        ui.set_text_model_notice("".into());
        ui.set_text_jobs_busy(false);
        ui.set_text_jobs_enabled(false);
        ui.set_text_jobs_automatic("".into());
        ui.set_text_jobs_summary("".into());
        ui.set_text_jobs_detail("".into());
        ui.set_text_jobs_error("".into());
        ui.set_text_jobs_failure("".into());
        ui.set_text_jobs_result("".into());
        ui.set_extraction_jobs(slint::ModelRc::default());
    }
}

pub(crate) fn load(force: bool) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() {
        return;
    }
    let read = ui.get_settings_page()
        && ui.get_can_manage_system()
        && !ui.get_text_model_busy()
        && !state.borrow().dirty
        && (force || !state.borrow().loaded);
    if read {
        launch("read", json!({}));
    }
    if !state.borrow().jobs_loading {
        launch("jobs", json!({}));
    }
}

fn configure(operation: &str) {
    let Some((ui, _, state)) = active() else {
        return;
    };
    let Ok(concurrency) = ui.get_text_model_concurrency().parse::<u32>() else {
        ui.set_text_model_error("并发须为 1～64 的整数。".into());
        return;
    };
    if !(1..=64).contains(&concurrency) || ui.get_text_model_name().trim().is_empty() {
        ui.set_text_model_error("请填写服务地址、模型名称和 1～64 的并发。".into());
        return;
    }
    let Some(provider_id) = crate::provider_browser::selected_text() else {
        ui.set_text_model_error("请先保存并选择一个服务商。".into());
        return;
    };
    let Ok(max_output_tokens) = ui.get_text_model_output_tokens().parse::<u32>() else {
        ui.set_text_model_error("输出预算须为1024～65536 Token。".into());
        return;
    };
    if !(1024..=65536).contains(&max_output_tokens) {
        ui.set_text_model_error("输出预算须为1024～65536 Token。".into());
        return;
    }
    let revision = state.borrow().revision;
    launch(
        operation,
        json!({"expected_revision":revision,"provider_id":provider_id,"model":ui.get_text_model_name().trim(),"max_concurrency":concurrency,"max_output_tokens":max_output_tokens}),
    );
}

fn queue_extract() {
    let Some((ui, _, _)) = active() else {
        return;
    };
    let ids = crate::messages_browser::current_event_ids();
    if ids.is_empty() {
        ui.set_text_jobs_error("先打开需要提炼的会话和消息页。".into());
        return;
    }
    let id = request_id();
    if id.is_empty() {
        ui.set_text_jobs_error("浏览器无法生成请求标识，请刷新重试。".into());
        return;
    }
    launch(
        "queue",
        json!({"request_id":id,"kind":"extract","memory_ids":[],"event_ids":ids}),
    );
}

fn launch(operation: &str, payload: Value) {
    let Some((ui, session, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() {
        return;
    }
    let generation = if matches!(operation, "jobs" | "queue") {
        state.borrow_mut().jobs_loading = true;
        if operation == "queue" {
            ui.set_text_jobs_busy(true);
            ui.set_text_jobs_error("".into());
            ui.set_text_jobs_failure("".into());
        }
        state.borrow_mut().jobs.begin(operation)
    } else {
        ui.set_text_model_busy(true);
        ui.set_text_model_error("".into());
        ui.set_text_model_notice("".into());
        state.borrow_mut().config.begin(operation)
    };
    send(
        &session.borrow().space,
        operation,
        &payload.to_string(),
        generation,
    );
}

fn state_name(state: &str) -> &'static str {
    match state {
        "pending" => "等待处理",
        "running" => "正在处理",
        "retry" => "等待自动重试",
        "done" => "已完成",
        "stale" => "输入或配置已变化，请重新发起",
        "cancelled" => "因遗忘已清理",
        _ => "处理失败",
    }
}

#[wasm_bindgen]
pub fn receive_text_model(operation: String, generation: u32, status: u16, body: String) {
    if matches!(operation.as_str(), "review" | "apply-revision") {
        crate::extraction_browser::receive(&operation, generation, status, &body);
        return;
    }
    let Some((ui, _, state)) = active() else {
        return;
    };
    if !ui.get_unlocked() {
        return;
    }
    let job = matches!(operation.as_str(), "jobs" | "queue");
    let accepted = if job {
        state.borrow_mut().jobs.accept(generation)
    } else {
        state.borrow_mut().config.accept(generation)
    };
    if accepted.as_deref() != Some(operation.as_str()) {
        return;
    }
    if job {
        state.borrow_mut().jobs_loading = false;
    } else {
        ui.set_text_model_busy(false);
    }
    if status == 401 {
        crate::browser::end_session();
        return;
    }
    let value = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    if status != 200 {
        let message = value["message"]
            .as_str()
            .unwrap_or("请求暂时失败，请刷新或检查模型配置。");
        if job {
            ui.set_text_jobs_busy(false);
            ui.set_text_jobs_error(message.into());
        } else {
            ui.set_text_model_error(message.into());
        }
        return;
    }
    match operation.as_str() {
        "read" | "reload" | "save" | "disable" => {
            let mut s = state.borrow_mut();
            let preserve = operation == "read" && s.dirty;
            let same = s.baseline.as_ref().is_some_and(|old| {
                crate::model_settings::text_edit_base(old)
                    == crate::model_settings::text_edit_base(&value)
            });
            s.loaded = true;
            if preserve && !same {
                ui.set_text_model_error(
                    "用途配置已在其他位置变更，草稿已保留；重新载入会放弃当前草稿。".into(),
                );
                return;
            }
            s.revision = value["revision"].as_i64().unwrap_or(0);
            s.baseline = Some(value.clone());
            if !preserve {
                s.dirty = false;
                crate::provider_browser::select_text(value["provider_id"].as_str());
                ui.set_text_model_enabled(value["enabled"] == true);
                ui.set_text_model_name(value["model"].as_str().unwrap_or_default().into());
                ui.set_text_model_concurrency(
                    value["max_concurrency"]
                        .as_u64()
                        .unwrap_or(4)
                        .to_string()
                        .into(),
                );
                ui.set_text_model_output_tokens(
                    value["max_output_tokens"]
                        .as_u64()
                        .unwrap_or(16384)
                        .to_string()
                        .into(),
                );
            } else {
                ui.set_text_model_notice("已同步连接版本，模型草稿已保留。".into());
            }
            if operation == "reload" {
                ui.set_text_model_notice("已载入最新配置。".into());
            } else if operation != "read" {
                crate::provider_browser::reload_organization();
                ui.set_text_model_notice(
                    if operation == "save" {
                        "已保存并启用，后台按自动整理设置运行。"
                    } else {
                        "已停用，已有记忆保持可用。"
                    }
                    .into(),
                );
            }
            drop(s);
            launch("jobs", json!({}));
        }
        "test" => ui.set_text_model_notice(
            format!(
                "连接成功 · {:.0} ms",
                value["elapsed_ms"].as_f64().unwrap_or(0.0)
            )
            .into(),
        ),
        "queue" => {
            ui.set_text_jobs_summary("任务已排队，完成后会自动更新。".into());
            launch("jobs", json!({}));
        }
        "jobs" => {
            let enabled = value["enabled"] == true;
            ui.set_text_jobs_enabled(enabled);
            let organization = &value["organization"];
            let status = match organization["state"].as_str().unwrap_or_default() {
                "unconfigured" => "文本模型未启用",
                "paused" => "已暂停 · 可在系统设置启用",
                "budget_limited" => "已达到今日上限，明日自动继续",
                "idle" => "已追平，等待新增或更正的记忆",
                _ => "正在分批处理历史与新增记忆",
            };
            ui.set_text_jobs_automatic(
                format!(
                    "{}\n已整理 {} / {} 条 · 待处理 {} · 失败 {}",
                    status,
                    organization["completed"].as_u64().unwrap_or(0),
                    organization["total"].as_u64().unwrap_or(0),
                    organization["pending"].as_u64().unwrap_or(0),
                    organization["failed"].as_u64().unwrap_or(0)
                )
                .into(),
            );
            let extraction = &value["extraction"];
            ui.set_text_jobs_automatic(
                format!(
                    "{}\n对话提炼：{} · 已处理 {} / {} 条消息",
                    ui.get_text_jobs_automatic(),
                    match extraction["state"].as_str().unwrap_or_default() {
                        "paused" => "已暂停",
                        "unconfigured" => "文本模型未启用",
                        "idle" => "已追平",
                        "budget_limited" => "今日额度已用完",
                        _ => "等待分批处理",
                    },
                    extraction["completed"].as_u64().unwrap_or(0),
                    extraction["total"].as_u64().unwrap_or(0)
                )
                .into(),
            );
            let jobs = value["jobs"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|j| !ui.get_messages_page() || j["kind"] == "extract")
                .collect::<Vec<_>>();
            let failure = jobs
                .first()
                .filter(|job| job["state"] != "done")
                .and_then(|job| job["result"]["error"]["message"].as_str())
                .unwrap_or_default();
            ui.set_text_jobs_failure(failure.into());
            let busy = jobs
                .iter()
                .any(|job| matches!(job["state"].as_str(), Some("pending" | "running" | "retry")));
            ui.set_text_jobs_busy(busy);
            let summary = if !enabled {
                "文本模型未启用，可在系统设置中配置。".to_owned()
            } else if let Some(job) = jobs.first() {
                format!(
                    "{} · {}",
                    if job["kind"] == "extract" {
                        "消息提炼"
                    } else {
                        "主题整理"
                    },
                    if job["state"] == "done"
                        && job["result"]["memory_ids"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                    {
                        "已完成，本次无需更新"
                    } else {
                        state_name(job["state"].as_str().unwrap_or_default())
                    }
                )
            } else {
                "尚无整理结果。后台完成后会自动更新。".into()
            };
            ui.set_text_jobs_summary(summary.into());
            let mut detail = Vec::new();
            let result_id = jobs
                .first()
                .filter(|job| job["state"] == "done")
                .and_then(|job| job["result"]["memory_ids"][0].as_str())
                .unwrap_or_default();
            crate::extraction_browser::jobs(&ui, &jobs);
            for job in jobs.iter().take(3) {
                let count = job["result"]["memory_ids"].as_array().map_or(0, Vec::len);
                detail.push(format!(
                    "{} · {}",
                    if job["kind"] == "extract" {
                        "消息提炼"
                    } else {
                        "主题整理"
                    },
                    if job["state"] == "done" {
                        if job["kind"] == "extract" && job["result"]["has_review"] == true {
                            format!(
                                "新增 {} · 待修订 {} · 暂缓 {}",
                                job["result"]["counts"]["create"],
                                job["result"]["pending_revisions"].as_u64().unwrap_or(0),
                                job["result"]["counts"]["defer"]
                            )
                        } else if count == 0 {
                            "无需更新".to_owned()
                        } else {
                            format!("写入 {count} 条")
                        }
                    } else {
                        state_name(job["state"].as_str().unwrap_or_default()).to_owned()
                    }
                ));
                if job["state"] != "done"
                    && let Some(message) = job["result"]["error"]["message"].as_str()
                {
                    detail.push(message.to_owned());
                }
                for conflict in job["result"]["conflicts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(2)
                {
                    detail.push(format!(
                        "待核对：{}",
                        conflict["reason"].as_str().unwrap_or_default()
                    ));
                }
            }
            ui.set_text_jobs_detail(detail.join("\n").into());
            ui.set_text_jobs_result(result_id.into());
            if let Some(job) = jobs.iter().find(|job| job["state"] == "done") {
                let id = job["job_id"].as_str().unwrap_or_default();
                let changed = state.borrow().last_done != id;
                if changed {
                    state.borrow_mut().last_done = id.into();
                    crate::knowledge_browser::load(true);
                    if job["result"]["memory_ids"]
                        .as_array()
                        .is_some_and(|ids| !ids.is_empty())
                        && !ui.get_loading()
                        && !ui.get_settings_page()
                        && !ui.get_messages_page()
                        && !ui.get_access_page()
                        && !ui.get_recall_page()
                        && !ui.get_transfer_page()
                    {
                        ui.invoke_refresh();
                    }
                }
            }
        }
        _ => (),
    }
}

pub(crate) fn reload_settings() {
    launch("read", json!({}));
}

pub(crate) fn refresh_memories() {
    if let Some((ui, _, _)) = active() {
        ui.invoke_refresh();
    }
}
