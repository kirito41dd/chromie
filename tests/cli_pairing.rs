#![cfg(unix)]

use chromie::{
    agent::{CreateAgent, KeyKind, UpdateAgent},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    types::SpaceId,
    web_session::WebSessions,
};
use migration::MigratorTrait;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::Duration,
};
use uuid::Uuid;
#[allow(dead_code)]
mod support;

const PASSWORD: &str = "fictional-cli-pairing-password";

async fn fixture() -> (Store, SpaceId, String, String, tokio::task::JoinHandle<()>) {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = sea_orm::ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = sea_orm::Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new(
            &std::env::var("CHROMIE_QDRANT_URL").unwrap(),
            std::env::var("CHROMIE_QDRANT_API_KEY").ok(),
        )
        .unwrap(),
    ))
    .await;
    let suffix: String = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .rev()
        .take(20)
        .map(|c| char::from(b'a' + c.to_digit(16).unwrap() as u8))
        .collect();
    let slug = SpaceKey::parse(&format!("cli-{suffix}")).unwrap();
    store
        .create_space(
            slug.clone(),
            format!("虚构 CLI 接入测试-{suffix}"),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let space = store.resolve_space(&slug).await.unwrap();
    store
        .create_space(
            SpaceKey::parse(&format!("other-{suffix}")).unwrap(),
            format!("虚构其他配对空间-{suffix}"),
            PASSWORD.into(),
        )
        .await
        .unwrap();
    let router = chromie::http::router_with_sessions(
        store.clone(),
        None,
        WebSessions::from_key(&support::root()).unwrap(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (store, space, slug.as_str().into(), url, task)
}

async fn start(client: &Client, base: &str, slug: &str) -> Value {
    client
        .post(format!("{base}/spaces/{slug}/cli/pairings"))
        .json(&json!({"client":"codex"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn cookie(client: &Client, base: &str, slug: &str) -> String {
    client
        .post(format!("{base}/spaces/{slug}/login"))
        .header("x-chromie-password", PASSWORD)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
async fn approve(
    client: &Client,
    base: &str,
    slug: &str,
    pair: &Value,
    cookie: &str,
    agent: Uuid,
) -> reqwest::Response {
    client
        .post(format!(
            "{base}/spaces/{slug}/agents/pairings/{}",
            pair["pairing_id"].as_str().unwrap()
        ))
        .header("cookie", cookie)
        .json(&json!({"agent_id":agent,"code":pair["code"]}))
        .send()
        .await
        .unwrap()
}
async fn poll(client: &Client, base: &str, slug: &str, pair: &Value) -> reqwest::Response {
    client
        .post(format!(
            "{base}/spaces/{slug}/cli/pairings/{}",
            pair["pairing_id"].as_str().unwrap()
        ))
        .bearer_auth(pair["device_key"].as_str().unwrap())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn pairing_requires_browser_cookie_explicit_approval_and_one_time_device_credential() {
    let (store, space, slug, base, task) = fixture().await;
    let agent = store
        .create_agent(
            space,
            CreateAgent {
                agent_name: "虚构 CLI 电脑".into(),
            },
        )
        .await
        .unwrap()
        .agent_id;
    let client = Client::new();
    let pair = start(&client, &base, &slug).await;
    assert_eq!(
        poll(&client, &base, &slug, &pair)
            .await
            .json::<Value>()
            .await
            .unwrap()["status"],
        "pending"
    );
    let path = format!(
        "{base}/spaces/{slug}/agents/pairings/{}",
        pair["pairing_id"].as_str().unwrap()
    );
    assert_eq!(
        client
            .get(&path)
            .header("x-chromie-password", PASSWORD)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(&path)
            .json(&json!({"agent_id":agent,"code":pair["code"]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let session = cookie(&client, &base, &slug).await;
    let other_slug = format!("other-{}", slug.strip_prefix("cli-").unwrap());
    let other_cookie = cookie(&client, &base, &other_slug).await;
    let other_path = format!(
        "{base}/spaces/{other_slug}/agents/pairings/{}",
        pair["pairing_id"].as_str().unwrap()
    );
    assert_eq!(
        client
            .get(&path)
            .header("cookie", &other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&other_path)
            .header("cookie", &other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let other_space = store
        .resolve_space(&SpaceKey::parse(&other_slug).unwrap())
        .await
        .unwrap();
    let foreign_agent = store
        .create_agent(
            other_space,
            CreateAgent {
                agent_name: "虚构其他空间接入".into(),
            },
        )
        .await
        .unwrap()
        .agent_id;
    assert_eq!(
        approve(&client, &base, &slug, &pair, &session, foreign_agent)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .post(&path)
            .header("cookie", &session)
            .json(&json!({"agent_id":agent,"code":"wrong-confirmation-code"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let preview = client
        .get(&path)
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(preview["code"], pair["code"]);
    assert!(
        preview["expires_in"]
            .as_u64()
            .is_some_and(|n| n > 0 && n <= 300)
    );
    assert!(preview["agent_id"].is_null());
    assert!(
        !preview.to_string().contains("device_key") && !preview.to_string().contains("cm_mcp_")
    );
    assert_eq!(
        client
            .post(&path)
            .header("cookie", &session)
            .header("origin", "https://evil.invalid")
            .json(&json!({"agent_id":agent,"code":pair["code"]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let foreign = Uuid::now_v7();
    assert_eq!(
        approve(&client, &base, &slug, &pair, &session, foreign)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        approve(&client, &base, &slug, &pair, &session, agent)
            .await
            .status()
            .is_success()
    );
    assert_eq!(
        approve(&client, &base, &slug, &pair, &session, agent)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let public = format!(
        "{base}/spaces/{slug}/cli/pairings/{}",
        pair["pairing_id"].as_str().unwrap()
    );
    assert_eq!(
        client
            .post(&public)
            .bearer_auth("wrong-device-key")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(&public)
            .header("cookie", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // 批准后轮换，领取必须得到当前值，不能缓存旧密钥。
    store
        .rotate_agent_key(space, agent, KeyKind::Hook)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        poll(&client, &base, &slug, &pair),
        poll(&client, &base, &slug, &pair)
    );
    let (ok, used) = if a.status().is_success() {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(used.status(), StatusCode::NOT_FOUND);
    let result = ok.json::<Value>().await.unwrap();
    assert_eq!(result["agent_id"], agent.to_string());
    assert_eq!(
        result["hook_key"],
        store
            .reveal_agent_key(space, agent, KeyKind::Hook)
            .await
            .unwrap()
            .as_str()
    );
    assert!(result["mcp_key"].as_str().unwrap().starts_with("cm_mcp_"));
    assert_eq!(
        poll(&client, &base, &slug, &pair).await.status(),
        StatusCode::NOT_FOUND
    );
    task.abort();
}

#[tokio::test]
#[ignore = "需要真实隔离 MySQL；见开发运行文档"]
async fn disabled_agents_and_changed_space_password_cancel_pending_credential_delivery() {
    let (store, space, slug, base, task) = fixture().await;
    let agent = store
        .create_agent(
            space,
            CreateAgent {
                agent_name: "虚构撤销接入".into(),
            },
        )
        .await
        .unwrap()
        .agent_id;
    let client = Client::new();
    let session = cookie(&client, &base, &slug).await;
    let pair = start(&client, &base, &slug).await;
    approve(&client, &base, &slug, &pair, &session, agent)
        .await
        .error_for_status()
        .unwrap();
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: Some(false),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        poll(&client, &base, &slug, &pair).await.status(),
        StatusCode::UNAUTHORIZED
    );
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: Some(true),
                allow_secrets: None,
            },
        )
        .await
        .unwrap();
    let pair = start(&client, &base, &slug).await;
    approve(&client, &base, &slug, &pair, &session, agent)
        .await
        .error_for_status()
        .unwrap();
    store
        .admin_set_password(space.as_uuid(), "fictional-cli-new-password".into())
        .await
        .unwrap();
    assert_eq!(
        poll(&client, &base, &slug, &pair).await.status(),
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要真实隔离 MySQL 与本机 Codex CLI；见开发运行文档"]
async fn native_cli_pairs_installs_codex_hook_repeats_and_uploads_without_echoing_keys() {
    let (store, space, slug, base, task) = fixture().await;
    let agent = store
        .create_agent(
            space,
            CreateAgent {
                agent_name: "虚构自动初始化".into(),
            },
        )
        .await
        .unwrap()
        .agent_id;
    let root = std::env::temp_dir().join(format!("chromie-native-init-{}", Uuid::now_v7()));
    fs::create_dir_all(root.join("codex")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        root.join("codex/config.toml"),
        "# 用户原有配置\n[mcp_servers.other]\nurl = \"https://example.invalid/mcp\"\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_chromie"))
        .args([
            "init",
            "--url",
            &base,
            "--space",
            &slug,
            "--no-browser",
            "--codex-dir",
        ])
        .arg(root.join("codex"))
        .arg("--data-dir")
        .arg(root.join("data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.starts_with("http://") {
                tx.send(line.clone()).unwrap();
            }
            output.push_str(&line);
            output.push('\n');
        }
        output
    });
    let link = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let id = link.split("?pair=").nth(1).unwrap();
    let client = Client::new();
    let session = cookie(&client, &base, &slug).await;
    let preview: Value = client
        .get(format!("{base}/spaces/{slug}/agents/pairings/{id}"))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    approve(&client, &base, &slug, &preview, &session, agent)
        .await
        .error_for_status()
        .unwrap();
    let result = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let output = reader.join().unwrap();
    assert!(
        result.status.success(),
        "CLI 初始化失败：{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !output.contains("cm_mcp_")
            && !output.contains("cm_hook_")
            && !output.contains("device_key")
    );
    let private = root.join("data/client.json");
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let codex = fs::read_to_string(root.join("codex/config.toml")).unwrap();
    assert_eq!(
        fs::metadata(root.join("codex/config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(codex.contains("# 用户原有配置") && codex.contains("mcp_servers.other"));
    assert!(codex.contains("chromie-cli"));
    // 重复初始化默认复用同一服务和空间，不能再次打开浏览器配对。
    let binary = env!("CARGO_BIN_EXE_chromie").to_owned();
    let check_root = root.clone();
    let reuse_url = base.clone();
    let reuse_space = slug.clone();
    let repeated = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(binary)
            .args([
                "init",
                "--url",
                &reuse_url,
                "--space",
                &reuse_space,
                "--no-browser",
                "--codex-dir",
            ])
            .arg(check_root.join("codex"))
            .arg("--data-dir")
            .arg(check_root.join("data"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                panic!("复用连接超时，可能错误地进入了浏览器配对");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
    .await
    .unwrap();
    assert!(
        repeated.status.success(),
        "重复初始化失败，未输出凭据或诊断原文"
    );
    let repeated = String::from_utf8(repeated.stdout).unwrap();
    let final_line: Value = serde_json::from_str(repeated.lines().last().unwrap()).unwrap();
    assert_eq!(final_line["reused"], true);
    assert_eq!(
        final_line["check"]["hook"]["codex_home"],
        root.join("codex").to_string_lossy().as_ref()
    );
    assert_eq!(final_line["check"]["hook"]["plugin_installed"], true);
    assert_eq!(final_line["check"]["hook"]["plugin_enabled"], true);
    assert_eq!(
        final_line["check"]["hook"]["cached_definition_present"],
        true
    );
    assert_eq!(final_line["check"]["native_hook_verified"], false);
    assert!(
        !repeated.contains("?pair=")
            && !repeated.contains("cm_mcp_")
            && !repeated.contains("cm_hook_")
    );
    let relocated = root.join("relocated-client.json");
    fs::copy(&private, &relocated).unwrap();
    fs::set_permissions(&relocated, fs::Permissions::from_mode(0o600)).unwrap();
    for source in [&private, &relocated] {
        let private = source.clone();
        let root = root.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut command=Command::new(env!("CARGO_BIN_EXE_chromie"));
            if cfg!(target_os="macos") && std::path::Path::new("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex").is_file() {
                command.env("PATH","/usr/bin:/bin");
            }
            command.args(["hook", "install", "--config"])
                .arg(private)
                .arg("--codex-dir")
                .arg(root.join("codex"))
                .arg("--data-dir")
                .arg(root.join("data"))
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            result.status.success(),
            "重复 hook 安装失败：{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    // 验证 Codex 实际缓存的插件，不能只验证生成目录里的脚本。
    let plugin = root.join(format!(
        "codex/plugins/cache/chromie-cli/chromie/{}",
        env!("CARGO_PKG_VERSION")
    ));
    let script = plugin.join("scripts/capture.sh");
    assert!(
        fs::read_to_string(&script)
            .unwrap()
            .contains("relocated-client.json"),
        "重复安装须刷新 Codex 缓存中的脚本"
    );
    let plugin_copy = plugin.clone();
    let captured=tokio::task::spawn_blocking(move||{
        use std::io::Write;
        let mut child=Command::new(script).env("PLUGIN_ROOT",plugin_copy).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(r#"{"hook_event_name":"UserPromptSubmit","session_id":"fictional-cli-install","prompt":"虚构 CLI 自动安装采集消息"}"#.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }).await.unwrap();
    assert!(captured.status.success());
    assert!(!String::from_utf8_lossy(&captured.stdout).contains("虚构 CLI"));
    let context: Value = serde_json::from_slice(&captured.stdout).unwrap();
    assert!(
        context["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("get_setup_guide(topic=\"secret\")"),
        "实际插件缓存也须提供 Secret 路由"
    );
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let events = chromie::entity::source_events::Entity::find()
        .filter(chromie::entity::source_events::Column::SpaceId.eq(space.bytes()))
        .all(&store.db)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|v| v.body.as_deref() == Some("虚构 CLI 自动安装采集消息")
                && v.agent_id.as_deref() == Some(agent.as_bytes().as_slice()))
    );
    assert!(!fs::read_dir(root.join("data")).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pairing-")
    }));
    task.abort();
    fs::remove_dir_all(root).unwrap();
}

const PTY_INIT: &str = r#"
import json,os,pty,select,subprocess,sys,time
binary,source,codex,data,reply=sys.argv[1:]
master,slave=pty.openpty()
child=subprocess.Popen([binary,'init','--config',source,'--codex-dir',codex,'--data-dir',data],stdin=slave,stdout=slave,stderr=slave)
os.close(slave)
output=bytearray();answered=False;deadline=time.monotonic()+25
try:
 while time.monotonic()<deadline:
  ready,_,_=select.select([master],[],[],0.1)
  if ready:
   try:chunk=os.read(master,65536)
   except OSError:break
   if not chunk:break
   output.extend(chunk)
   if b'[y/N]' in output and not answered:
    os.write(master,(reply+'\n').encode());answered=True
  if len(output)>131072:raise RuntimeError('output limit')
  if child.poll() is not None and not ready:break
 else:
  child.kill();raise RuntimeError('timeout')
 child.wait(timeout=2)
finally:os.close(master)
print(json.dumps({'exit':child.returncode,'answered':answered,'output':output.decode(errors='replace')}))
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要真实隔离 MySQL、Python 3 与本机 Codex CLI；见开发运行文档"]
async fn interactive_conflict_confirmation_continues_without_repeating_authorization() {
    let (store, space, slug, base, task) = fixture().await;
    let (agent, mcp, hook) = support::grant(&store, space, false).await;
    let root = std::env::temp_dir().join(format!("chromie-init-confirm-{}", Uuid::now_v7()));
    fs::create_dir_all(root.join("codex")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source.json");
    fs::write(
        &source,
        serde_json::to_vec(
            &json!({"url":base,"space":slug,"agent_id":agent,"mcp_key":mcp,"hook_key":hook}),
        )
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let config = root.join("codex/config.toml");
    let original = "# 保留的配置\n[mcp_servers.other]\nurl='https://other.example/mcp'\n[mcp_servers.chromie]\nurl='https://previous.example/mcp'\n";
    fs::write(&config, original).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    for reply in ["", "n", "y"] {
        let test_root = root.clone();
        let source = source.clone();
        let output = tokio::task::spawn_blocking(move || {
            Command::new("python3")
                .args(["-c", PTY_INIT, env!("CARGO_BIN_EXE_chromie")])
                .arg(source)
                .arg(test_root.join("codex"))
                .arg(test_root.join("data"))
                .arg(reply)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(output.status.success(), "PTY 测试辅助程序失败，输出未回显");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["answered"], true);
        let text = result["output"].as_str().unwrap();
        assert!(!text.contains(&mcp) && !text.contains(&hook) && !text.contains("?pair="));
        if reply != "y" {
            assert_ne!(result["exit"], 0);
            assert_eq!(fs::read_to_string(&config).unwrap(), original);
            assert!(!root.join("data").exists());
        } else {
            assert_eq!(result["exit"], 0, "同一进程确认后应继续完成初始化");
            assert!(root.join("data/client.json").is_file());
            let text = fs::read_to_string(&config).unwrap();
            assert!(text.contains("# 保留的配置") && text.contains("mcp_servers.other"));
            assert!(fs::read_dir(root.join("codex")).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.chromie-backup-")
            }));
        }
    }
    task.abort();
    fs::remove_dir_all(root).unwrap();
}
