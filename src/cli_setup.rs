//! 接入引导只打印公开确认链接与结果；接入密钥仅在本机私有文件中流转。
use crate::{
    agent_key::KeyKind,
    client_config, codex_setup,
    error::{Error, Result},
    space_key::SpaceKey,
};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Init,
    Doctor,
    HookInstall,
}

#[derive(Default)]
pub struct Options {
    pub config: Option<PathBuf>,
    pub url: Option<String>,
    pub space: Option<String>,
    pub codex_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub generic: bool,
    pub replace: bool,
    pub reconnect: bool,
    pub no_browser: bool,
    pub test_capture: bool,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn codex_output(
    codex_dir: &Path,
    args: &[&str],
) -> std::io::Result<(PathBuf, std::process::Output)> {
    let program = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("codex"))
            .find(|path| executable(path))
    });
    #[cfg(target_os = "macos")]
    let program = program.or_else(|| {
        let relative = "ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";
        let mut candidates = vec![PathBuf::from("/Applications").join(relative)];
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join("Applications").join(relative));
        }
        candidates.into_iter().find(|path| executable(path))
    });
    let program = program.unwrap_or_else(|| PathBuf::from("codex"));
    let output = Command::new(&program)
        .args(args)
        .env("CODEX_HOME", codex_dir)
        .stdin(Stdio::null())
        .output()?;
    Ok((program, output))
}

fn invoke_codex(codex_dir: &Path, args: &[&str]) -> Result<()> {
    let (_, output) = codex_output(codex_dir, args).map_err(|_| {
        Error::invalid(
            "找不到可运行的 Codex CLI；MCP 配置已保留，安装 Codex 后重试 chromie hook install",
        )
    })?;
    if !output.status.success() {
        return Err(Error::invalid(
            "Codex 插件注册未完成；MCP 配置已保留，可重试 chromie hook install。未输出客户端配置或错误原文",
        ));
    }
    Ok(())
}

/// 插件只带固定 hook 与采集程序，不携带 MCP／hook 密钥，不修改信任记录。
pub fn install_hook(config: &Path, codex_dir: &Path, data_dir: &Path) -> Result<Value> {
    #[cfg(not(unix))]
    return Err(Error::invalid(
        "首版自动安装 hook 支持 macOS 与 Linux，其他平台请使用通用接入",
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let connection = client_config::load(KeyKind::Hook, Some(config))?;
        codex_setup::directory(codex_dir, false)?;
        codex_setup::directory(data_dir, true)?;
        let config =
            fs::canonicalize(config).map_err(|_| Error::invalid("无法定位私有接入文件"))?;
        let root = data_dir.join("codex-plugin");
        let plugin = root.join("plugin");
        for dir in [
            root.join(".agents/plugins"),
            plugin.join(".codex-plugin"),
            plugin.join("hooks"),
            plugin.join("bin"),
            plugin.join("scripts"),
        ] {
            codex_setup::directory(&dir, true)?;
        }
        let marketplace = json!({"name":"chromie-cli","interface":{"displayName":"Chromie CLI"},"plugins":[{
            "name":"chromie","source":{"source":"local","path":"./plugin"},"policy":{"installation":"AVAILABLE","authentication":"ON_USE"},"category":"Productivity"}]});
        let manifest = json!({"name":"chromie","version":env!("CARGO_PKG_VERSION"),"description":"Chromie CLI 安装的消息采集 hook","hooks":"./hooks/hooks.json"});
        for (path, value) in [
            (root.join(".agents/plugins/marketplace.json"), marketplace),
            (plugin.join(".codex-plugin/plugin.json"), manifest),
        ] {
            codex_setup::replace(
                &path,
                &serde_json::to_vec_pretty(&value).map_err(|_| Error::unavailable())?,
            )?;
        }
        // 复制本次 CLI 到固定插件目录；更新全局 CLI 后重新安装会更新它。
        let executable = std::env::current_exe().map_err(|_| Error::unavailable())?;
        let binary = plugin.join("bin/chromie");
        codex_setup::replace(
            &binary,
            &fs::read(executable).map_err(|_| Error::unavailable())?,
        )?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::unavailable())?;
        let config_text = config
            .to_str()
            .ok_or_else(|| Error::invalid("私有配置路径须为 UTF-8"))?;
        let queue = data_dir.join("queue");
        let queue_text = queue
            .to_str()
            .ok_or_else(|| Error::invalid("采集目录路径须为 UTF-8"))?;
        let script = format!(
            "#!/bin/sh\nset -eu\n# 凭据只由采集程序从私有文件读取。\nCHROMIE_QUEUE_DIR={}\nexport CHROMIE_QUEUE_DIR\nexec \"${{PLUGIN_ROOT}}/bin/chromie\" capture --config {}\n",
            shell_quote(queue_text),
            shell_quote(config_text)
        );
        let script_path = plugin.join("scripts/capture.sh");
        codex_setup::replace(&script_path, script.as_bytes())?;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::unavailable())?;
        codex_setup::replace(
            &plugin.join("hooks/hooks.json"),
            include_bytes!("../integrations/codex/plugin/hooks/hooks.json"),
        )?;
        let root_text = root
            .to_str()
            .ok_or_else(|| Error::invalid("插件路径须为 UTF-8"))?;
        invoke_codex(codex_dir, &["plugin", "marketplace", "add", root_text])?;
        invoke_codex(
            codex_dir,
            &["plugin", "add", "chromie@chromie-cli", "--json"],
        )?;
        Ok(
            json!({"installed":true,"agent_id":connection.agent_id,"plugin":"chromie@chromie-cli","plugin_root":root,
            "native_hook_verified":false,"next":"请先重载或重启 Codex，或开启新的本地会话，再到 /hooks 审核信任 Chromie hook；旧手工 hook 或 chromie-local 插件请先停用以免重复采集。"}),
        )
    }
}

