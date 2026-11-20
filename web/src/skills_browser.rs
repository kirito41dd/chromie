#![forbid(unsafe_code)]
use crate::{Dashboard, LibrarySkill};
use serde::Deserialize;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = chromieSkills)]
    fn fetch(space: &str, channel: &str, value: &str, revision: i32, path: &str);
    #[wasm_bindgen(js_namespace = window, js_name = chromieSkillFilePage)]
    fn file_page(page: i32);
    #[wasm_bindgen(js_namespace = window, js_name = chromieDownloadSkillFile)]
    fn download();
    #[wasm_bindgen(js_namespace = window, js_name = chromieSkillArchive)]
    fn download_archive(space: &str, id: &str, revision: i32);
    #[wasm_bindgen(js_namespace = window, js_name = chromieCancelSkills)]
    fn cancel(channel: &str);
}

thread_local! { static UI: RefCell<Option<slint::Weak<Dashboard>>> = const {RefCell::new(None)}; }

fn current_ui() -> Option<Dashboard> {
    UI.with_borrow(|current| current.as_ref()?.upgrade())
}

#[derive(Deserialize)]
struct Item {
    id: String,
    name: String,
    description: String,
    revision: i32,
}
#[derive(Deserialize)]
struct Page {
    items: Vec<Item>,
    next: Option<String>,
}
#[derive(Deserialize)]
struct Entry {
    revision: i32,
    files: Vec<PackageFile>,
}

#[derive(Deserialize)]
struct PackageFile {
    path: String,
}

#[derive(Deserialize)]
struct FilePage {
    content: String,
    binary: bool,
    page: i32,
    count: i32,
}

fn close_entry(ui: &Dashboard) {
    cancel("detail");
    cancel("file");
    ui.set_library_selected("".into());
    ui.set_library_entry("".into());
    ui.set_library_paths(ModelRc::default());
    ui.set_library_revision(0);
    ui.set_library_path("".into());
    ui.set_library_file_note("".into());
    ui.set_library_file_count(0);
    ui.set_library_file_page(0);
    ui.set_library_detail_note("".into());
}

pub(crate) fn clear() {
    cancel("list");
    cancel("archive");
    if let Some(ui) = current_ui() {
        close_entry(&ui);
        ui.set_library_downloading("".into());
        ui.set_library_skills(ModelRc::default());
        ui.set_library_next("".into());
        ui.set_library_error("".into());
        ui.set_library_busy(false);
    }
}

pub(crate) fn install(ui: &Dashboard) {
    UI.with_borrow_mut(|current| *current = Some(ui.as_weak()));
    ui.on_library_load(load);
    ui.on_library_open_file(|path| {
        if let Some(ui) = current_ui() {
            open_file(&ui, path.as_str());
        }
    });
    ui.on_library_turn_file(file_page);
    ui.on_library_download(download);
    ui.on_library_archive(|id, revision| {
        let Some(ui) = current_ui() else { return };
        if ui.get_unlocked() && ui.get_skills_page() && ui.get_library_downloading().is_empty() {
            ui.set_library_error("".into());
            ui.set_library_downloading(id.clone());
            download_archive(ui.get_space_id().as_str(), id.as_str(), revision);
        }
    });
    let weak = ui.as_weak();
    ui.on_library_select(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        let close = ui.get_library_selected() == id;
        close_entry(&ui);
        if !close && ui.get_unlocked() && ui.get_skills_page() {
            ui.set_library_selected(id.clone());
            ui.set_library_detail_note("正在读取文件列表…".into());
            fetch(ui.get_space_id().as_str(), "detail", id.as_str(), 0, "");
        }
    });
}

pub(crate) fn load(more: bool) {
    let Some(ui) = current_ui() else { return };
    if !ui.get_unlocked() || !ui.get_skills_page() {
        return;
    }
    let cursor = if more {
        ui.get_library_next()
    } else {
        close_entry(&ui);
        "".into()
    };
    if more && cursor.is_empty() {
        return;
    }
    ui.set_library_error("".into());
    ui.set_library_busy(true);
    fetch(ui.get_space_id().as_str(), "list", cursor.as_str(), 0, "");
}

