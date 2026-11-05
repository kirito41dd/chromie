#![cfg_attr(not(test), forbid(unsafe_code))]

/// Slint 提供 UTF-8 字节偏移。只允许完整字符边界，避免中文／emoji 截断。
fn selected(text: &str, anchor: i32, cursor: i32) -> Option<&str> {
    let anchor = usize::try_from(anchor).ok()?;
    let cursor = usize::try_from(cursor).ok()?;
    if anchor == cursor {
        return None;
    }
    text.get(anchor.min(cursor)..anchor.max(cursor))
}

#[derive(Default)]
struct Snapshot {
    owner: i32,
    text: String,
}
impl Snapshot {
    fn begin(&mut self) -> i32 {
        self.owner = self.owner.wrapping_add(1);
        if self.owner == 0 {
            self.owner = 1;
        }
        self.text.clear();
        self.owner
    }
    fn update(&mut self, owner: i32, text: &str, anchor: i32, cursor: i32) {
        if owner == self.owner {
            self.text = selected(text, anchor, cursor).unwrap_or_default().into();
        }
    }
    fn end(&mut self, owner: i32) {
        if owner == self.owner {
            self.begin();
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use crate::Dashboard;
    use slint::ComponentHandle;
    use std::cell::RefCell;
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = window, js_name = chromieArrowPointer)]
        fn arrow_pointer(arrow: bool);
    }
    thread_local! {
        static CURRENT: RefCell<Option<(slint::Weak<Dashboard>, Snapshot)>> = const { RefCell::new(None) };
    }
    pub(crate) fn install(ui: &Dashboard) {
        ui.global::<crate::PointerBridge>().on_update(arrow_pointer);
        CURRENT.with_borrow_mut(|c| *c = Some((ui.as_weak(), Snapshot::default())));
        let bridge = ui.global::<crate::CopyBridge>();
        bridge.on_begin(|| CURRENT.with_borrow_mut(|c| c.as_mut().map_or(0, |(_, s)| s.begin())));
        bridge.on_update(|owner, text, a, b| {
            CURRENT.with_borrow_mut(|c| {
                if let Some((_, s)) = c {
                    s.update(owner, &text, a, b);
                }
            })
        });
        bridge.on_end(|owner| {
            CURRENT.with_borrow_mut(|c| {
                if let Some((_, s)) = c {
                    s.end(owner);
                }
            })
        });
    }
    pub(crate) fn clear() {
        CURRENT.with_borrow_mut(|c| {
            if let Some((_, s)) = c {
                s.begin();
            }
        });
    }
    #[wasm_bindgen]
    pub fn selected_text() -> String {
        CURRENT.with_borrow(|c| {
            c.as_ref()
                .filter(|(ui, _)| ui.upgrade().is_some_and(|ui| ui.get_unlocked()))
                .map_or_else(String::new, |(_, s)| s.text.clone())
        })
    }
}
#[cfg(target_arch = "wasm32")]
pub use browser::selected_text;
#[cfg(target_arch = "wasm32")]
pub(crate) use browser::{clear, install};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_focus_loss_cannot_clear_new_selection_and_explicit_clear_invalidates_old_owner() {
        let mut state = Snapshot::default();
        let old = state.begin();
        state.update(old, "old", 0, 3);
        let new = state.begin();
        state.update(new, "中文", 0, 6);
        state.end(old);
        assert_eq!(state.text, "中文");
        state.begin();
        state.update(new, "中文", 0, 6);
        assert!(state.text.is_empty());
        let last = state.begin();
        state.update(last, "shown", 0, 5);
        state.end(last);
        assert!(state.text.is_empty());
    }
    #[test]
    fn selected_copy_preserves_utf8_reverse_selection_and_rejects_invalid_ranges() {
        let text = "a中文🙂\nEND";
        assert_eq!(selected(text, 1, 11), Some("中文🙂"));
        assert_eq!(selected(text, 11, 1), Some("中文🙂"));
        assert_eq!(selected(text, 1, 15), Some("中文🙂\nEND"));
        for (a, b) in [(0, 0), (-1, 3), (1, 2), (0, 100)] {
            assert_eq!(selected(text, a, b), None);
        }
    }
}
