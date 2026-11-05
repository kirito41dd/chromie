#![cfg(unix)]
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use uuid::Uuid;

#[derive(Default)]
struct Data {
    bytes: Vec<u8>,
    revision: i32,
    receipts: HashMap<String, (Vec<u8>, i32)>,
}
#[derive(Clone)]
struct Mock {
    data: Arc<Mutex<Data>>,
    mode: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    id: Uuid,
    received: PathBuf,
}
fn error_response(mode: usize) -> Option<Response> {
    match mode {
        1 => Some(
            (
                StatusCode::FORBIDDEN,
                "fixture response secret must never be echoed",
            )
                .into_response(),
        ),
        2 => Some(
            (
                StatusCode::TEMPORARY_REDIRECT,
                [(header::LOCATION, "http://127.0.0.1:9/should-not-follow")],
            )
                .into_response(),
        ),
        _ => None,
    }
}
async fn write(
    State(state): State<Mock>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    bytes: Bytes,
) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    assert!(
        headers[header::AUTHORIZATION]
            .to_str()
            .unwrap()
            .starts_with("Bearer cm_mcp_")
    );
    let mode = state.mode.load(Ordering::SeqCst);
    if let Some(response) = error_response(mode) {
        return response;
    }
    if mode == 6 {
        fs::write(&state.received, b"received").unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    if mode == 5 {
        return axum::Json(
            json!({"memory_id":state.id,"secret_ref":"fixture-unsafe-value","hidden":true}),
        )
        .into_response();
    }
    let mut data = state.data.lock().unwrap();
    let id = query.get("request_id").unwrap().clone();
    let (revision, replayed) = if let Some((old, revision)) = data.receipts.get(&id) {
        if old.as_slice() != bytes.as_ref() {
            return StatusCode::CONFLICT.into_response();
        }
        (*revision, true)
    } else {
        if let Some(expected) = query.get("expected_revision") {
            if expected.parse::<i32>().unwrap() != data.revision {
                return StatusCode::CONFLICT.into_response();
            }
        }
        data.revision += 1;
        data.bytes = bytes.to_vec();
        let revision = data.revision;
        data.receipts.insert(id, (bytes.to_vec(), revision));
        (revision, false)
    };
    axum::Json(json!({"memory_id":state.id,"secret_ref":format!("secret://demo/{}",state.id),"revision":revision,"readable":true,"replayed":replayed})).into_response()
}
async fn read(State(state): State<Mock>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let mode = state.mode.load(Ordering::SeqCst);
    if let Some(response) = error_response(mode) {
        return response;
    }
    let bytes = if mode == 4 {
        vec![b'x'; 60001]
    } else {
        state.data.lock().unwrap().bytes.clone()
    };
    (
        [(
            header::CONTENT_TYPE,
            if mode == 3 {
                "text/html"
            } else {
                "application/octet-stream"
            },
        )],
        bytes,
    )
        .into_response()
}
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    state: Mock,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("chromie-secret-cli-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Mock {
            data: Arc::default(),
            mode: Arc::default(),
            calls: Arc::default(),
            id: Uuid::now_v7(),
            received: root.join("received"),
        };
        let router = Router::new()
            .route("/spaces/demo/secrets", post(write))
            .route("/spaces/demo/secrets/{id}", get(read).put(write))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = root.join("client.json");
        private(&config,&serde_json::to_vec(&json!({"url":url,"space":"demo","agent_id":Uuid::now_v7(),"mcp_key":format!("cm_mcp_{}","a".repeat(64)),"hook_key":format!("cm_hook_{}","b".repeat(64))})).unwrap());
        Self {
            root,
            config,
            state,
            task,
        }
    }
    async fn run(&self, args: Vec<String>) -> Output {
        let config = self.config.clone();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_chromie"));
            command
                .arg("secret")
                .args(args)
                .arg("--config")
                .arg(config)
                .env("HOME", root)
                .env_remove("DATABASE_URL")
                .env_remove("CHROMIE_CLIENT_CONFIG");
            command.output().unwrap()
        })
        .await
        .unwrap()
    }
    fn reference(&self) -> String {
        format!("secret://demo/{}", self.state.id)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn private(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn safe(output: &Output, secret: &[u8]) {
    let text = [output.stdout.as_slice(), output.stderr.as_slice()].concat();
    assert!(!text.windows(secret.len()).any(|p| p == secret));
    assert!(!text.windows(64).any(|p| p == "a".repeat(64).as_bytes()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_round_trip_update_idempotency_and_replace_never_echo_plaintext() {
    let f = Fixture::new().await;
    let source = f.root.join("source.bin");
    let first = b"fictional-secret-alpha\0\xff";
    private(&source, first);
    let request = Uuid::now_v7().to_string();
    let args = vec![
        "put".into(),
        "--label".into(),
        "help".into(),
        "--file".into(),
        source.to_string_lossy().into(),
        "--request-id".into(),
        request,
    ];
    let output = f.run(args.clone()).await;
    assert!(output.status.success(), "CLI put 失败");
    safe(&output, first);
    let saved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["replayed"], false);
    let output = f.run(args).await;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["replayed"],
        true
    );
    let target = f.root.join("download.bin");
    let get = vec![
        "get".into(),
        "--ref".into(),
        f.reference(),
        "--out".into(),
        target.to_string_lossy().into(),
    ];
    let output = f.run(get.clone()).await;
    assert!(output.status.success());
    safe(&output, first);
    assert_eq!(fs::read(&target).unwrap(), first);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let output = f.run(get.clone()).await;
    assert!(!output.status.success());
    assert_eq!(fs::read(&target).unwrap(), first);
    let second = b"fictional-secret-beta";
    private(&source, second);
    let update = vec![
        "update".into(),
        "--ref".into(),
        f.reference(),
        "--revision".into(),
        "1".into(),
        "--file".into(),
        source.to_string_lossy().into(),
    ];
    let output = f.run(update.clone()).await;
    assert!(output.status.success());
    safe(&output, second);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["revision"],
        2
    );
    let output = f.run(update).await;
    assert!(!output.status.success());
    safe(&output, second);
    assert!(String::from_utf8_lossy(&output.stderr).contains("request_id="));
    let mut replace = get;
    replace.push("--replace".into());
    let output = f.run(replace).await;
    assert!(output.status.success());
    assert_eq!(fs::read(&target).unwrap(), second);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_space_permission_redirect_and_response_boundaries_preserve_existing_files() {
    let f = Fixture::new().await;
    let source = f.root.join("source.bin");
    let secret = b"fixture-private-secret-value";
    private(&source, secret);
    let put = vec![
        "put".into(),
        "--label".into(),
        "虚构服务".into(),
        "--file".into(),
        source.to_string_lossy().into(),
    ];
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!f.run(put.clone()).await.status.success());
    assert_eq!(f.state.calls.load(Ordering::SeqCst), 0);
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.root.join("link.bin");
    symlink(&source, &link).unwrap();
    let mut symlink_put = put.clone();
    *symlink_put.last_mut().unwrap() = link.to_string_lossy().into();
    assert!(!f.run(symlink_put).await.status.success());
    assert_eq!(f.state.calls.load(Ordering::SeqCst), 0);
    for mode in [1, 2, 5] {
        f.state.mode.store(mode, Ordering::SeqCst);
        let output = f.run(put.clone()).await;
        assert!(!output.status.success());
        safe(&output, secret);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture response secret"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-unsafe-value"));
    }
    f.state.mode.store(0, Ordering::SeqCst);
    assert!(f.run(put).await.status.success());
    let target = f.root.join("existing.bin");
    private(&target, b"original");
    let get = vec![
        "get".into(),
        "--ref".into(),
        f.reference(),
        "--out".into(),
        target.to_string_lossy().into(),
        "--replace".into(),
    ];
    for mode in [1, 2, 3, 4] {
        f.state.mode.store(mode, Ordering::SeqCst);
        let output = f.run(get.clone()).await;
        assert!(!output.status.success());
        safe(&output, secret);
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }
    f.state.mode.store(0, Ordering::SeqCst);
    let calls = f.state.calls.load(Ordering::SeqCst);
    let mut foreign = get;
    foreign[2] = format!("secret://other/{}", f.state.id);
    assert!(!f.run(foreign).await.status.success());
    assert_eq!(f.state.calls.load(Ordering::SeqCst), calls);
    let output = f
        .run(vec![
            "put".into(),
            "--label".into(),
            "虚构服务".into(),
            "--prompt".into(),
        ])
        .await;
    assert!(!output.status.success());
    assert_eq!(f.state.calls.load(Ordering::SeqCst), calls);
}

