#![cfg_attr(
    not(test),
    deny(clippy::string_slice, clippy::expect_used, clippy::unwrap_used)
)]
#![cfg_attr(not(test), forbid(unsafe_code))]

use chromie::{
    config::Config,
    error::{Error, Result},
    search::Search,
    space_key::SpaceKey,
    store::Store,
    web_session::WebSessions,
};
use clap::{Parser, Subcommand};
use migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "chromie-server",
    version,
    about = "Chromie Server · 服务部署与空间管理",
    after_help = "容器部署：docker compose up -d
Agent 接入、消息采集与 Secret 取用请使用 chromie。"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// 启动服务，读取现有环境或持久配置
    Serve,
    /// 执行 SeaORM 数据库迁移
    Migrate,
    /// 首次创建实例根私钥，拒绝覆盖原文件
    InitKey {
        #[arg(value_name = "私钥路径")]
        path: PathBuf,
    },
    /// 新建记忆空间，密码从标准输入读取
    CreateSpace {
        #[arg(value_parser = space, value_name = "空间")]
        space: SpaceKey,
        #[arg(value_name = "名称")]
        name: String,
    },
    /// 修改空间标识
    RenameSpace {
        #[arg(value_name = "原标识")]
        old: String,
        #[arg(value_parser = space, value_name = "新标识")]
        new: SpaceKey,
    },
    /// 修改空间密码，新密码从标准输入读取
    SetPassword {
        #[arg(value_parser = space, value_name = "空间")]
        space: SpaceKey,
    },
    /// 将空间的检索索引重建任务加入队列
    Rebuild {
        #[arg(value_parser = space, value_name = "空间")]
        space: SpaceKey,
    },
    /// 容器首次初始化，保留已有配置、私钥和数据
    ContainerInit,
    /// 使用 /config 持久配置启动容器服务
    ContainerServe,
    /// 仅在自己的终端查看初始登录信息
    InitialLogin,
    #[command(hide = true)]
    Healthcheck,
}
fn space(raw: &str) -> std::result::Result<SpaceKey, String> {
    SpaceKey::parse(raw).ok_or_else(|| "空间标识须为小写英文，可用单个连字符分隔".into())
}
fn password() -> Result<String> {
    let mut bytes = String::new();
    std::io::stdin()
        .take(1027)
        .read_to_string(&mut bytes)
        .map_err(|_| Error::invalid("无法从标准输入读取密码"))?;
    if bytes.len() > 1026 {
        return Err(Error::invalid("密码输入超过上限"));
    }
    Ok(bytes.trim_end_matches(['\r', '\n']).to_owned())
}

async fn container_serve() -> Result<()> {
    let config_dir = Path::new("/config");
    let _lock = chromie::deployment::lock(config_dir)?;
    let deployment = chromie::deployment::Deployment::load(config_dir)?;
    let config = deployment.runtime_config()?;
    let mut options = ConnectOptions::new(config.database_url.clone());
    options
        .max_connections(8)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(10))
        .sqlx_logging(false);
    let mut connected = None;
    for _ in 0..60 {
        match Database::connect(options.clone()).await {
            Ok(db) => {
                connected = Some(db);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    let db = connected
        .ok_or_else(|| Error::invalid("等待 MySQL 就绪超时，请检查容器状态；未输出数据库凭据"))?;
    migration::Migrator::up(&db, None).await?;
    let key = chromie::server_key::ServerKey::load(&config.key_file)?;
    let sessions = WebSessions::from_key(&key)?;
    let store = Store::new(db, Search::new(&config.qdrant_url, config.qdrant_key)?);
    let store = store
        .enable_secrets(chromie::secret_crypto::SecretCipher::from_key(&key)?)
        .await?;
    deployment.ensure_initial_space(&store).await?;
    drop(_lock);
    // 容器监听与外部 Origin 分别配置；默认 Compose 只发布到宿主回环。
    // 外部 Origin 可为 HTTP 或 HTTPS，TLS 由部署和网关决定。
    chromie::server::serve(store, config.listen, config.public_origin, sessions).await
}

async fn run(command: Commands) -> Result<()> {
    match command {
        Commands::ContainerInit => {
            let created =
                chromie::deployment::initialize(Path::new("/config"), Path::new("/data"))?;
            println!(
                "{}",
                if created {
                    "首次配置已生成；初始空间 personal，登录信息位于 config/initial-space.txt，请安全备份 config 与 data"
                } else {
                    "已核对原实例配置，保留现有根私钥和凭据"
                }
            );
            return Ok(());
        }
        Commands::ContainerServe => return container_serve().await,
        Commands::InitialLogin => {
            print!(
                "{}",
                chromie::deployment::initial_login(Path::new("/config"))?
            );
            return Ok(());
        }
        Commands::Healthcheck => {
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(4))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| Error::unavailable())?
                .get("http://127.0.0.1:8732/ready")
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !response.status().is_success() {
                return Err(Error::unavailable());
            }
            return Ok(());
        }
        Commands::InitKey { path } => {
            chromie::server_key::ServerKey::initialize(&path)?;
            println!("P-256 实例根私钥已持久保存；请独立安全备份，未覆盖已有文件");
            return Ok(());
        }
        _ => {}
    }
    let config = Config::load()?;
    let mut options = ConnectOptions::new(config.database_url);
    options
        .max_connections(8)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(10))
        .sqlx_logging(false);
    let db = Database::connect(options).await?;
    if matches!(command, Commands::Migrate) {
        migration::Migrator::up(&db, None).await?;
        println!("数据库迁移已完成");
        return Ok(());
    }
    let store = Store::new(db, Search::new(&config.qdrant_url, config.qdrant_key)?);
    match command {
        Commands::CreateSpace { space, name } => {
            store.create_space(space.clone(), name, password()?).await?;
            println!("{}", serde_json::json!({"space_id":space.as_str()}));
        }
        Commands::RenameSpace { old, new } => {
            store.rename_space(&old, new.clone()).await?;
            println!("{}", serde_json::json!({"space_id":new.as_str()}));
        }
        Commands::SetPassword { space } => {
            let id = store.resolve_space(&space).await?.as_uuid();
            store.admin_set_password(id, password()?).await?;
            println!("空间密码已更新");
        }
        Commands::Rebuild { space } => {
            let id = store.resolve_space(&space).await?.as_uuid();
            println!(
                "{}",
                serde_json::json!({"queued":store.admin_rebuild(id).await?})
            );
        }
        Commands::Serve => {
            if !config.listen.ip().is_loopback() && config.public_origin.is_none() {
                return Err(Error::invalid(
                    "非本机监听必须显式配置外部访问 Origin（HTTP 或 HTTPS）",
                ));
            }
            let key = chromie::server_key::ServerKey::load(&config.key_file)?;
            let sessions = WebSessions::from_key(&key)?;
            let store = store
                .enable_secrets(chromie::secret_crypto::SecretCipher::from_key(&key)?)
                .await?;
            chromie::server::serve(store, config.listen, config.public_origin, sessions).await?;
        }
        _ => return Err(Error::invalid("此命令已在启动阶段处理")),
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli: Cli = chromie::command_line::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("chromie=info"))
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = run(cli.command).await {
        eprintln!("{}：{}", error.code, error.message);
        std::process::exit(1);
    }
}
