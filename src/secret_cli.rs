//! 本机 Secret 存取。原文只经隐藏输入／私有文件与独立 HTTP 流转。
use crate::{
    agent_key::KeyKind,
    client_config::{self, ClientConnection},
    error::{Error, Result},
    secret_contract::MAX_SECRET_BYTES,
    secret_contract::{SecretContract, TextKind},
    secret_terminal::{Cancellation, Session},
};
use p256::elliptic_curve::zeroize::Zeroizing;
use reqwest::{Client, Method, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(clap::Subcommand)]
pub enum Command {
    /// 录入 Secret，只返回公开引用与回执
    #[command(
        after_help = "示例（Bash/Nushell 通用）：\n  chromie secret put --label '服务密码' --prompt\n  chromie secret put --label '服务密码' --file /私有目录/password.txt --format text --kind password\n尚未提供原文时使用用户终端隐藏输入；已提供并授权代存时，Agent 可通过私有文件完成，无需再次录入。原文不作为命令参数，不写入输出。"
    )]
    Put(Put),
    /// 将 Secret 写入本机私有文件
    Get(Get),
    /// 按当前修订号替换 Secret 原文
    Update(Update),
    /// 补充公开格式契约，不修改原文
    Describe(Describe),
    /// 仅将 JSON 结构清单写到本机私有文件
    Inspect(Inspect),
    /// 临时取用 Secret 并托管执行本机程序，默认不输出子进程内容
    Exec(crate::secret_exec::Exec),
}

#[derive(clap::Args)]
struct Access {
    /// 本机私有接入文件的绝对路径
    #[arg(long, value_name = "文件")]
    config: Option<PathBuf>,
}
#[derive(clap::Args)]
#[group(id = "secret-source", required = true, multiple = false)]
struct Source {
    /// 从用户自己的终端隐藏输入并确认
    #[arg(long)]
    prompt: bool,
    /// 原文所在的绝对私有文件路径
    #[arg(long, value_name = "文件")]
    file: Option<PathBuf>,
}
#[derive(clap::Args)]
struct Metadata {
    /// 公开格式契约文件，不能包含原文
    #[arg(long, value_name = "文件", conflicts_with_all = ["format", "kind"])]
    contract: Option<PathBuf>,
    /// 原文格式；JSON 须使用契约文件声明字段
    #[arg(long, value_parser = ["text", "binary"], value_name = "格式")]
    format: Option<String>,
    /// 文本用途；隐藏输入默认 password
    #[arg(long, value_parser = ["password", "api_token", "text"], value_name = "用途")]
    kind: Option<String>,
    /// 写操作标识，重试时复用同一 UUID
    #[arg(long, value_name = "UUID")]
    request_id: Option<Uuid>,
}
#[derive(clap::Args)]
pub struct Put {
    /// 服务、用途及环境等公开说明，最多 256 UTF-8 字节
    #[arg(long, value_name = "说明")]
    label: String,
    /// 可选的项目范围
    #[arg(long, value_name = "项目")]
    project: Option<String>,
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    metadata: Metadata,
    #[command(flatten)]
    access: Access,
}
#[derive(clap::Args)]
pub struct Get {
    /// secret://空间/记忆UUID
    #[arg(long = "ref", value_name = "引用")]
    reference: String,
    /// 输出的绝对私有文件路径；默认不覆盖
    #[arg(long, value_name = "文件")]
    out: PathBuf,
    /// 仅下载指定修订；自动取用格式契约时须指定
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..), value_name = "修订")]
    revision: Option<i32>,
    /// 允许替换既有目标文件
    #[arg(long)]
    replace: bool,
    #[command(flatten)]
    access: Access,
}
#[derive(clap::Args)]
pub struct Update {
    /// secret://空间/记忆UUID
    #[arg(long = "ref", value_name = "引用")]
    reference: String,
    /// 已读取的当前修订号
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..), value_name = "修订")]
    revision: i32,
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    metadata: Metadata,
    #[command(flatten)]
    access: Access,
}
#[derive(clap::Args)]
pub struct Describe {
    /// secret://空间/记忆UUID
    #[arg(long = "ref", value_name = "引用")]
    reference: String,
    /// 已读取的当前修订号
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..), value_name = "修订")]
    revision: i32,
    #[command(flatten)]
    metadata: Metadata,
    #[command(flatten)]
    access: Access,
}
#[derive(clap::Args)]
pub struct Inspect {
    /// secret://空间/记忆UUID
    #[arg(long = "ref", value_name = "引用")]
    reference: String,
    /// 已读取的当前修订号
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..), value_name = "修订")]
    revision: i32,
    /// 结构清单的绝对私有文件路径
    #[arg(long, value_name = "文件")]
    out: PathBuf,
    /// 允许替换既有清单文件
    #[arg(long)]
    replace: bool,
    #[command(flatten)]
    access: Access,
}