// 每次看见提示立即发送，不加输入延迟；原终端设置逐字段比较。
const TTY_SCRIPT: &str = r#"import os,pty,select,sys,time,json,termios,fcntl,subprocess,signal
binary,config,mode,received=sys.argv[1:]; marker=b'fixture-hidden-password-321'
fd,slave=pty.openpty(); original=termios.tcgetattr(slave); report_r,report_w=os.pipe(); pid=os.fork()
if pid==0:
 os.close(fd);os.close(report_r);os.setsid();fcntl.ioctl(slave,termios.TIOCSCTTY,0)
 for target in (0,1,2):os.dup2(slave,target)
 if slave>2:os.close(slave)
 signal.signal(signal.SIGINT,signal.SIG_IGN)
 original=termios.tcgetattr(0)
 result=subprocess.run([binary,'secret','put','--label','TTY fixture','--prompt','--config',config])
 restored=termios.tcgetattr(0)
 # macOS 切换 ICANON 后由内核设置 PENDIN；只忽略此待处理状态，逐项核对其余设置。
 original[3]&=~getattr(termios,'PENDIN',0);restored[3]&=~getattr(termios,'PENDIN',0)
 os.write(report_w,json.dumps({'exit':result.returncode,'restored':restored==original,'changes':[i for i,(a,b) in enumerate(zip(original,restored)) if a!=b]}).encode());os._exit(0)
