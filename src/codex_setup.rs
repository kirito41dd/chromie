use crate::{
    agent_key::KeyKind,
    client_config,
    error::{Error, Result},
};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table, value};
use uuid::Uuid;

const CONFIG_LIMIT: u64 = 1024 * 1024;

pub fn default_data_dir() -> Result<PathBuf> {
    Ok(user_home()?.join(".config/chromie/codex"))
}

pub fn default_codex_dir() -> Result<PathBuf> {
    // 不重定义或修改 CODEX_HOME，仅读取当前客户端已配置的位置。
    Ok(std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or(user_home()?.join(".codex")))
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::invalid("无法定位用户目录，请显式指定配置目录"))
}

pub(crate) fn directory(path: &Path, private: bool) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::invalid("本机配置目录必须使用绝对路径"));
    }
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(path)
            .map_err(|_| Error::invalid("无法创建本机配置目录"))?;
    }
    let meta = fs::symlink_metadata(path).map_err(|_| Error::invalid("无法检查本机配置目录"))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::invalid("本机配置目录不能是符号链接或普通文件"));
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::unavailable())?;
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}

fn read_existing(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= CONFIG_LIMIT =>
        {
            fs::read(path)
                .map(Zeroizing::new)
                .map(Some)
                .map_err(|_| Error::invalid("无法读取已有本机配置"))
        }
        Ok(_) => Err(Error::invalid(
            "已有本机配置必须是普通文件，最大 1 MiB，不接受符号链接",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::invalid("无法检查已有本机配置")),
    }
}

fn same_target(a: &client_config::ClientConnection, b: &client_config::ClientConnection) -> bool {
    a.agent_id == b.agent_id && a.base_url == b.base_url && a.space_id == b.space_id
}

fn matches_mcp(
    server: &Item,
    current: &client_config::ClientConnection,
    old: Option<&client_config::ClientConnection>,
) -> Result<bool> {
    let table = server
        .as_table_like()
        .ok_or_else(|| Error::invalid("已有 Chromie 配置结构无法合并"))?;
    let existing_key = table
        .get("http_headers")
        .and_then(Item::as_table_like)
        .and_then(|headers| headers.get("Authorization"))
        .and_then(Item::as_str);
    let key = Zeroizing::new(format!("Bearer {}", current.key.as_str()));
    let old_key = old
        .filter(|old| same_target(old, current))
        .map(|old| Zeroizing::new(format!("Bearer {}", old.key.as_str())));
    let environment_auth = table
        .get("env_http_headers")
        .and_then(Item::as_table_like)
        .is_some_and(|headers| {
            headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        });
    Ok(table.get("url").and_then(Item::as_str)
        == Some(format!("{}/spaces/{}/mcp", current.base_url, current.space_id).as_str())
        && table.get("command").is_none()
        && table.get("bearer_token_env_var").is_none()
        && !environment_auth
        && (existing_key == Some(key.as_str())
            || old_key
                .as_ref()
                .is_some_and(|key| existing_key == Some(key.as_str()))))
}

fn local_conflict(message: &'static str) -> Error {
    Error {
        code: "local_connection_conflict",
        message,
        status: reqwest::StatusCode::CONFLICT,
    }
}

/// 只读预检：浏览器授权前发现无效 TOML、路径或已有连接，不写入文件与备份。
pub fn needs_replacement(source: Option<&Path>, codex_dir: &Path, data_dir: &Path) -> Result<bool> {
    for directory in [codex_dir, data_dir] {
        if !directory.is_absolute() {
            return Err(Error::invalid("本机配置目录必须使用绝对路径"));
        }
        match fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::invalid("本机配置目录不能是符号链接或普通文件")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::invalid("无法检查本机配置目录")),
        }
    }
    let config = read_existing(&codex_dir.join("config.toml"))?;
    let text = config
        .as_deref()
        .map(|bytes| std::str::from_utf8(bytes))
        .transpose()
        .map_err(|_| Error::invalid("已有 Codex 配置不是 UTF-8，未修改文件"))?
        .unwrap_or("");
    let document = text
        .parse::<DocumentMut>()
        .map_err(|_| Error::invalid("已有 Codex TOML 无效，未修改文件"))?;
    let server = match document.get("mcp_servers") {
        Some(item) => item
            .as_table_like()
            .ok_or_else(|| Error::invalid("已有 mcp_servers 结构无法合并"))?
            .get("chromie"),
        None => None,
    };
    if let Some(server) = server {
        server
            .as_table_like()
            .ok_or_else(|| Error::invalid("已有 Chromie 配置结构无法合并"))?;
    }
    let client_path = data_dir.join("client.json");
    let old = if read_existing(&client_path)?.is_some() {
        Some(client_config::load(KeyKind::Mcp, Some(&client_path))?)
    } else {
        None
    };
    let Some(source) = source else {
        return Ok(old.is_some() || server.is_some());
    };
    let incoming = client_config::load(KeyKind::Mcp, Some(source))?;
    client_config::load(KeyKind::Hook, Some(source))?;
    Ok(old.as_ref().is_some_and(|old| !same_target(old, &incoming))
        || server
            .map(|item| matches_mcp(item, &incoming, old.as_ref()).map(|same| !same))
            .transpose()?
            .unwrap_or(false))
}

pub(crate) fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| Error::invalid("无法创建私有配置或已有初始化正在执行"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::unavailable())
}

pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(Error::unavailable)?;
    let staging = parent.join(format!(".chromie-{}.part", Uuid::now_v7()));
    create_private(&staging, bytes)?;
    if fs::rename(&staging, path).is_err() {
        let _ = fs::remove_file(&staging);
        return Err(Error::invalid("写入配置未完成，原文件未被截断"));
    }
    // rename 后新文件已可见；目录同步不支持时不能只回滚另一份配置。
    #[cfg(unix)]
    let _ = fs::File::open(parent).and_then(|file| file.sync_all());
    Ok(())
}

