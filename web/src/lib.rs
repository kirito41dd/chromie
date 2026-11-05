#![cfg_attr(not(test), deny(clippy::string_slice))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(unsafe_code))]

// 用户批准的唯一例外：Slint 生成的绑定。手写模块各自保持 forbid。
#[allow(unsafe_code)]
mod generated {
    slint::slint! {
        #[style = "fluent-light"]
        export { ConversationRow, CapturedRow } from "../ui/messages.slint";
        export { TransferRow } from "../ui/transfer.slint";
        export { AgentRow } from "../ui/access.slint";
        export { SecretLine } from "../ui/secret-dialog.slint";
        export { CopyBridge, PointerBridge } from "../ui/selectable-text.slint";
        export { Dashboard } from "../ui/dashboard.slint";
        export { UsageTotal, UsageDay } from "../ui/model-usage.slint";
        export { MemoryRow } from "../ui/common.slint";
        export { RecallRow, RecallStage, RecallOverview } from "../ui/recall-models.slint";
    }
}

pub use generated::{
    AgentRow, CapturedRow, ConversationRow, CopyBridge, Dashboard, MemoryRow, PointerBridge,
    RecallOverview, RecallRow, RecallStage, SecretLine, TransferRow, UsageDay, UsageTotal,
};

#[cfg(any(target_arch = "wasm32", test))]
mod session;

#[cfg(any(target_arch = "wasm32", test))]
mod recall;

#[cfg(any(target_arch = "wasm32", test))]
mod memory;
#[cfg(target_arch = "wasm32")]
mod model_browser;
#[cfg(any(target_arch = "wasm32", test))]
mod model_settings;
#[cfg(target_arch = "wasm32")]
pub use model_browser::receive_model;

#[cfg(any(target_arch = "wasm32", test))]
mod secret_view;

#[cfg(any(target_arch = "wasm32", test))]
mod secret_format;

#[cfg(any(target_arch = "wasm32", test))]
mod selection;
#[cfg(target_arch = "wasm32")]
pub use selection::selected_text;

#[cfg(any(target_arch = "wasm32", test))]
mod diagnostics;

#[cfg(any(target_arch = "wasm32", test))]
mod access;

#[cfg(any(target_arch = "wasm32", test))]
#[path = "../../src/shell_commands.rs"]
mod shell_commands;

// 共用不依赖后端运行时的标识类型，保持 HTTP、CLI 与页面的校验一致。
#[cfg(any(target_arch = "wasm32", test))]
#[path = "../../src/space_key.rs"]
mod space_key;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod access_browser;

#[cfg(target_arch = "wasm32")]
pub use access_browser::{hide_access_keys, receive_access, receive_access_notice, resume_access};

#[cfg(target_arch = "wasm32")]
mod recall_browser;

#[cfg(target_arch = "wasm32")]
mod secret_browser;

#[cfg(target_arch = "wasm32")]
pub use secret_browser::{
    hide_secret, receive_secret, receive_secret_copy, receive_secret_verification,
};

#[cfg(target_arch = "wasm32")]
pub use recall_browser::receive_recall;

#[cfg(target_arch = "wasm32")]
pub use browser::{
    end_session, receive_snapshot, resize_viewport, restore_session, set_access_page,
    set_messages_page, set_page, set_settings_page, set_transfer_page, start,
};

#[cfg(test)]
mod layout_tests;

#[cfg(any(target_arch = "wasm32", test))]
mod messages;
#[cfg(target_arch = "wasm32")]
mod messages_browser;
#[cfg(target_arch = "wasm32")]
pub use messages_browser::receive_messages;

#[cfg(target_arch = "wasm32")]
mod transfer_browser;
#[cfg(any(target_arch = "wasm32", test))]
mod transfer_view;

#[cfg(any(target_arch = "wasm32", test))]
mod transfer_access;
#[cfg(target_arch = "wasm32")]
pub use transfer_browser::{receive_transfer, receive_transfer_file};