os.close(report_w)
out=bytearray();sent=0;deadline=time.monotonic()+20;status=None;cancelled=False
try:
 while time.monotonic()<deadline:
  if select.select([fd],[],[],0.01)[0]:
   out.extend(os.read(fd,4096))
   if sent==0 and 'Secret（'.encode() in out:
    os.write(fd,b'\x03' if mode=='cancel-first' else b'\x04' if mode=='eof-first' else marker+b'\n');sent=1
   if sent==1 and '再次输入确认：'.encode() in out:
    os.write(fd,b'\x03' if mode=='cancel-second' else b'fixture-wrong-confirmation\n' if mode=='mismatch' else marker+b'\n');sent=2
  if mode=='http-cancel' and not cancelled and os.path.exists(received):
   os.write(fd,b'\x03');cancelled=True
  done,code=os.waitpid(pid,os.WNOHANG)
  if done:
   status=os.waitstatus_to_exitcode(code)
   while select.select([fd],[],[],0)[0]:
    try:chunk=os.read(fd,4096)
    except OSError:break
    if not chunk:break
    out.extend(chunk)
   break
 if status is None:os.kill(pid,9);os.waitpid(pid,0);raise RuntimeError('TTY timeout')
 report=json.loads(os.read(report_r,4096));text=out.decode(errors='replace')
 print(json.dumps({'exit':report['exit'],'sent':sent,'echoed':marker in out or b'fixture-wrong-confirmation' in out,'receipt':'secret://demo/' in text,'restored':report['restored'],'changes':report['changes'],'cancelled':cancelled,'unknown':'写入结果可能未知' in text,'denied':'secret_access_denied' in text}))
finally:os.close(fd);os.close(slave);os.close(report_r)
"#;