enum Input {
    File(PathBuf),
    Prompt,
}
enum Operation {
    Put {
        label: String,
        project: Option<String>,
        source: Input,
        id: Uuid,
    },
    Get {
        reference: String,
        target: PathBuf,
        replace: bool,
        revision: Option<i32>,
        inspect: bool,
    },
    Describe {
        reference: String,
        revision: i32,
        id: Uuid,
    },
    Update {
        reference: String,
        revision: i32,
        source: Input,
        id: Uuid,
    },
}
struct Options {
    operation: Operation,
    config: Option<PathBuf>,
    contract: Option<SecretContract>,
}

fn access(access: Access) -> Result<Option<PathBuf>> {
    if access
        .config
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(Error::invalid("接入配置须为绝对路径"));
    }
    Ok(access.config)
}

fn source(source: Source) -> Result<Input> {
    match (source.file, source.prompt) {
        (Some(path), false) if path.is_absolute() => Ok(Input::File(path)),
        (None, true) => Ok(Input::Prompt),
        _ => Err(Error::invalid(
            "须且只能指定 --file 绝对私有路径或 --prompt",
        )),
    }
}

fn target(path: PathBuf) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::invalid("须提供 --out 绝对私有文件路径"));
    }
    Ok(path)
}

fn metadata(value: Metadata, prompt: bool) -> Result<(Option<SecretContract>, Uuid)> {
    let Metadata {
        contract,
        format,
        kind,
        request_id,
    } = value;
    let contract = if let Some(path) = contract {
        if !path.is_absolute() || format.is_some() || kind.is_some() {
            return Err(Error::invalid(
                "契约文件须为绝对路径，不能与 --format／--kind 混用",
            ));
        }
        Some(SecretContract::parse(&read_private(&path)?)?)
    } else {
        match format.as_deref().or_else(|| prompt.then_some("text")) {
            Some("text") => Some(SecretContract::Text {
                version: 1,
                kind: match kind.as_deref().unwrap_or("password") {
                    "password" => TextKind::Password,
                    "api_token" => TextKind::ApiToken,
                    "text" => TextKind::Text,
                    _ => return Err(Error::invalid("文本用途须为 password、api_token 或 text")),
                },
            }),
            Some("binary") if kind.is_none() => Some(SecretContract::Binary { version: 1 }),
            None if kind.is_none() => None,
            _ => {
                return Err(Error::invalid(
                    "格式须为 text 或 binary；--kind 仅用于 text",
                ));
            }
        }
    };
    Ok((contract, request_id.unwrap_or_else(Uuid::now_v7)))
}

fn options(command: Command) -> Result<Options> {
    match command {
        Command::Put(args) => {
            let config = access(args.access)?;
            crate::types::bounded(&args.label, 256)?;
            if let Some(project) = &args.project {
                crate::types::bounded(project, 128)?;
            }
            let (contract, id) = metadata(args.metadata, args.source.prompt)?;
            Ok(Options {
                config,
                contract,
                operation: Operation::Put {
                    label: args.label,
                    project: args.project,
                    source: source(args.source)?,
                    id,
                },
            })
        }
        Command::Get(args) => Ok(Options {
            config: access(args.access)?,
            contract: None,
            operation: Operation::Get {
                reference: args.reference,
                target: target(args.out)?,
                replace: args.replace,
                revision: args.revision,
                inspect: false,
            },
        }),
        Command::Update(args) => {
            let config = access(args.access)?;
            let (contract, id) = metadata(args.metadata, args.source.prompt)?;
            Ok(Options {
                config,
                contract,
                operation: Operation::Update {
                    reference: args.reference,
                    revision: args.revision,
                    source: source(args.source)?,
                    id,
                },
            })
        }
        Command::Describe(args) => {
            let config = access(args.access)?;
            let (contract, id) = metadata(args.metadata, false)?;
            if contract.is_none() {
                return Err(Error::invalid("补契约须提供 --contract 或 --format"));
            }
            Ok(Options {
                config,
                contract,
                operation: Operation::Describe {
                    reference: args.reference,
                    revision: args.revision,
                    id,
                },
            })
        }
        Command::Exec(_) => Err(Error::invalid("执行参数路由无效")),
        Command::Inspect(args) => Ok(Options {
            config: access(args.access)?,
            contract: None,
            operation: Operation::Get {
                reference: args.reference,
                target: target(args.out)?,
                replace: args.replace,
                revision: Some(args.revision),
                inspect: true,
            },
        }),
    }
}