async fn response(mut response: reqwest::Response) -> Result<Value> {
    if !response.status().is_success() {
        return Err(Error::invalid(
            "接入请求未成功；请检查地址、确认是否过期，必要时重新运行 init",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
        if bytes.len() + chunk.len() > 8192 {
            return Err(Error::invalid("接入响应超过上限"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::invalid("接入响应格式无效"))
}

fn address(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw).map_err(|_| Error::invalid("服务地址无效"))?;
    if !crate::transport::allows(&url)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::invalid(
            "服务地址须为 HTTP 或 HTTPS Origin，不能含路径、凭据、查询或片段",
        ));
    }
    Ok(raw.trim_end_matches('/').into())
}

async fn pair(options: &Options, data: &Path) -> Result<PathBuf> {
    let url = address(
        options
            .url
            .as_deref()
            .ok_or_else(|| Error::invalid("init 须提供 --url 服务地址"))?,
    )?;
    let space = options
        .space
        .as_deref()
        .and_then(SpaceKey::parse)
        .ok_or_else(|| Error::invalid("init 须提供 --space 空间短标识"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::unavailable())?;
    let prefix = format!("{url}/spaces/{}/cli/pairings", space.as_str());
    let started = response(
        client
            .post(&prefix)
            .json(&json!({"client":if options.generic {"generic"} else {"codex"}}))
            .send()
            .await
            .map_err(|_| Error::invalid("无法连接 Chromie 服务"))?,
    )
    .await?;
    let id = started["pairing_id"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(Error::unavailable)?;
    let key = Zeroizing::new(
        started["device_key"]
            .as_str()
            .ok_or_else(Error::unavailable)?
            .to_owned(),
    );
    let code = started["code"].as_str().ok_or_else(Error::unavailable)?;
    let link = format!("{url}/spaces/{}/access?pair={id}", space.as_str());
    println!(
        "请在浏览器登录空间，核对确认码 {code}，选择本机的 Agent 接入并确认：\n{link}\n等待浏览器确认（最多五分钟）……"
    );
    if !options.no_browser {
        let launcher = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = Command::new(launcher)
            .arg(&link)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    let expires = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() >= expires {
            return Err(Error::invalid("浏览器确认已超时，请重新运行 init"));
        }
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.as_str()))
            .map_err(|_| Error::unavailable())?;
        auth.set_sensitive(true);
        let value = response(
            client
                .post(format!("{prefix}/{id}"))
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .map_err(|_| Error::invalid("接入领取中断，请重新运行 init"))?,
        )
        .await?;
        match value["status"].as_str() {
            Some("pending") => tokio::time::sleep(Duration::from_secs(2)).await,
            Some("ready") => {
                let config = Zeroizing::new(serde_json::to_vec_pretty(&json!({"url":url,"space":space.as_str(),"agent_id":value["agent_id"],"mcp_key":value["mcp_key"],"hook_key":value["hook_key"]})).map_err(|_| Error::unavailable())?);
                let path = data.join(format!(".pairing-{id}.json"));
                codex_setup::create_private(&path, &config)?;
                return Ok(path);
            }
            _ => return Err(Error::invalid("接入领取返回了未知状态")),
        }
    }
}

struct TemporaryConfig(Option<PathBuf>);
impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn confirm_replacement() -> Result<()> {
    use std::io::{BufRead, IsTerminal, Read, Write};
    if !std::io::stdin().is_terminal() {
        return Err(Error::invalid(
            "检测到已有 Chromie 配置；非交互运行须显式 --replace，现有配置未修改，也未启动新的浏览器授权",
        ));
    }
    eprintln!(
        "检测到已有 Chromie 接入配置。继续会按本次选择更新本机接入；涉及 Codex 时会备份原配置并只修改 Chromie 项。"
    );
    eprint!("是否继续？[y/N] ");
    std::io::stderr()
        .flush()
        .map_err(|_| Error::unavailable())?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .take(64)
        .read_line(&mut answer)
        .map_err(|_| Error::invalid("无法读取确认，现有配置未修改"))?;
    if !matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "是" | "确认"
    ) {
        return Err(Error {
            code: "cancelled",
            message: "已取消初始化，现有配置未修改",
            status: reqwest::StatusCode::BAD_REQUEST,
        });
    }
    Ok(())
}

fn saved_connection(path: &Path) -> Result<Option<crate::client_config::ClientConnection>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let mcp = client_config::load(KeyKind::Mcp, Some(path))?;
            client_config::load(KeyKind::Hook, Some(path))?;
            Ok(Some(mcp))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::invalid("无法检查本机接入文件")),
    }
}

