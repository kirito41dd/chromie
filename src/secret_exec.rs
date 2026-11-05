//! 本地受控生命周期。文件路径可交给子进程，原文和命令不进入回执或审计。
use crate::{
    error::{Error, Result},
    secret_audit::{ExecutionOutcome, ExecutionReport},
};
use serde_json::{Value, json};
use std::{ffi::OsString, path::PathBuf};

#[derive(clap::Args)]
pub struct Exec {
    /// secret://空间/记忆UUID，只取当前服务所需的 Secret
    #[arg(long = "ref", value_name = "引用")]
    reference: String,
    /// 刚从公开元数据读取的修订号，不能省略
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..), value_name = "修订")]
    revision: i32,
    /// 简短公开用途，不能填写凭据、完整任务或命令
    #[arg(long, value_name = "用途")]
    purpose: String,
    /// 超时秒数；到期停止进程组并删除临时文件
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600), value_name = "秒")]
    timeout: u64,
    /// 透传子进程输出；须先确认程序不会输出凭据
    #[arg(long)]
    inherit_output: bool,
    /// 本机私有接入文件的绝对路径
    #[arg(long, value_name = "文件")]
    config: Option<PathBuf>,
    /// 要执行的程序及参数，放在 -- 后；不会通过 Shell 解释
    #[arg(last = true, required = true, num_args = 1.., value_name = "程序及参数")]
    program_args: Vec<OsString>,
}

#[cfg(not(unix))]
pub async fn run(_: Exec) -> Result<Value> {
    Err(Error::invalid("Secret 托管执行支持 macOS 与 Linux"))
}

#[cfg(unix)]
pub async fn run(args: Exec) -> Result<Value> {
    use crate::{secret_cli as cli, secret_contract::MAX_SECRET_BYTES};
    use reqwest::{Client, header};
    use std::{fs, os::unix::fs::DirBuilderExt, process::Stdio, time::Duration};
    use tokio::signal::unix::{SignalKind, signal};
    use uuid::Uuid;

    crate::secret_audit::validate_purpose(Some(&args.purpose))?;
    if args.config.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err(Error::invalid("接入配置须为绝对路径"));
    }
    let connection = cli::connection(args.config.as_deref())?;
    let id = cli::reference_id(&args.reference, &connection.space_id)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::unavailable())?;
    // 先安装信号处理，再创建任何私有文件；HTTP 中断只会取消等待。
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| Error::unavailable())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|_| Error::unavailable())?;
    let fetch = async {
        let response = client
            .get(format!(
                "{}/spaces/{}/secrets/{id}",
                connection.base_url, connection.space_id
            ))
            .header(header::AUTHORIZATION, cli::auth(&connection)?)
            .query(&json!({"expected_revision":args.revision,"purpose":args.purpose}))
            .send()
            .await
            .map_err(|_| Error::invalid("无法连接 Secret 服务"))?;
        cli::status(&response)?;
        let headers = response.headers();
        if headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/octet-stream")
            || headers
                .get("x-chromie-revision")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i32>().ok())
                != Some(args.revision)
        {
            return Err(Error::invalid(
                "服务端未确认一致的 Secret 修订或格式，未执行",
            ));
        }
        let audit_id = headers
            .get("x-chromie-audit-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| Error::invalid("服务端不支持取用审计，请先升级服务"))?;
        let bytes = cli::body(response, MAX_SECRET_BYTES).await?;
        cli::validate_size(&bytes)?;
        Ok((bytes, audit_id))
    };
    let (bytes, audit_id) = tokio::select! {
        biased;
        _ = interrupt.recv() => return Err(Error::invalid("已取消取用，未执行")),
        _ = terminate.recv() => return Err(Error::invalid("已取消取用，未执行")),
        result = fetch => result?,
    };
    let directory = std::env::temp_dir().join(format!("chromie-exec-{}", Uuid::now_v7()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| Error::invalid("无法创建私有执行目录，未执行"))?;
    let mut guard = ExecutionGuard {
        directory,
        group: None,
    };
    let file = guard.directory.join("secret");
    crate::codex_setup::create_private(&file, &bytes)?;
    drop(bytes);
    let program = args
        .program_args
        .first()
        .ok_or_else(|| Error::invalid("缺少执行程序"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args.program_args.iter().skip(1))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command.process_group(0);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CHROMIE_") {
            command.env_remove(key);
        }
    }
    command.env("CHROMIE_SECRET_FILE", &file);
    if args.inherit_output {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let report = match command.spawn() {
        Err(_) => ExecutionReport {
            outcome: ExecutionOutcome::SpawnFailed,
            exit_code: None,
        },
        Ok(mut child) => {
            guard.group = child
                .id()
                .and_then(|n| i32::try_from(n).ok())
                .and_then(rustix::process::Pid::from_raw);
            let result = tokio::select! {
                biased;
                _ = interrupt.recv() => ExecutionReport { outcome: ExecutionOutcome::Interrupted, exit_code: None },
                _ = terminate.recv() => ExecutionReport { outcome: ExecutionOutcome::Interrupted, exit_code: None },
                _ = tokio::time::sleep(Duration::from_secs(args.timeout)) => ExecutionReport { outcome: ExecutionOutcome::TimedOut, exit_code: None },
                status = child.wait() => match status {
                    Ok(status) => ExecutionReport { outcome: if status.success() {ExecutionOutcome::Succeeded} else {ExecutionOutcome::Failed}, exit_code: status.code() },
                    Err(_) => ExecutionReport { outcome: ExecutionOutcome::Failed, exit_code: None },
                },
            };
            guard.stop();
            let _ = child.wait().await;
            result
        }
    };
    // 先清理再发 HTTP；网络故障不能延长凭据文件的寿命。
    let cleaned = guard.cleanup();
    let reported = match client
        .post(format!(
            "{}/spaces/{}/secrets/access-audit/{audit_id}/result",
            connection.base_url, connection.space_id
        ))
        .header(header::AUTHORIZATION, cli::auth(&connection)?)
        .json(&report)
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    };
    Ok(
        json!({"execution":true,"outcome":report.outcome,"exit_code":report.exit_code,
        "revision":args.revision,"audit_id":audit_id,"audit_reported":reported,"temporary_files_removed":cleaned,
        "result_source":"client_reported"}),
    )
}

#[cfg(unix)]
struct ExecutionGuard {
    directory: PathBuf,
    group: Option<rustix::process::Pid>,
}
#[cfg(unix)]
impl ExecutionGuard {
    fn stop(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
    fn cleanup(&mut self) -> bool {
        match std::fs::remove_dir_all(&self.directory) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }
}
#[cfg(unix)]
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.stop();
        self.cleanup();
    }
}
