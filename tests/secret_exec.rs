#![cfg(all(unix, feature = "server", feature = "client"))]
use chromie::{
    agent::UpdateAgent, entity::secret_access_audit, search::Search, space_key::SpaceKey,
    store::Store,
};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database, EntityTrait};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use uuid::Uuid;
#[allow(dead_code)]
mod support;

async fn execute(
    config: &Path,
    tmp: &Path,
    reference: &str,
    revision: i32,
    script: &Path,
    marker: &Path,
    mode: &str,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_chromie"));
    cmd.args(["secret", "exec", "--config"])
        .arg(config)
        .args([
            "--ref",
            reference,
            "--revision",
            &revision.to_string(),
            "--purpose",
            "虚构服务连通性验证",
            "--timeout",
            "1",
            "--",
            "python3",
        ])
        .arg(script)
        .arg(marker)
        .arg(mode)
        .env("TMPDIR", tmp)
        .env("CHROMIE_MCP_KEY", "fixture-child-must-not-inherit");
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

async fn rpc(client: &reqwest::Client, url: &str, key: &str, method: &str, params: Value) -> Value {
    client
        .post(url)
        .bearer_auth(key)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0","id":Uuid::now_v7(),"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要真实隔离 MySQL，见开发运行文档"]
async fn execution_lifecycle_audit_boundaries_and_cached_guide_permissions() {
    let url = std::env::var("CHROMIE_TEST_DATABASE_URL").unwrap();
    assert!(reqwest::Url::parse(&url).unwrap().path().ends_with("_test"));
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    let store = support::enable(Store::new(
        db,
        Search::new("http://127.0.0.1:9", None).unwrap(),
    ))
    .await;
    let slug = format!("exec-{}", Uuid::now_v7().simple())
        .chars()
        .take(28)
        .map(|c| {
            if c.is_ascii_digit() {
                char::from(b'a' + c as u8 - b'0')
            } else {
                c
            }
        })
        .collect::<String>();
    let key = SpaceKey::parse(&slug).unwrap();
    store
        .create_space(
            key.clone(),
            format!("虚构执行-{slug}"),
            "fictional-password".into(),
        )
        .await
        .unwrap();
    let space = store.resolve_space(&key).await.unwrap();
    let (agent, mcp, _) = support::grant(&store, space, true).await;
    let (_, other_mcp, _) = support::grant(&store, space, true).await;
    let router = chromie::http::router(store.clone(), None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let mut latest = None;
    let root = std::env::temp_dir().join(format!("chromie-exec-test-{}", Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.join("client.json");
    private(
        &config,
        &serde_json::to_vec(&json!({"url":base,"space":slug,"agent_id":agent,"mcp_key":mcp}))
            .unwrap(),
    );
    let script = root.join("check.py");
    private(
        &script,
        br#"import os,sys,stat,json,time
p=os.environ['CHROMIE_SECRET_FILE']
assert not any(k.startswith('CHROMIE_') and k!='CHROMIE_SECRET_FILE' for k in os.environ)
assert stat.S_IMODE(os.stat(p).st_mode)==0o600
assert stat.S_IMODE(os.stat(os.path.dirname(p)).st_mode)==0o700
raw=open(p,'rb').read()
assert raw in (b'00123456',b'{"user":"fixture","password":"fictional-json-secret"}')
open(sys.argv[1],'w').write(p)
print(raw.decode(),flush=True)
print(raw.decode(),file=sys.stderr,flush=True)
if sys.argv[2]=='timeout':time.sleep(20)
if sys.argv[2]=='fail':sys.exit(7)
"#,
    );
    for (format, bytes) in [
        ("text", b"00123456".as_slice()),
        (
            "json",
            br#"{"user":"fixture","password":"fictional-json-secret"}"#.as_slice(),
        ),
    ] {
        let contract = if format == "text" {
            json!({"version":1,"format":"text","kind":"password"})
        } else {
            json!({"version":1,"format":"json","fields":{"password":{"pointer":"/password","type":"string"}}})
        };
        let receipt: Value = client.post(format!("{base}/spaces/{slug}/secrets")).bearer_auth(&mcp)
            .header("content-type","application/octet-stream")
            .query(&json!({"request_id":Uuid::now_v7(),"label":"虚构单服务凭据","contract":contract.to_string()}))
            .body(bytes.to_vec()).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let reference = receipt["secret_ref"].as_str().unwrap();
        let marker = root.join("marker");
        for mode in ["ok", "fail", "timeout"] {
            let output = execute(&config, &root, reference, 1, &script, &marker, mode).await;
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(output.status.success(), mode == "ok");
            assert_eq!(
                receipt["outcome"],
                match mode {
                    "ok" => "succeeded",
                    "fail" => "failed",
                    _ => "timed_out",
                }
            );
            assert_eq!(receipt["audit_reported"], true);
            assert_eq!(receipt["temporary_files_removed"], true);
            let path = fs::read_to_string(&marker).unwrap();
            assert!(!Path::new(&path).exists());
            assert!(!Path::new(&path).parent().unwrap().exists());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("fictional-json-secret"));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("00123456"));
            let audit = Uuid::parse_str(receipt["audit_id"].as_str().unwrap()).unwrap();
            let row = secret_access_audit::Entity::find_by_id((
                space.as_uuid().as_bytes().to_vec(),
                audit.as_bytes().to_vec(),
            ))
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
            assert_eq!(row.agent_id, Some(agent.as_bytes().to_vec()));
            assert_eq!(row.revision, 1);
            assert_eq!(row.purpose.as_deref(), Some("虚构服务连通性验证"));
            assert!(row.reported_at.is_some());
            latest = Some(audit);
        }
        fs::remove_file(&marker).unwrap();
        let mismatch = execute(&config, &root, reference, 2, &script, &marker, "ok").await;
        assert!(!mismatch.status.success());
        assert!(!marker.exists());
        let missing = execute(
            &config,
            &root,
            reference,
            1,
            &root.join("missing.py"),
            &marker,
            "ok",
        )
        .await;
        assert!(!missing.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&missing.stdout).unwrap()["temporary_files_removed"],
            true
        );
    }
    // SIGTERM 必须走同一清理路径，而非让进程默认退出后留下文件。
    let reference: String = client
        .post(format!("{base}/spaces/{slug}/secrets"))
        .bearer_auth(&mcp)
        .header("content-type", "application/octet-stream")
        .query(&json!({"request_id":Uuid::now_v7(),"label":"虚构中断测试凭据"}))
        .body("00123456")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["secret_ref"]
        .as_str()
        .unwrap()
        .into();
    for (signal_name, signal_value) in [
        ("term", rustix::process::Signal::TERM),
        ("int", rustix::process::Signal::INT),
    ] {
        let marker = root.join(format!("signal-marker-{signal_name}"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_chromie"))
            .args(["secret", "exec", "--config"])
            .arg(&config)
            .args([
                "--ref",
                &reference,
                "--revision",
                "1",
                "--purpose",
                "虚构中断测试",
                "--",
                "python3",
            ])
            .arg(&script)
            .arg(&marker)
            .arg("timeout")
            .env("TMPDIR", &root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(marker.exists());
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
            signal_value,
        )
        .unwrap();
        let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
            .await
            .unwrap();
        assert!(!output.status.success());
        let signal_receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(signal_receipt["outcome"], "interrupted");
        assert_eq!(signal_receipt["audit_reported"], true);
        assert!(!Path::new(&fs::read_to_string(marker).unwrap()).exists());
    }
    let output = tokio::task::spawn_blocking({
        let config = config.clone();
        let root = root.clone();
        let reference = reference.clone();
        move || {
            Command::new(env!("CARGO_BIN_EXE_chromie"))
                .args(["secret", "exec", "--config"])
                .arg(config)
                .args([
                    "--ref",
                    &reference,
                    "--revision",
                    "1",
                    "--purpose",
                    "虚构启动失败",
                    "--",
                    "/chromie-fixture-no-such-program",
                ])
                .env("TMPDIR", root)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(!output.status.success());
    let failed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failed["outcome"], "spawn_failed");
    assert_eq!(failed["audit_reported"], true);
    assert_eq!(failed["temporary_files_removed"], true);
    assert!(fs::read_dir(&root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("chromie-exec-")
    }));
    let audit = latest.unwrap();
    let endpoint = format!("{base}/spaces/{slug}/secrets/access-audit/{audit}/result");
    let report = json!({"outcome":"timed_out","exit_code":null});
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&other_mcp)
            .json(&report)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&mcp)
            .json(&report)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&mcp)
            .json(&json!({"outcome":"succeeded","exit_code":0}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let rpc_url = format!("{base}/spaces/{slug}/mcp");
    rpc(&client,&rpc_url,&mcp,"initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"0"}})).await;
    let guide = rpc(
        &client,
        &rpc_url,
        &mcp,
        "tools/call",
        json!({"name":"get_setup_guide","arguments":{"topic":"secret"}}),
    )
    .await;
    let version = guide["result"]["structuredContent"]["guide_version"].clone();
    assert_eq!(
        guide["result"]["structuredContent"]["secret_access_enabled"],
        true
    );
    store
        .update_agent(
            space,
            agent,
            UpdateAgent {
                agent_name: None,
                enabled: None,
                allow_secrets: Some(false),
            },
        )
        .await
        .unwrap();
    let cached = rpc(&client,&rpc_url,&mcp,"tools/call",json!({"name":"get_setup_guide","arguments":{"topic":"secret","known_guide_version":version}})).await;
    assert_eq!(
        cached["result"]["structuredContent"]["secret_access_enabled"],
        false
    );
    assert_eq!(cached["result"]["structuredContent"]["guide_cached"], true);
    assert!(cached["result"]["structuredContent"].get("guide").is_none());
    task.abort();
    fs::remove_dir_all(root).unwrap();
}