pub(crate) fn connection(config: Option<&Path>) -> Result<ClientConnection> {
    if config.is_some() || std::env::var_os("CHROMIE_CLIENT_CONFIG").is_some() {
        return client_config::load(KeyKind::Mcp, config);
    }
    if [
        "CHROMIE_URL",
        "CHROMIE_SPACE",
        "CHROMIE_AGENT_ID",
        "CHROMIE_MCP_KEY",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
    {
        return client_config::load(KeyKind::Mcp, None);
    }
    let default = crate::codex_setup::default_data_dir()?.join("client.json");
    match fs::symlink_metadata(&default) {
        Ok(_) => client_config::load(KeyKind::Mcp, Some(&default)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            client_config::load(KeyKind::Mcp, None)
        }
        Err(_) => Err(Error::invalid("无法检查本机接入文件")),
    }
}

pub(crate) fn reference_id(reference: &str, space: &str) -> Result<Uuid> {
    let address =
        reqwest::Url::parse(reference).map_err(|_| Error::invalid("Secret 引用格式无效"))?;
    if address.scheme() != "secret"
        || address.host_str() != Some(space)
        || !address.username().is_empty()
        || address.password().is_some()
        || address.port().is_some()
        || address.query().is_some()
        || address.fragment().is_some()
    {
        return Err(Error::invalid(
            "Secret 引用必须属于当前空间，不能跨空间取用",
        ));
    }
    Uuid::parse_str(address.path().trim_start_matches('/'))
        .map_err(|_| Error::invalid("Secret 引用须包含有效的记忆 UUID"))
}

fn read_private(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let before = fs::symlink_metadata(path).map_err(|_| Error::invalid("无法读取私有输入文件"))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(Error::invalid("输入必须是普通私有文件，不接受符号链接"));
    }
    private_mode(&before, 0o077)?;
    let file = fs::File::open(path).map_err(|_| Error::invalid("无法打开私有输入文件"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = file.metadata().map_err(|_| Error::unavailable())?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(Error::invalid("输入文件发生变化，未读取原文"));
        }
        private_mode(&after, 0o077)?;
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((MAX_SECRET_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::invalid("读取私有文件失败"))?;
    validate_size(&bytes)?;
    Ok(bytes)
}
fn private_mode(meta: &fs::Metadata, mask: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & mask != 0 {
            return Err(Error::invalid(
                "需要本人使用的私有文件（0600）或目录（0700）",
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (meta, mask);
        Err(Error::invalid(
            "首版 Secret 私有文件操作支持 macOS 与 Linux",
        ))
    }
}
pub(crate) fn validate_size(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
        Err(Error::invalid("Secret 必须为 1～60,000 字节"))
    } else {
        Ok(())
    }
}
fn plaintext(source: Input) -> Result<(Zeroizing<Vec<u8>>, Cancellation)> {
    match source {
        Input::File(path) => Ok((read_private(&path)?, Cancellation::default())),
        Input::Prompt => {
            if !std::io::stdin().is_terminal() {
                return Err(Error::invalid(
                    "--prompt 须由用户在自己的交互终端运行；自动化请使用 --file",
                ));
            }
            let terminal = Session::start()?;
            let first = Zeroizing::new(
                rpassword::prompt_password("Secret（隐藏输入，不要粘贴到聊天）：")
                    .map_err(prompt_error)?,
            );
            validate_size(first.as_bytes())?;
            let second =
                Zeroizing::new(rpassword::prompt_password("再次输入确认：").map_err(prompt_error)?);
            if first.as_bytes() != second.as_bytes() {
                return Err(Error::invalid("两次输入不一致，未保存"));
            }
            Ok((
                Zeroizing::new(first.as_bytes().to_vec()),
                terminal.finish()?,
            ))
        }
    }
}

fn prompt_error(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::UnexpectedEof => {
            Error::invalid("已取消输入，未发送 Secret")
        }
        _ => Error::invalid("终端隐藏输入未完成，未发送 Secret"),
    }
}

pub(crate) fn auth(connection: &ClientConnection) -> Result<header::HeaderValue> {
    let mut value = header::HeaderValue::from_str(&format!("Bearer {}", connection.key.as_str()))
        .map_err(|_| Error::invalid("接入密钥无效"))?;
    value.set_sensitive(true);
    Ok(value)
}
pub(crate) fn status(response: &reqwest::Response) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    Err(match response.status() {
        StatusCode::UNAUTHORIZED => Error::agent_unauthorized(),
        StatusCode::FORBIDDEN => Error {
            code: "secret_access_denied",
            message: "此接入未开启 Secret 存取；请在 Web 接入页核对 Agent 并开启权限",
            status: StatusCode::FORBIDDEN,
        },
        StatusCode::NOT_FOUND => Error::missing(),
        StatusCode::CONFLICT => Error::conflict("secret_write_conflict"),
        _ => Error::invalid("Secret HTTP 请求未成功；未输出服务端响应原文"),
    })
}
pub(crate) async fn body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::invalid("Secret 响应读取失败，未输出原文"))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(Error::invalid("Secret 响应超过上限，已停止读取"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(crate) fn write_private(target: &Path, bytes: &[u8], replace: bool) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::invalid("输出文件路径无效"))?;
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .map_err(|_| Error::invalid("无法创建私有输出目录"))?;
    }
    let parent = fs::canonicalize(parent).map_err(|_| Error::invalid("无法定位私有输出目录"))?;
    let info = fs::metadata(&parent).map_err(|_| Error::unavailable())?;
    private_mode(&info, 0o077)?;
    let destination = parent.join(
        target
            .file_name()
            .ok_or_else(|| Error::invalid("输出路径须为文件"))?,
    );
    match fs::symlink_metadata(&destination) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(Error::invalid("输出目标不能是符号链接或目录"));
            }
            if !replace {
                return Err(Error::invalid(
                    "输出文件已存在；确认替换后显式使用 --replace",
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::invalid("无法检查输出文件")),
    }
    let staging = Staging(parent.join(format!(".chromie-secret-{}.part", Uuid::now_v7())));
    crate::codex_setup::create_private(&staging.0, bytes)?;
    if replace {
        fs::rename(&staging.0, &destination)
            .map_err(|_| Error::invalid("替换输出未成功，未截断原文件"))?;
    } else {
        fs::hard_link(&staging.0, &destination)
            .map_err(|_| Error::invalid("创建输出未成功或目标已存在，未覆盖原文件"))?;
    }
    let _ = fs::File::open(&parent).and_then(|f| f.sync_all());
    Ok(destination)
}

