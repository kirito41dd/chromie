//! 工作副本和服务端身份分开保存，目录可直接交给任意支持 Agent Skills 的宿主。
use crate::{
    client_config::ClientConnection,
    codex_setup,
    error::{Error, Result},
    secret_cli,
    skill_package::{self, MAX_BYTES, MAX_FILES, PublishSkill, RollbackSkill, SkillUploadFile},
};
use clap::Subcommand;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Command {
    /// 导出随 CLI 分发的 Chromie 使用指南
    Export {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        replace: bool,
    },
    /// 离线验证可分享的标准 Skill 包
    Validate { directory: PathBuf },
    /// 将当前云端版本拉取到一个新目录
    Pull {
        skill: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 创建或从工作副本发布新修订
    Push {
        directory: PathBuf,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 查看修订、原因与可回退状态
    History {
        skill_id: Uuid,
        #[arg(long)]
        before: Option<i32>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 从工作副本回退到尚保留的修订，生成新版本
    Rollback {
        directory: PathBuf,
        #[arg(long)]
        revision: i32,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn local() -> Error {
    Error::invalid("无法安全读写 Skill 目录或工作副本状态")
}
fn network() -> Error {
    Error::unavailable()
}
/// 保持目录文件描述符直至读完子项；文件树在上传过程中变化也不能重定向到包外。
#[cfg(unix)]
fn package(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    use rustix::fs::{Dir, Mode, OFlags, open, openat};
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let root = open(dir, flags | OFlags::DIRECTORY, Mode::empty()).map_err(|_| local())?;
    let mut files = BTreeMap::new();
    let mut total = 0;
    fn visit(
        parent: &std::os::fd::OwnedFd,
        prefix: &str,
        flags: OFlags,
        files: &mut BTreeMap<String, Vec<u8>>,
        total: &mut usize,
    ) -> Result<()> {
        for entry in Dir::read_from(parent).map_err(|_| local())? {
            let entry = entry.map_err(|_| local())?;
            let part = entry.file_name().to_str().map_err(|_| local())?;
            if matches!(part, "." | "..") || part.eq_ignore_ascii_case(".git") {
                continue;
            }
            let path = if prefix.is_empty() {
                part.to_owned()
            } else {
                format!("{prefix}/{part}")
            };
            skill_package::validate_path(&path)?;
            let child =
                openat(parent, entry.file_name(), flags, Mode::empty()).map_err(|_| local())?;
            let file = fs::File::from(child);
            let meta = file.metadata().map_err(|_| local())?;
            if meta.is_dir() {
                visit(&file.into(), &path, flags, files, total)?;
                continue;
            }
            if !meta.is_file() || files.len() >= MAX_FILES {
                return Err(Error::invalid("Skill 只接受普通文件，最多 512 个"));
            }
            let mut data = Vec::new();
            file.take((MAX_BYTES - *total + 1) as u64)
                .read_to_end(&mut data)
                .map_err(|_| local())?;
            *total += data.len();
            if *total > MAX_BYTES {
                return Err(Error::invalid("Skill 总大小超过 16 MiB"));
            }
            files.insert(path, data);
        }
        Ok(())
    }
    visit(&root, "", flags, &mut files, &mut total)?;
    Ok(files)
}

#[cfg(not(unix))]
fn package(_: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    Err(Error::invalid("当前发布的 Skill CLI 支持 macOS 和 Linux"))
}
fn name(dir: &Path) -> Result<&str> {
    dir.file_name().and_then(|n| n.to_str()).ok_or_else(local)
}

struct Remote {
    connection: ClientConnection,
    http: Client,
}
impl Remote {
    fn new(config: Option<&Path>) -> Result<Self> {
        Ok(Self {
            connection: secret_cli::connection(config)?,
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| network())?,
        })
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let c = &self.connection;
        Ok(self
            .http
            .request(
                method,
                format!("{}/spaces/{}/skills/{}", c.base_url, c.space_id, path),
            )
            .header(reqwest::header::AUTHORIZATION, secret_cli::auth(c)?)
            .header("x-chromie-agent-id", c.agent_id.to_string()))
    }
    async fn body(&self, request: RequestBuilder, limit: usize) -> Result<Vec<u8>> {
        let mut response = request.send().await.map_err(|_| network())?;
        if !response.status().is_success() {
            return Err(http_error(&response));
        }
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| network())? {
            if chunk.len() > limit - data.len() {
                return Err(Error::invalid("服务器文件或清单超过上限"));
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }
    async fn json(&self, request: RequestBuilder) -> Result<Value> {
        serde_json::from_slice(&self.body(request, 1024 * 1024).await?).map_err(|_| network())
    }
    async fn receipt(&self, id: Uuid) -> Result<Option<Value>> {
        let res = self
            .request(Method::GET, &format!("receipts/{id}"))?
            .send()
            .await
            .map_err(|_| network())?;
        if res.status() == 404 {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(http_error(&res));
        }
        res.json().await.map(Some).map_err(|_| network())
    }
}
fn http_error(r: &Response) -> Error {
    match r.status().as_u16() {
        401 | 403 => Error::agent_unauthorized(),
        404 => Error::missing(),
        409 => Error::conflict("skill_conflict"),
        400 | 413 | 422 => Error::invalid("服务拒绝 Skill 参数、文件或清单；请先运行 validate"),
        _ => network(),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Workspace {
    id: Option<Uuid>,
    revision: Option<i32>,
    pending: Option<Pending>,
    #[serde(default)]
    needs_pull: bool,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    request_id: Uuid,
    fingerprint: String,
    #[serde(default)]
    rollback: bool,
}
struct State {
    path: PathBuf,
    _lock: fs::File,
    workspace: Workspace,
}
impl State {
    fn open(remote: &Remote, dir: &Path) -> Result<Self> {
        let data = match std::env::var_os("CHROMIE_SKILL_STATE_DIR") {
            Some(p) => PathBuf::from(p),
            None => codex_setup::default_data_dir()?.join("skill-workspaces"),
        };
        codex_setup::directory(&data, true)?;
        let absolute = dir.canonicalize().map_err(|_| local())?;
        if data
            .canonicalize()
            .map_err(|_| local())?
            .starts_with(&absolute)
        {
            return Err(local());
        }
        let key = serde_json::to_vec(&(
            &remote.connection.base_url,
            &remote.connection.space_id,
            remote.connection.agent_id,
            absolute,
        ))
        .map_err(|_| local())?;
        let hash = skill_package::digest(&key);
        let base = data.join(hash.trim_start_matches("sha256:"));
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.with_extension("lock"))
            .map_err(|_| local())?;
        lock.try_lock()
            .map_err(|_| Error::invalid("另一个 CLI 正在操作此工作副本"))?;
        let path = base.with_extension("json");
        let workspace = match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| local())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Workspace::default(),
            Err(_) => return Err(local()),
        };
        Ok(Self {
            path,
            _lock: lock,
            workspace,
        })
    }
    fn save(&self) -> Result<()> {
        codex_setup::replace(
            &self.path,
            &serde_json::to_vec(&self.workspace).map_err(|_| local())?,
        )
    }
    fn finish(&mut self, receipt: &Value) -> Result<()> {
        self.workspace.id =
            Some(serde_json::from_value(receipt["skill_id"].clone()).map_err(|_| network())?);
        self.workspace.revision =
            Some(serde_json::from_value(receipt["revision"].clone()).map_err(|_| network())?);
        self.workspace.needs_pull = self.workspace.pending.as_ref().is_some_and(|p| p.rollback);
        self.workspace.pending = None;
        self.save()
    }
}

#[derive(Deserialize)]
struct Manifest {
    skill_id: Uuid,
    revision: i32,
    frontmatter: Value,
    files: Vec<FileInfo>,
}
#[derive(Deserialize)]
struct FileInfo {
    path: String,
    size: usize,
    digest: String,
}
#[derive(Deserialize)]
struct Staged {
    object_id: Uuid,
    digest: String,
    size: usize,
}

async fn pull(remote: Remote, skill: String, out: PathBuf) -> Result<Value> {
    let request = if let Ok(id) = Uuid::parse_str(&skill) {
        remote.request(Method::GET, &id.to_string())?
    } else {
        skill_package::validate_name(&skill)?;
        remote
            .request(Method::GET, "lookup")?
            .query(&[("name", skill)])
    };
    // JSON 转义可能超过原始 frontmatter 大小。额外空间覆盖文件清单和固定字段。
    let limit = MAX_BYTES * 6 + 1024 * 1024;
    let m: Manifest =
        serde_json::from_slice(&remote.body(request, limit).await?).map_err(|_| network())?;
    let skill_name = m.frontmatter["name"].as_str().ok_or_else(network)?;
    if name(&out)? != skill_name {
        return Err(Error::invalid("目标目录的末级名称必须等于 Skill 名称"));
    }
    skill_package::validate_layout(m.files.iter().map(|f| (f.path.as_str(), f.size)))?;
    let mut files = BTreeMap::new();
    for f in m.files {
        let req = remote
            .request(Method::GET, &format!("{}/file", m.skill_id))?
            .query(&[
                ("path", f.path.as_str()),
                ("revision", &m.revision.to_string()),
            ]);
        let data = remote.body(req, f.size).await?;
        if data.len() != f.size || skill_package::digest(&data) != f.digest {
            return Err(Error::invalid("下载文件校验失败"));
        }
        files.insert(f.path, data);
    }
    let metadata = skill_package::validate_package(skill_name, &files)?;
    if metadata != m.frontmatter {
        return Err(Error::invalid("入口文件与清单元数据不一致"));
    }
    // 先写入私有同级目录，所有文件就绪后才原子公开；已有或同时创建的目标均受保护。
    let parent = std::path::absolute(&out).map_err(|_| local())?;
    let staging = parent
        .parent()
        .ok_or_else(local)?
        .join(format!(".chromie-pull-{}", Uuid::now_v7()));
    let mut create = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        create.mode(0o700);
    }
    create.create(&staging).map_err(|_| local())?;
    let result = (|| {
        for (path, data) in files {
            let destination = staging.join(path);
            fs::create_dir_all(destination.parent().ok_or_else(local)?).map_err(|_| local())?;
            // 目标卷若把两个清单路径视为同一文件，整包拉取失败并保留原目录。
            codex_setup::create_private(&destination, &data).map_err(|_| {
                Error::invalid("目标卷无法单独创建清单中的文件，请检查文件名和权限")
            })?;
        }
        install_pulled(&staging, &out)
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    // 若私有状态写入失败，仍保全已经校验的完整导出目录。
    let mut state = State::open(&remote, &out)?;
    state.workspace = Workspace::default();
    let receipt = json!({"skill_id":m.skill_id,"revision":m.revision});
    state.finish(&receipt)?;
    Ok(receipt)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn install_pulled(staging: &Path, target: &Path) -> Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, staging, CWD, target, RenameFlags::NOREPLACE)
        .map_err(|_| Error::invalid("目标目录须不存在且父目录可写"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_pulled(_: &Path, _: &Path) -> Result<()> {
    Err(Error::invalid("当前发布的 Skill CLI 支持 macOS 和 Linux"))
}

async fn write(remote: Remote, dir: PathBuf, reason: String, target: Option<i32>) -> Result<Value> {
    if reason.trim().is_empty() || reason.len() > 2000 {
        return Err(Error::invalid("请提供简短的公开原因"));
    }
    let mut state = State::open(&remote, &dir)?;
    let files = if target.is_none() {
        let files = package(&dir)?;
        skill_package::validate_package(name(&dir)?, &files)?;
        files
    } else {
        BTreeMap::new()
    };
    let entries: Vec<_> = files
        .iter()
        .map(|(p, b)| (p, skill_package::digest(b), b.len()))
        .collect();
    let fingerprint = skill_package::digest(
        &serde_json::to_vec(&(target, &reason, &entries)).map_err(|_| local())?,
    );
    if let Some(p) = &state.workspace.pending {
        let same = p.fingerprint == fingerprint;
        if let Some(receipt) = remote.receipt(p.request_id).await? {
            state.finish(&receipt)?;
            if same {
                return Ok(receipt);
            }
        } else if !same {
            // 尚无旧回执，新内容保留原基础修订。即使旧请求迟到，服务端的版本检查
            // 或包名唯一约束也只允许一方发布。
            state.workspace.pending = None;
        }
    }
    if state.workspace.needs_pull {
        return Err(Error::invalid(
            "回退已生成新修订，本地文件仍是回退前的内容；请拉取到新目录再编辑",
        ));
    }
    if target.is_some() && state.workspace.id.is_none() {
        return Err(Error::invalid("回退需要已拉取或已发布的工作副本"));
    }
    let workspace = &mut state.workspace;
    let request_id = workspace
        .pending
        .get_or_insert_with(|| Pending {
            request_id: Uuid::now_v7(),
            fingerprint,
            rollback: target.is_some(),
        })
        .request_id;
    state.save()?;
    let result = if let Some(revision) = target {
        let rollback = RollbackSkill {
            request_id,
            skill_id: state
                .workspace
                .id
                .ok_or_else(|| Error::invalid("回退需要已拉取的工作副本"))?,
            expected_revision: state.workspace.revision.ok_or_else(local)?,
            target_revision: revision,
            reason,
        };
        remote
            .json(remote.request(Method::POST, "rollback")?.json(&rollback))
            .await
    } else {
        let mut staged = Vec::new();
        for (path, data) in files {
            let digest = skill_package::digest(&data);
            let size = data.len();
            let r: Staged = serde_json::from_value(
                remote
                    .json(remote.request(Method::POST, "objects")?.body(data))
                    .await?,
            )
            .map_err(|_| network())?;
            if digest != r.digest || size != r.size {
                return Err(Error::invalid("服务端上传回执与本机摘要不匹配"));
            }
            staged.push(SkillUploadFile {
                path,
                object_id: r.object_id,
                digest,
                size,
            });
        }
        let publish = PublishSkill {
            request_id,
            skill_id: state.workspace.id,
            expected_revision: state.workspace.revision,
            reason,
            files: staged,
        };
        remote
            .json(remote.request(Method::POST, "publish")?.json(&publish))
            .await
    };
    match result {
        Ok(receipt) => {
            state.finish(&receipt)?;
            Ok(receipt)
        }
        Err(e) if matches!(e.status.as_u16(), 400 | 404 | 409 | 413 | 422) => {
            state.workspace.pending = None;
            state.save()?;
            Err(e)
        }
        Err(e) => Err(e),
    }
}

pub async fn run(cmd: Command) -> Result<Value> {
    match cmd {
        Command::Export { out, replace } => crate::client_skills::export(&out, replace),
        Command::Validate { directory } => {
            let files = package(&directory)?;
            let metadata = skill_package::validate_package(name(&directory)?, &files)?;
            Ok(
                json!({"name":metadata["name"],"files":files.keys().collect::<Vec<_>>(),"total_bytes":files.values().map(Vec::len).sum::<usize>()}),
            )
        }
        Command::Pull { skill, out, config } => {
            pull(Remote::new(config.as_deref())?, skill, out).await
        }
        Command::Push {
            directory,
            reason,
            config,
        } => write(Remote::new(config.as_deref())?, directory, reason, None).await,
        Command::History {
            skill_id,
            before,
            config,
        } => {
            let remote = Remote::new(config.as_deref())?;
            let request = remote.request(Method::GET, &format!("{skill_id}/history"))?;
            remote.json(request.query(&[("before", before)])).await
        }
        Command::Rollback {
            directory,
            revision,
            reason,
            config,
        } => {
            write(
                Remote::new(config.as_deref())?,
                directory,
                reason,
                Some(revision),
            )
            .await
        }
    }
}
