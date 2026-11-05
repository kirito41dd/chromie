use crate::{
    agent_key::KeyKind,
    client_config::{self, ClientConnection},
    error::{Error, Result},
    types::{now, timestamp},
};
use reqwest::{Client, header};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use uuid::Uuid;

fn authorization(key: &str) -> Result<header::HeaderValue> {
    let mut value = header::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| Error::invalid("接入密钥格式无效"))?;
    value.set_sensitive(true);
    Ok(value)
}

async fn rpc(client: &Client, connection: &ClientConnection, body: Value) -> Result<Value> {
    let mut response = client
        .post(format!(
            "{}/spaces/{}/mcp",
            connection.base_url, connection.space_id
        ))
        .header(header::AUTHORIZATION, authorization(&connection.key)?)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&body)
        .send()
        .await
        .map_err(|_| Error::invalid("无法连接 MCP，请检查地址和网络"))?;
    if !response.status().is_success() {
        return Err(Error::invalid(
            "MCP 初始化或工具发现失败，请检查接入启停、MCP 密钥及客户端重载状态",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
        if bytes.len() + chunk.len() > 131072 {
            return Err(Error::invalid("MCP 诊断响应超过上限"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("MCP 诊断未收到有效 JSON 响应"))?;
    if value.get("error").is_some() {
        return Err(Error::invalid("MCP 返回协议错误，未输出响应内容"));
    }
    Ok(value)
}

/// 只输出连接元数据；测试事件与真实客户端 hook 状态明确区分。
pub async fn run(path: &Path, test_capture: bool) -> Result<Value> {
    run_with_context(path, test_capture, None, true).await
}

pub async fn run_with_context(
    path: &Path,
    test_capture: bool,
    codex_dir: Option<&Path>,
    inspect_hooks: bool,
) -> Result<Value> {
    let connection = client_config::load(KeyKind::Mcp, Some(path))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::unavailable())?;
    let initialized = rpc(&client, &connection, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"chromie-codex-setup","version":env!("CARGO_PKG_VERSION")}
    }})).await?;
    if initialized["result"]["protocolVersion"] != "2025-06-18" {
        return Err(Error::invalid("MCP 未接受诊断客户端的协议版本"));
    }
    client
        .post(format!(
            "{}/spaces/{}/mcp",
            connection.base_url, connection.space_id
        ))
        .header(header::AUTHORIZATION, authorization(&connection.key)?)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .map_err(|_| Error::invalid("MCP 初始化通知未发送成功"))?
        .error_for_status()
        .map_err(|_| Error::invalid("MCP 初始化通知被拒绝"))?;
    let listed = rpc(
        &client,
        &connection,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await?;
    let tools = listed["result"]["tools"]
        .as_array()
        .ok_or_else(|| Error::invalid("MCP 未返回工具列表"))?;
    for name in [
        "recall",
        "get_memory",
        "remember",
        "correct_memory",
        "forget_memory",
        "get_setup_guide",
    ] {
        if !tools.iter().any(|tool| tool["name"] == name) {
            return Err(Error::invalid("MCP 工具不完整，请核对 Chromie 服务版本"));
        }
    }
    let mut capture = json!({"requested":false});
    if test_capture {
        let hook = client_config::load(KeyKind::Hook, Some(path))?;
        if hook.agent_id != connection.agent_id
            || hook.base_url != connection.base_url
            || hook.space_id != connection.space_id
        {
            return Err(Error::invalid("检查过程中本机接入配置发生变化，请重新检查"));
        }
        let event = Uuid::now_v7();
        let response = client.post(format!("{}/spaces/{}/events", hook.base_url, hook.space_id))
            .header(header::AUTHORIZATION, authorization(&hook.key)?)
            .header("x-chromie-agent-id", hook.agent_id.to_string())
            .json(&json!({"id":event,"client_type":"codex","session_id":format!("chromie-setup-{event}"),
                "role":"user","body":"Chromie 接入检查：这是一条虚构测试消息，仅验证上传链路，不表示真实 Codex hook 已触发。",
                "project_key":"chromie-setup","occurred_at":timestamp(now())?}))
            .send().await.map_err(|_| Error::invalid("测试事件上传失败，请检查网络"))?;
        if !response.status().is_success() {
            return Err(Error::invalid(
                "测试事件被拒绝，请检查独立的 hook 上传密钥及 Agent ID",
            ));
        }
        capture = json!({"requested":true,"uploaded":true,"synthetic":true,"event_id":event});
    }
    let hook = if inspect_hooks {
        let directory = codex_dir
            .map(std::path::PathBuf::from)
            .map_or_else(crate::codex_setup::default_codex_dir, Ok)?;
        installation_status(&directory)
    } else {
        json!({"status":"not_applicable"})
    };
    let next = next_step(&hook);
    Ok(
        json!({"space":connection.space_id,"agent_id":connection.agent_id,"mcp":{"connected":true,"tool_count":tools.len()},
        "capture_test":capture,"native_hook_verified":false,"hook":hook,"next":next,
        "client":{"path":std::env::current_exe().ok(),"version":env!("CARGO_PKG_VERSION"),
            "capability_version":1,"capabilities":{"secret_exec":1,"secret_access_audit":1}}}),
    )
}