async fn tty(f: &Fixture, mode: &'static str) -> Value {
    let config = f.config.clone();
    let received = f.state.received.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("python3")
            .args(["-c", TTY_SCRIPT, env!("CARGO_BIN_EXE_chromie")])
            .arg(config)
            .arg(mode)
            .arg(received)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "PTY 辅助程序失败：{}",
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("无诊断")
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["echoed"], false, "{mode}");
    assert_eq!(
        data["restored"], true,
        "{mode}: terminal fields={}",
        data["changes"]
    );
    data
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tty_prompt_hides_immediate_input_and_restores_settings_on_success() {
    let f = Fixture::new().await;
    for _ in 0..12 {
        let data = tty(&f, "success").await;
        assert_eq!(data["exit"], 0);
        assert_eq!(data["sent"], 2);
        assert_eq!(data["receipt"], true);
    }
    assert_eq!(f.state.calls.load(Ordering::SeqCst), 12);
    assert_eq!(
        f.state.data.lock().unwrap().bytes,
        b"fixture-hidden-password-321"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tty_mismatch_ctrl_c_eof_and_http_failure_restore_without_leaking() {
    let f = Fixture::new().await;
    for mode in ["mismatch", "cancel-first", "cancel-second", "eof-first"] {
        let data = tty(&f, mode).await;
        assert_ne!(data["exit"], 0, "{mode}");
        assert_eq!(data["receipt"], false, "{mode}");
        assert_eq!(f.state.calls.load(Ordering::SeqCst), 0, "{mode}");
    }
    f.state.mode.store(1, Ordering::SeqCst);
    let data = tty(&f, "http-error").await;
    assert_ne!(data["exit"], 0);
    assert_eq!(data["denied"], true);
    f.state.mode.store(6, Ordering::SeqCst);
    let data = tty(&f, "http-cancel").await;
    assert_ne!(data["exit"], 0);
    assert_eq!(data["cancelled"], true);
    assert_eq!(data["unknown"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revision_guard_requires_explicit_server_confirmation_and_preserves_file() {
    let f = Fixture::new().await;
    f.state.data.lock().unwrap().bytes = b"fictional-server-bytes".to_vec();
    let target = f.root.join("guarded.bin");
    private(&target, b"original-file");
    let result = f
        .run(vec![
            "get".into(),
            "--ref".into(),
            f.reference(),
            "--out".into(),
            target.to_string_lossy().into(),
            "--replace".into(),
            "--revision".into(),
            "1".into(),
        ])
        .await;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("修订"));
    assert_eq!(fs::read(target).unwrap(), b"original-file");
    safe(&result, b"fictional-server-bytes");
}

#[test]
fn standard_help_does_not_require_a_connection_or_database() {
    for args in [
        vec!["--help"],
        vec!["secret", "--help"],
        vec!["secret", "put", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_chromie"))
            .args(args)
            .env_remove("DATABASE_URL")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("secret"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要本机 Nushell"]
async fn nushell_commands_use_the_local_default_config_and_explicit_environment_never_falls_back() {
    let f = Fixture::new().await;
    let directory = f.root.join(".config/chromie/codex");
    fs::create_dir_all(&directory).unwrap();
    let config = directory.join("client.json");
    fs::copy(&f.config, &config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let bin = f.root.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    symlink(env!("CARGO_BIN_EXE_chromie"), bin.join("chromie")).unwrap();
    let source = f.root.join("nu-secret.bin");
    let secret = b"fictional-nushell-secret";
    private(&source, secret);
    let output = f.root.join("nu-out.bin");
    let home = f.root.clone();
    let input = source.clone();
    let wrong_environment = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_chromie"))
            .args(["secret", "put", "--label", "Env fixture", "--file"])
            .arg(input)
            .env("HOME", home)
            .env_remove("CHROMIE_CLIENT_CONFIG")
            .env("CHROMIE_URL", "http://127.0.0.1:9")
            .env("CHROMIE_SPACE", "demo")
            .env("CHROMIE_AGENT_ID", Uuid::now_v7().to_string())
            .env("CHROMIE_MCP_KEY", format!("cm_mcp_{}", "a".repeat(64)))
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(!wrong_environment.status.success());
    assert_eq!(f.state.calls.load(Ordering::SeqCst), 0);
    let code = "$env.PATH = ($env.PATH | prepend ($nu.home-dir | path join '.local' 'bin')); let receipt = (chromie secret put --label 'Nu fixture' --file $env.CHROMIE_TEST_SOURCE | from json); chromie secret get --ref $receipt.secret_ref --out $env.CHROMIE_TEST_OUT";
    let home = f.root.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut command = Command::new("nu");
        command
            .args(["--no-config-file", "-c", code])
            .env("HOME", home)
            .env("CHROMIE_TEST_SOURCE", source)
            .env("CHROMIE_TEST_OUT", &output);
        for key in [
            "CHROMIE_CLIENT_CONFIG",
            "CHROMIE_URL",
            "CHROMIE_SPACE",
            "CHROMIE_AGENT_ID",
            "CHROMIE_MCP_KEY",
        ] {
            command.env_remove(key);
        }
        let result = command.output().unwrap();
        (result, output)
    })
    .await
    .unwrap();
    assert!(
        result.0.status.success(),
        "Nushell Secret 命令失败，输出未回显"
    );
    safe(&result.0, secret);
    assert_eq!(fs::read(result.1).unwrap(), secret);
}