struct SetupLock(PathBuf);
impl Drop for SetupLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// 私有文件进入帮助程序，不进入 agent 的参数和输出；只合并固定 MCP 表。
pub fn run(
    source: &Path,
    codex_dir: &Path,
    data_dir: &Path,
    replace_connection: bool,
) -> Result<Value> {
    if !source.is_absolute() {
        return Err(Error::invalid("接入文件必须使用绝对路径"));
    }
    let mcp = client_config::load(KeyKind::Mcp, Some(source))?;
    let hook = client_config::load(KeyKind::Hook, Some(source))?;
    if mcp.agent_id != hook.agent_id
        || mcp.base_url != hook.base_url
        || mcp.space_id != hook.space_id
    {
        return Err(Error::invalid("初始化过程中接入文件发生变化，请重新执行"));
    }
    directory(codex_dir, false)?;
    directory(data_dir, true)?;
    let lock = codex_dir.join("chromie-setup.lock");
    create_private(&lock, b"Chromie initialization\n")?;
    let _lock = SetupLock(lock);
    let config_path = codex_dir.join("config.toml");
    let client_path = data_dir.join("client.json");
    let legacy_hooks = read_existing(&codex_dir.join("hooks.json"))?.is_some_and(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        text.contains("chromie") && text.contains("capture")
    });
    let before = read_existing(&config_path)?;
    let previous_client = read_existing(&client_path)?;
    let old = if previous_client.is_some() {
        Some(client_config::load(KeyKind::Mcp, Some(&client_path))?)
    } else {
        None
    };
    let same_connection = old.as_ref().is_some_and(|old| same_target(old, &mcp));
    if old.is_some() && !same_connection && !replace_connection {
        return Err(local_conflict(
            "本机已绑定另一 Chromie 接入；确认更换后才可使用 --replace",
        ));
    }
    let text = before
        .as_deref()
        .map(|bytes| std::str::from_utf8(bytes))
        .transpose()
        .map_err(|_| Error::invalid("已有 Codex 配置不是 UTF-8，未修改文件"))?
        .unwrap_or("");
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|_| Error::invalid("已有 Codex TOML 无效，未修改文件，也未回显配置内容"))?;
    let endpoint = format!("{}/spaces/{}/mcp", mcp.base_url, mcp.space_id);
    if !document.contains_key("mcp_servers") {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| Error::invalid("已有 mcp_servers 结构无法合并，未修改文件"))?;
    if let Some(server) = servers.get("chromie") {
        let same = matches_mcp(server, &mcp, old.as_ref())?;
        if !same && !replace_connection {
            return Err(local_conflict(
                "Codex 已有不同的 Chromie MCP 配置；确认更换后才可使用 --replace",
            ));
        }
        if !same {
            servers.insert("chromie", Item::Table(Table::new()));
        }
    } else {
        servers.insert("chromie", Item::Table(Table::new()));
    }
    let server = servers
        .get_mut("chromie")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(Error::unavailable)?;
    server.insert("url", value(&endpoint));
    if server.get("http_headers").is_none() {
        server.insert("http_headers", Item::Table(Table::new()));
    }
    server
        .get_mut("http_headers")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| Error::invalid("已有 MCP 请求头结构无效"))?
        .insert(
            "Authorization",
            value(format!("Bearer {}", mcp.key.as_str())),
        );
    let rendered = Zeroizing::new(document.to_string());
    let client = Zeroizing::new(serde_json::to_vec_pretty(&json!({"url":mcp.base_url,"space":mcp.space_id,"agent_id":mcp.agent_id,"mcp_key":mcp.key.as_str(),"hook_key":hook.key.as_str()}))
        .map_err(|_| Error::unavailable())?);
    let changed_config = before
        .as_ref()
        .is_none_or(|old| old.as_slice() != rendered.as_bytes());
    let changed_client = previous_client
        .as_ref()
        .is_none_or(|old| old.as_slice() != client.as_slice());
    let backup = if changed_config && let Some(bytes) = &before {
        let path = codex_dir.join(format!("config.chromie-backup-{}.toml", Uuid::now_v7()));
        create_private(&path, bytes)?;
        Some(path)
    } else {
        None
    };
    if read_existing(&config_path)?.as_deref() != before.as_deref()
        || read_existing(&client_path)?.as_deref() != previous_client.as_deref()
    {
        return Err(Error::invalid(
            "本机配置被其他程序修改，请重新初始化，未覆盖新改动",
        ));
    }
    if changed_client {
        replace(&client_path, &client)?;
    }
    if changed_config && let Err(error) = replace(&config_path, rendered.as_bytes()) {
        let rollback = match &previous_client {
            Some(bytes) => replace(&client_path, bytes),
            None => fs::remove_file(&client_path).map_err(|_| Error::unavailable()),
        };
        if rollback.is_err() {
            return Err(Error::invalid(
                "Codex 配置写入失败，本机接入回滚未完成，请检查私有文件与备份",
            ));
        }
        return Err(error);
    }
    // 内容相同也收紧已有文件权限，避免旧配置的宽松权限继续暴露鉴权头。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::invalid("无法将 Codex 配置权限设为 0600，请检查文件权限"))?;
    }
    Ok(
        json!({"configured":true,"changed":changed_config || changed_client,"agent_id":mcp.agent_id,"space":mcp.space_id,
        "client_config":client_path,"codex_config":config_path,"backup":backup,"legacy_chromie_hook_detected":legacy_hooks,
        "next":"重载 MCP 或启动新会话；在 /hooks 审核信任插件 hook，避免与旧手动 Chromie hook 重复。"}),
    )
}
