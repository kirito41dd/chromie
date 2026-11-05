#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};
use uuid::Uuid;

#[test]
fn conflicts_and_invalid_toml_are_reported_before_browser_authorization_or_writes() {
    for invalid in [false, true] {
        let root = std::env::temp_dir().join(format!("chromie-cli-preflight-{}", Uuid::now_v7()));
        let codex = root.join("codex");
        let data = root.join("data");
        fs::create_dir_all(&codex).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let original = if invalid {
            "broken = ["
        } else {
            "# 保留用户配置\n[mcp_servers.chromie]\nurl='https://different.example/mcp'\n"
        };
        fs::write(codex.join("config.toml"), original).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_chromie"))
            .args([
                "init",
                "--url",
                "http://127.0.0.1:9",
                "--space",
                "demo",
                "--no-browser",
                "--codex-dir",
            ])
            .arg(&codex)
            .arg("--data-dir")
            .arg(&data)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(if invalid {
            error.contains("TOML 无效")
        } else {
            error.contains("非交互") && error.contains("--replace")
        });
        assert!(!String::from_utf8_lossy(&output.stdout).contains("请在浏览器"));
        assert!(!data.exists());
        assert_eq!(
            fs::read_to_string(codex.join("config.toml")).unwrap(),
            original
        );
        assert_eq!(fs::read_dir(&codex).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore = "需要本机已安装的 Nushell"]
fn nushell_generated_path_and_capture_commands_work_with_real_stdin_and_scoped_environment() {
    use chromie::shell_commands::Shell;
    use serde_json::{Value, json};
    let root = std::env::temp_dir().join(format!("chromie-nu-capture-{}", Uuid::now_v7()));
    fs::create_dir_all(root.join(".local/bin")).unwrap();
    fs::create_dir_all(root.join(".config/chromie/generic")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_chromie"),
        root.join(".local/bin/chromie"),
    )
    .unwrap();
    fs::set_permissions(
        root.join(".local/bin/chromie"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let id = Uuid::now_v7();
    let config = root.join(".config/chromie/generic/client.json");
    fs::write(&config,serde_json::to_vec(&json!({"url":"http://127.0.0.1:9","space":"demo","agent_id":id,"mcp_key":format!("cm_mcp_{}","a".repeat(64)),"hook_key":format!("cm_hook_{}","b".repeat(64))})).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let message =
        json!({"session_id":"fictional-nushell","role":"user","body":"虚构 Nushell 采集验证"})
            .to_string();
    let code = format!(
        "{}; {}; if (($env.PATH | first) != ($nu.home-dir | path join '.local' 'bin')) {{error make {{msg: 'PATH 未设置'}}}}; {} | {}",
        Shell::Nushell.path_line(),
        Shell::Nushell.path_line(),
        serde_json::to_string(&message).unwrap(),
        Shell::Nushell.capture(false)
    );
    let result = Command::new("nu")
        .args(["--no-config-file", "-c", &code])
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Nushell 命令执行失败：{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        json!({})
    );
    let queue = root.join(".config/chromie/generic/queue");
    let queued: Vec<_> = fs::read_dir(queue)
        .unwrap()
        .map(|item| item.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    assert_eq!(queued.len(), 1);
    let body = fs::read_to_string(&queued[0]).unwrap();
    assert!(body.contains("虚构 Nushell 采集验证") && body.contains(&id.to_string()));
    assert!(!body.contains("cm_mcp_") && !body.contains("cm_hook_"));
    fs::remove_dir_all(root).unwrap();
}