fn feature_status(text: &str) -> Value {
    let mut result = serde_json::Map::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(name) = fields.first() else {
            continue;
        };
        if !matches!(*name, "hooks" | "codex_hooks" | "plugin_hooks") || fields.len() < 3 {
            continue;
        }
        let enabled = match fields.last().copied() {
            Some("true") => true,
            Some("false") => false,
            _ => continue,
        };
        let stage = fields
            .iter()
            .skip(1)
            .take(fields.len() - 2)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        result.insert((*name).into(), json!({"enabled":enabled,"stage":stage}));
    }
    Value::Object(result)
}

fn read_definition(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(65537).read_to_end(&mut bytes).is_err() || bytes.len() > 65536 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    ["UserPromptSubmit", "Stop"].iter().all(|name| {
        value["hooks"][name].as_array().is_some_and(|groups| {
            groups.iter().any(|group| {
                group["hooks"].as_array().is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook["type"] == "command"
                            && hook["command"]
                                .as_str()
                                .is_some_and(|command| !command.trim().is_empty())
                    })
                })
            })
        })
    })
}

fn installation_status(codex_dir: &Path) -> Value {
    let version = crate::cli_setup::codex_output(codex_dir, &["--version"]);
    let Ok((program, version)) = version else {
        return json!({"status":"cli_unavailable","codex_home":codex_dir});
    };
    let text = String::from_utf8_lossy(&version.stdout);
    let mut words = text.split_whitespace();
    let version = match (words.next(), words.next()) {
        (Some("codex-cli"), Some(value))
            if value.len() < 80
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte)) =>
        {
            format!("codex-cli {value}")
        }
        _ => "版本未知".into(),
    };
    let features = crate::cli_setup::codex_output(codex_dir, &["features", "list"])
        .ok()
        .filter(|(_, output)| output.status.success())
        .map(|(_, output)| feature_status(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_else(|| json!({}));
    let listed = crate::cli_setup::codex_output(
        codex_dir,
        &["plugin", "list", "--marketplace", "chromie-cli", "--json"],
    )
    .ok()
    .filter(|(_, output)| output.status.success() && output.stdout.len() <= 262144)
    .and_then(|(_, output)| serde_json::from_slice::<Value>(&output.stdout).ok());
    installation_report(codex_dir, &program, &version, features, listed.as_ref())
}

fn installation_report(
    codex_dir: &Path,
    program: &Path,
    version: &str,
    features: Value,
    listed: Option<&Value>,
) -> Value {
    let plugin = listed
        .and_then(|value| value["installed"].as_array())
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| plugin["pluginId"] == "chromie@chromie-cli")
        });
    let installed = plugin.is_some_and(|plugin| plugin["installed"] == true);
    let enabled = plugin.is_some_and(|plugin| plugin["enabled"] == true);
    let source = plugin
        .and_then(|plugin| plugin["source"]["path"].as_str())
        .map(std::path::PathBuf::from);
    let cache = plugin
        .and_then(|plugin| plugin["version"].as_str())
        .filter(|version| {
            !version.is_empty()
                && *version != "."
                && *version != ".."
                && version.len() < 100
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
        .map(|version| {
            codex_dir
                .join("plugins/cache/chromie-cli/chromie")
                .join(version)
        });
    let source_definition = source
        .as_ref()
        .is_some_and(|root| read_definition(&root.join("hooks/hooks.json")));
    let cached_definition = cache
        .as_ref()
        .is_some_and(|root| read_definition(&root.join("hooks/hooks.json")));
    let disabled = ["hooks", "codex_hooks", "plugin_hooks"]
        .iter()
        .any(|name| features[name]["enabled"] == false && features[name]["stage"] != "removed");
    let status = if listed.is_none() {
        "plugin_query_failed"
    } else if !installed {
        "not_installed"
    } else if !enabled {
        "plugin_disabled"
    } else if disabled {
        "hooks_disabled"
    } else if !source_definition && !cached_definition {
        "definition_missing"
    } else {
        "installed_runtime_unverified"
    };
    json!({"status":status,"cli_path":program,"cli_version":version,"codex_home":codex_dir,"plugin_installed":installed,"plugin_enabled":enabled,"features":features,"source_root":source,"cache_root":cache,"source_definition_present":source_definition,"cached_definition_present":cached_definition})
}

fn next_step(hook: &Value) -> &'static str {
    match hook["status"].as_str() {
        Some("not_applicable") => {
            "通用接入需在所用 Agent 中配置 hook 调用 chromie capture；测试上传不代表真实 hook 已触发。"
        }
        Some("cli_unavailable") => {
            "未找到 Codex CLI，请核对安装位置或终端 PATH，再运行 chromie hook install。"
        }
        Some("not_installed" | "definition_missing") => {
            "当前 Codex 配置目录未发现完整的 Chromie hook，请运行 chromie hook install；桌面版和终端须使用同一 CODEX_HOME。"
        }
        Some("plugin_disabled") => {
            "Chromie 插件已安装但未启用，请在当前 Codex 的插件页面启用，再到 /hooks 审核信任。"
        }
        Some("hooks_disabled") => {
            "当前 Codex 的 hooks 或旧版 plugin_hooks 功能未启用，请核对 features 输出及桌面版版本；安装插件不会自动启用被禁用的功能。"
        }
        Some("plugin_query_failed") => {
            "无法读取当前 Codex 的插件状态，请核对 CLI 版本与 CODEX_HOME；MCP 连接成功不表示 hook 已装好。"
        }
        _ => {
            "插件与 hook 文件已发现；重载桌面版或开启新会话，在 /hooks 审核信任后发送新消息，再核对 Web 来源与上传时间。测试上传不代表真实 hook 已触发。"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn installed_enabled_definitions_and_old_feature_gates_are_independent() {
        let root = std::env::temp_dir().join(format!("chromie-doctor-{}", Uuid::now_v7()));
        let plugin = root.join("source");
        fs::create_dir_all(plugin.join("hooks")).unwrap();
        let marker = "fixture-private-value-must-not-be-shown";
        fs::write(plugin.join("hooks/hooks.json"), json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":marker}]}],"Stop":[{"hooks":[{"type":"command","command":marker}]}]}}).to_string()).unwrap();
        let installed = json!({"installed":[{"pluginId":"chromie@chromie-cli","installed":true,"enabled":true,"version":"0.1.0","source":{"path":plugin},"private":marker}]});
        let modern = feature_status("hooks stable true\nplugin_hooks removed false\n");
        let report = installation_report(
            &root,
            Path::new("/fixture/codex"),
            "codex-cli 0.165.0",
            modern,
            Some(&installed),
        );
        assert_eq!(report["status"], "installed_runtime_unverified");
        assert_eq!(report["source_definition_present"], true);
        assert_eq!(report["cached_definition_present"], false);
        assert!(!report.to_string().contains(marker));
        assert!(next_step(&report).contains("重载"));
        let older =
            feature_status("codex_hooks experimental true\nplugin_hooks under development false\n");
        let report = installation_report(
            &root,
            Path::new("/fixture/codex"),
            "codex-cli 0.144.1",
            older,
            Some(&installed),
        );
        assert_eq!(report["status"], "hooks_disabled");
        assert_eq!(
            report["features"]["plugin_hooks"]["stage"],
            "under development"
        );
        let mut disabled = installed.clone();
        disabled["installed"][0]["enabled"] = json!(false);
        assert_eq!(
            installation_report(
                &root,
                Path::new("/fixture/codex"),
                "fixture",
                json!({}),
                Some(&disabled)
            )["status"],
            "plugin_disabled"
        );
        fs::remove_file(plugin.join("hooks/hooks.json")).unwrap();
        assert_eq!(
            installation_report(
                &root,
                Path::new("/fixture/codex"),
                "fixture",
                json!({}),
                Some(&installed)
            )["status"],
            "definition_missing"
        );
        let missing = installation_report(
            &root,
            Path::new("/fixture/codex"),
            "fixture",
            json!({}),
            Some(&json!({"installed":[]})),
        );
        assert_eq!(missing["status"], "not_installed");
        assert!(next_step(&missing).contains("chromie hook install"));
        fs::remove_dir_all(root).unwrap();
    }
}