#[derive(Deserialize)]
struct Receipt {
    memory_id: Uuid,
    secret_ref: Option<String>,
    revision: Option<i32>,
    readable: Option<bool>,
    hidden: Option<bool>,
    replayed: Option<bool>,
}
async fn upload(
    client: &Client,
    connection: &ClientConnection,
    method: Method,
    reference: Option<&str>,
    metadata: Value,
    bytes: Zeroizing<Vec<u8>>,
    request_id: Uuid,
) -> Result<Value> {
    let mut endpoint = format!(
        "{}/spaces/{}/secrets",
        connection.base_url, connection.space_id
    );
    let target = reference
        .map(|reference| reference_id(reference, &connection.space_id))
        .transpose()?;
    if let Some(target) = target {
        endpoint = format!("{endpoint}/{target}");
    }
    let response = client
        .request(method, endpoint)
        .header(header::AUTHORIZATION, auth(connection)?)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .query(&metadata)
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|_| Error::invalid("Secret 请求未完成；重试须复用本次 request_id"))?;
    decode_receipt(response, connection, target, request_id).await
}
async fn decode_receipt(
    response: reqwest::Response,
    connection: &ClientConnection,
    target: Option<Uuid>,
    request_id: Uuid,
) -> Result<Value> {
    status(&response)?;
    let bytes = body(response, 8192).await?;
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("Secret 回执格式无效，未输出响应原文"))?;
    let expected = format!("secret://{}/{}", connection.space_id, receipt.memory_id);
    if target.is_some_and(|target| target != receipt.memory_id)
        || receipt.revision.is_some_and(|v| v < 1)
    {
        return Err(Error::invalid("Secret 回执对象或修订与请求不符"));
    }
    if receipt
        .secret_ref
        .as_ref()
        .is_some_and(|value| value != &expected)
    {
        return Err(Error::invalid("Secret 回执引用不属于当前空间或对象"));
    }
    if !receipt.hidden.unwrap_or(false)
        && (receipt.secret_ref.as_deref() != Some(expected.as_str())
            || receipt.revision.is_none_or(|v| v < 1))
    {
        return Err(Error::invalid("Secret 回执引用或修订无效"));
    }
    Ok(
        json!({"request_id":request_id,"memory_id":receipt.memory_id,"secret_ref":receipt.secret_ref,"revision":receipt.revision,"readable":receipt.readable,"hidden":receipt.hidden,"replayed":receipt.replayed}),
    )
}