fn open_file(ui: &Dashboard, path: &str) {
    if !ui.get_unlocked()
        || !ui.get_skills_page()
        || !ui
            .get_library_paths()
            .iter()
            .any(|item| item.as_str() == path)
    {
        return;
    }
    ui.set_library_path(path.into());
    ui.set_library_entry("".into());
    ui.set_library_file_count(0);
    ui.set_library_file_page(0);
    ui.set_library_file_note("正在读取文件…".into());
    fetch(
        ui.get_space_id().as_str(),
        "file",
        ui.get_library_selected().as_str(),
        ui.get_library_revision(),
        path,
    );
}

#[wasm_bindgen]
pub fn receive_skills(space: &str, channel: &str, value: &str, status: u16, body: &str) {
    let Some(ui) = current_ui() else { return };
    if !ui.get_unlocked() || !ui.get_skills_page() || ui.get_space_id() != space {
        return;
    }
    if status == 401 {
        crate::browser::end_session();
        return;
    }
    if channel == "archive" && ui.get_library_downloading() == value {
        ui.set_library_downloading("".into());
        match status {
            200 => {}
            409 => ui.set_library_error("该 Skill 已发布新修订，请刷新列表后重新下载。".into()),
            _ => ui.set_library_error("整包下载失败，请重试。".into()),
        }
    } else if channel == "list" {
        ui.set_library_busy(false);
        if let Some(page) = (status == 200)
            .then(|| serde_json::from_str::<Page>(body).ok())
            .flatten()
        {
            let mut rows: Vec<_> = if value.is_empty() {
                Vec::new()
            } else {
                ui.get_library_skills().iter().collect()
            };
            rows.extend(page.items.into_iter().map(|i| LibrarySkill {
                id: i.id.into(),
                name: i.name.into(),
                description: i.description.into(),
                revision: i.revision,
            }));
            ui.set_library_skills(ModelRc::new(VecModel::from(rows)));
            ui.set_library_next(page.next.unwrap_or_default().into());
        } else {
            ui.set_library_error("Skill 列表读取失败，请重试。".into());
        }
    } else if channel == "detail" && ui.get_library_selected() == value {
        if let Some(detail) = (status == 200)
            .then(|| serde_json::from_str::<Entry>(body).ok())
            .flatten()
        {
            ui.set_library_revision(detail.revision);
            let mut paths = detail
                .files
                .into_iter()
                .map(|file| slint::SharedString::from(file.path))
                .collect::<Vec<_>>();
            if let Some(index) = paths.iter().position(|path| path == "SKILL.md") {
                paths[..=index].rotate_right(1);
            }
            ui.set_library_paths(ModelRc::new(VecModel::from(paths)));
            ui.set_library_detail_note("".into());
            open_file(&ui, "SKILL.md");
        } else {
            ui.set_library_detail_note("文件列表读取失败，请收起后重试。".into());
        }
    } else if channel == "file" && ui.get_library_path() == value {
        if let Some(preview) = (status == 200)
            .then(|| serde_json::from_str::<FilePage>(body).ok())
            .flatten()
        {
            ui.set_library_file_note(
                if preview.binary {
                    "二进制 · 十六进制视图"
                } else if preview.content.is_empty() {
                    "空文件"
                } else {
                    ""
                }
                .into(),
            );
            ui.set_library_entry(preview.content.into());
            ui.set_library_file_page(preview.page);
            ui.set_library_file_count(preview.count);
        } else {
            let note = match status {
                409 => "该 Skill 已发布新修订，请刷新列表后再打开。",
                404 => "当前修订中找不到这个文件，请刷新列表。",
                _ => "文件读取失败，点文件名可重试。",
            };
            ui.set_library_file_note(note.into());
        }
    }
}