pub async fn run(command: Action, mut options: Options) -> Result<Value> {
    if options.config.is_some()
        && (options.url.is_some() || options.space.is_some() || options.reconnect)
    {
        return Err(Error::invalid("本机文件导入与浏览器配对不能同时指定"));
    }
    let data = options.data_dir.clone().map_or_else(
        || {
            codex_setup::default_data_dir().map(|dir| {
                if options.generic {
                    dir.with_file_name("generic")
                } else {
                    dir
                }
            })
        },
        Ok,
    )?;
    let codex = options
        .codex_dir
        .clone()
        .map_or_else(codex_setup::default_codex_dir, Ok)?;
    if command != Action::Init && (options.url.is_some() || options.space.is_some()) {
        return Err(Error::invalid("服务地址与空间仅用于 init"));
    }
    if command == Action::Doctor {
        return crate::codex_check::run_with_context(
            &options.config.unwrap_or_else(|| data.join("client.json")),
            options.test_capture,
            Some(&codex),
            !options.generic,
        )
        .await;
    }
    if command == Action::HookInstall {
        return install_hook(
            &options.config.unwrap_or_else(|| data.join("client.json")),
            &codex,
            &data,
        );
    }
    let client_path = data.join("client.json");
    let saved = saved_connection(&client_path)?;
    let mut reused = false;
    if options.config.is_none() {
        if options.url.is_none()
            && options.space.is_none()
            && let Some(saved) = &saved
        {
            options.url = Some(saved.base_url.clone());
            options.space = Some(saved.space_id.clone());
        }
        let url = address(options.url.as_deref().ok_or_else(|| {
            Error::invalid("首次 init 须提供 --url 和 --space；已有连接可直接运行 init")
        })?)?;
        let space = options
            .space
            .as_deref()
            .and_then(SpaceKey::parse)
            .ok_or_else(|| Error::invalid("init 须提供有效空间短标识"))?;
        if !options.reconnect
            && !options.replace
            && saved
                .as_ref()
                .is_some_and(|old| old.base_url == url && old.space_id == space.as_str())
        {
            options.config = Some(client_path.clone());
            reused = true;
        }
    }
    let needs_replacement = if options.generic {
        match (&saved, options.config.as_deref()) {
            (Some(old), Some(source)) => {
                let next = client_config::load(KeyKind::Mcp, Some(source))?;
                old.agent_id != next.agent_id
                    || old.base_url != next.base_url
                    || old.space_id != next.space_id
            }
            (Some(_), None) => true,
            _ => false,
        }
    } else {
        codex_setup::needs_replacement(options.config.as_deref(), &codex, &data)?
    };
    let mut replace = options.replace;
    if needs_replacement && !replace {
        confirm_replacement()?;
        replace = true;
    }
    if reused {
        println!("已找到同一服务和空间的本机接入，复用当前 Agent，检查凭据与配置……");
        crate::codex_check::run_with_context(&client_path, false, Some(&codex), !options.generic)
            .await
            .inspect_err(|_| {
                eprintln!("如需重新选择接入或更新凭据，运行 init --reconnect。");
            })?;
    }
    codex_setup::directory(&data, true)?;
    let temporary = TemporaryConfig(if options.config.is_none() {
        Some(pair(&options, &data).await?)
    } else {
        None
    });
    let source = options
        .config
        .as_deref()
        .or(temporary.0.as_deref())
        .ok_or_else(Error::unavailable)?;
    let configured = if options.generic {
        let mcp = client_config::load(KeyKind::Mcp, Some(source))?;
        client_config::load(KeyKind::Hook, Some(source))?;
        let destination = data.join("client.json");
        if destination.exists() {
            let old = client_config::load(KeyKind::Mcp, Some(&destination))?;
            if !replace
                && (old.agent_id != mcp.agent_id
                    || old.space_id != mcp.space_id
                    || old.base_url != mcp.base_url)
            {
                confirm_replacement()?;
            }
        }
        codex_setup::replace(
            &destination,
            &Zeroizing::new(fs::read(source).map_err(|_| Error::unavailable())?),
        )?;
        json!({"configured":true,"client_config":destination,"agent_id":mcp.agent_id,"next":"在客户端配置通用 MCP；hook 调用 chromie capture --config 本机私有配置路径"})
    } else {
        match codex_setup::run(source, &codex, &data, replace) {
            Err(error) if error.code == "local_connection_conflict" && !replace => {
                confirm_replacement()?;
                codex_setup::run(source, &codex, &data, true)?
            }
            result => result?,
        }
    };
    let hook = if options.generic {
        json!({"installed":false,"manual_adapter_required":true})
    } else {
        install_hook(&data.join("client.json"), &codex, &data)?
    };
    let checked =
        crate::codex_check::run_with_context(&client_path, false, Some(&codex), !options.generic)
            .await?;
    Ok(json!({"reused":reused,"configuration":configured,"hook":hook,"check":checked}))
}