pub async fn run(command: Command) -> Result<Value> {
    let command = match command {
        Command::Exec(args) => return crate::secret_exec::run(args).await,
        other => other,
    };
    let options = options(command)?;
    let connection = connection(options.config.as_deref())?;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::unavailable())?;
    match options.operation {
        Operation::Describe {
            reference,
            revision,
            id,
        } => {
            let target = reference_id(&reference, &connection.space_id)?;
            eprintln!("本次写操作 request_id={id}；重试请复用。");
            let contract = options
                .contract
                .ok_or_else(|| Error::invalid("缺少公开契约"))?;
            let response = client
                .put(format!(
                    "{}/spaces/{}/secrets/{target}/contract",
                    connection.base_url, connection.space_id
                ))
                .header(header::AUTHORIZATION, auth(&connection)?)
                .json(&json!({"request_id":id,"expected_revision":revision,"contract":contract}))
                .send()
                .await
                .map_err(|_| Error::invalid("Secret 契约请求未完成；重试须复用 request_id"))?;
            decode_receipt(response, &connection, Some(target), id).await
        }
        Operation::Get {
            reference,
            target,
            replace,
            revision,
            inspect,
        } => {
            let id = reference_id(&reference, &connection.space_id)?;
            let response = client
                .get(format!(
                    "{}/spaces/{}/secrets/{id}",
                    connection.base_url, connection.space_id
                ))
                .header(header::AUTHORIZATION, auth(&connection)?)
                .query(
                    &revision
                        .map(|v| json!({"expected_revision":v}))
                        .unwrap_or_else(|| json!({})),
                )
                .send()
                .await
                .map_err(|_| Error::invalid("无法连接 Secret 服务"))?;
            status(&response)?;
            if response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                != Some("application/octet-stream")
            {
                return Err(Error::invalid("响应不是 Secret 二进制数据，未写入文件"));
            }
            let returned_revision = response
                .headers()
                .get("x-chromie-revision")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i32>().ok())
                .filter(|v| *v > 0);
            if revision.is_some() && returned_revision != revision {
                return Err(Error::invalid("服务端未确认一致的 Secret 修订，未写文件"));
            }
            let audit_id = response
                .headers()
                .get("x-chromie-audit-id")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| Uuid::parse_str(v).ok());
            let bytes = body(response, MAX_SECRET_BYTES).await?;
            validate_size(&bytes)?;
            let inspected;
            let content = if inspect {
                inspected = Zeroizing::new(
                    serde_json::to_vec(&crate::secret_contract::inspect_json(&bytes)?)
                        .map_err(|_| Error::unavailable())?,
                );
                inspected.as_slice()
            } else {
                bytes.as_slice()
            };
            let path = write_private(&target, content, replace)?;
            Ok(json!({"written":true,"path":path,"revision":returned_revision,"audit_id":audit_id}))
        }
        Operation::Put {
            label,
            project,
            source,
            id,
        } => {
            eprintln!("本次写操作 request_id={id}；重试请复用。");
            let (bytes, cancellation) = plaintext(source)?;
            if let Some(contract) = &options.contract {
                contract.validate_payload(&bytes)?;
            }
            let mut metadata = json!({"label":label,"request_id":id});
            if let Some(contract) = &options.contract {
                metadata["contract"] = serde_json::to_string(contract)
                    .map_err(|_| Error::unavailable())?
                    .into();
            }
            if let Some(project) = project {
                metadata["project_key"] = project.into();
            }
            cancellation
                .run(upload(
                    &client,
                    &connection,
                    Method::POST,
                    None,
                    metadata,
                    bytes,
                    id,
                ))
                .await
        }
        Operation::Update {
            reference,
            revision,
            source,
            id,
        } => {
            reference_id(&reference, &connection.space_id)?;
            eprintln!("本次写操作 request_id={id}；重试请复用。");
            let (bytes, cancellation) = plaintext(source)?;
            if let Some(contract) = &options.contract {
                contract.validate_payload(&bytes)?;
            }
            let mut metadata = json!({"request_id":id,"expected_revision":revision});
            if let Some(contract) = &options.contract {
                metadata["contract"] = serde_json::to_string(contract)
                    .map_err(|_| Error::unavailable())?
                    .into();
            }
            cancellation
                .run(upload(
                    &client,
                    &connection,
                    Method::PUT,
                    Some(&reference),
                    metadata,
                    bytes,
                    id,
                ))
                .await
        }
    }
}
