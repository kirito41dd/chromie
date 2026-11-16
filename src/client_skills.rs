//! 与 Codex 插件共用同一份按需工作流；导出给其他支持 Skill 的 Agent 使用。
use crate::error::{Error, Result};
use std::{fs, path::Path};

pub(crate) const FILES: &[(&str, &[u8])] = &[
    (
        "SKILL.md",
        include_bytes!("../integrations/codex/plugin/skills/memory/SKILL.md"),
    ),
    (
        "references/retrieval.md",
        include_bytes!("../integrations/codex/plugin/skills/memory/references/retrieval.md"),
    ),
    (
        "references/organization.md",
        include_bytes!("../integrations/codex/plugin/skills/memory/references/organization.md"),
    ),
    (
        "references/secrets.md",
        include_bytes!("../integrations/codex/plugin/skills/memory/references/secrets.md"),
    ),
];

pub fn export(directory: &Path, replace: bool) -> Result<serde_json::Value> {
    let directory =
        std::path::absolute(directory).map_err(|_| Error::invalid("无法确定 Skill 导出目录"))?;
    for (relative, bytes) in FILES {
        let path = directory.join(relative);
        if path.symlink_metadata().is_ok()
            && !replace
            && fs::read(&path).map_err(|_| Error::invalid("无法核对已有 Skill 文件"))? != *bytes
        {
            return Err(Error::invalid(
                "目标已有不同 Skill 文件；确认覆盖后使用 --replace",
            ));
        }
    }
    crate::codex_setup::directory(&directory, true)?;
    crate::codex_setup::directory(&directory.join("references"), true)?;
    for (relative, bytes) in FILES {
        crate::codex_setup::replace(&directory.join(relative), bytes)?;
    }
    Ok(
        serde_json::json!({"exported":true,"path":directory,"skill":"chromie-memory","next":"将此目录放入使用者的 Agent Skill 目录；实时服务能力与权限仍由 MCP 提供。"}),
    )
}
