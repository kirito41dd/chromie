#![cfg_attr(
    not(test),
    deny(clippy::string_slice, clippy::expect_used, clippy::unwrap_used)
)]
#![cfg_attr(not(test), forbid(unsafe_code))]

use chromie::{cli_setup, error::Result, shell_commands::Shell};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "chromie",
    version,
    about = "Chromie · 连接 Agent 与个人记忆空间",
    after_help = "开始使用：chromie init --url <服务地址> --space <空间>\n查看详细用法：chromie <命令> --help\n服务部署与管理请使用 chromie-server。"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 连接空间，配置 MCP 与消息采集
    Init(Init),
    /// 检查本机接入与服务连接
    Doctor(Doctor),
    /// 安装或修复 Agent 消息采集
    Hook {
        #[command(subcommand)]
        command: Hook,
    },
    /// 在本机安全录入或取用 Secret
    Secret {
        #[command(subcommand)]
        command: chromie::secret_cli::Command,
    },
    /// 上传 hook 输入的消息，离线时进入本机队列
    Capture(PrivateConfig),
    /// 补传本机队列中尚未发送的消息
    Flush(PrivateConfig),
    /// 显示将 chromie 加入 PATH 的方法
    Path {
        /// 当前使用的 Shell
        #[arg(long, value_enum, value_name = "Shell")]
        shell: Option<ShellKind>,
    },
    #[command(hide = true)]
    CodexSetup(LegacySetup),
    #[command(hide = true)]
    CodexCheck {
        #[command(flatten)]
        config: PrivateConfig,
        #[arg(long)]
        test_capture: bool,
    },
    #[command(hide = true)]
    Version,
}

#[derive(Clone, Copy, ValueEnum, Default)]
enum Agent {
    #[default]
    Codex,
    Generic,
}
#[derive(Clone, Copy, ValueEnum)]
enum ShellKind {
    Bash,
    #[value(alias = "nu")]
    Nushell,
}

#[derive(Args)]
struct PrivateConfig {
    /// 本机私有接入文件的绝对路径
    #[arg(long, value_name = "文件")]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct Init {
    /// 服务的 HTTP 或 HTTPS Origin
    #[arg(long, value_name = "地址", requires = "space")]
    url: Option<String>,
    /// 小写英文和连字符组成的空间标识
    #[arg(long, value_name = "空间", requires = "url")]
    space: Option<String>,
    /// 导入已有的本机私有接入文件
    #[arg(long, value_name = "文件", conflicts_with_all = ["url", "space", "reconnect"])]
    config: Option<PathBuf>,
    /// Codex 自动接入，或通用 MCP 接入
    #[arg(long, value_enum, default_value = "codex", value_name = "类型")]
    agent: Agent,
    /// Codex 配置目录
    #[arg(long, value_name = "目录")]
    codex_dir: Option<PathBuf>,
    /// 本机接入配置与队列目录
    #[arg(long, value_name = "目录")]
    data_dir: Option<PathBuf>,
    /// 重新配对并选择 Agent 接入
    #[arg(long)]
    reconnect: bool,
    /// 显式替换冲突配置
    #[arg(long)]
    replace: bool,
    /// 只打印配对链接，不自动打开浏览器
    #[arg(long)]
    no_browser: bool,
}

#[derive(Args)]
struct Doctor {
    #[command(flatten)]
    config: PrivateConfig,
    /// 选择本机接入配置类型
    #[arg(long, value_enum, default_value = "codex", value_name = "类型")]
    agent: Agent,
    /// Codex 配置目录，默认读取 CODEX_HOME 或 ~/.codex
    #[arg(long, value_name = "目录")]
    codex_dir: Option<PathBuf>,
    /// 本机接入配置目录
    #[arg(long, value_name = "目录")]
    data_dir: Option<PathBuf>,
    /// 上传一条虚构消息验证采集链路
    #[arg(long)]
    test_capture: bool,
}
#[derive(Subcommand)]
enum Hook {
    /// 为 Codex 安装或修复采集插件
    Install(HookInstall),
}
#[derive(Args)]
struct HookInstall {
    #[command(flatten)]
    config: PrivateConfig,
    /// Codex 配置目录
    #[arg(long, value_name = "目录")]
    codex_dir: Option<PathBuf>,
    /// 本机接入配置目录
    #[arg(long, value_name = "目录")]
    data_dir: Option<PathBuf>,
}
#[derive(Args)]
struct LegacySetup {
    #[arg(long, value_name = "文件")]
    config: PathBuf,
    #[arg(long, value_name = "目录")]
    codex_dir: Option<PathBuf>,
    #[arg(long, value_name = "目录")]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    replace: bool,
}

async fn run(command: Commands) -> Result<()> {
    let value = match command {
        Commands::Init(args) => {
            cli_setup::run(
                cli_setup::Action::Init,
                cli_setup::Options {
                    url: args.url,
                    space: args.space,
                    config: args.config,
                    generic: matches!(args.agent, Agent::Generic),
                    codex_dir: args.codex_dir,
                    data_dir: args.data_dir,
                    replace: args.replace,
                    reconnect: args.reconnect,
                    no_browser: args.no_browser,
                    ..Default::default()
                },
            )
            .await?
        }
        Commands::Doctor(args) => {
            cli_setup::run(
                cli_setup::Action::Doctor,
                cli_setup::Options {
                    config: args.config.config,
                    generic: matches!(args.agent, Agent::Generic),
                    codex_dir: args.codex_dir,
                    data_dir: args.data_dir,
                    test_capture: args.test_capture,
                    ..Default::default()
                },
            )
            .await?
        }
        Commands::Hook {
            command: Hook::Install(args),
        } => {
            cli_setup::run(
                cli_setup::Action::HookInstall,
                cli_setup::Options {
                    config: args.config.config,
                    codex_dir: args.codex_dir,
                    data_dir: args.data_dir,
                    ..Default::default()
                },
            )
            .await?
        }
        Commands::Secret { command } => chromie::secret_cli::run(command).await?,
        Commands::Capture(args) => {
            chromie::capture::run_with_config(false, args.config.as_deref()).await?
        }
        Commands::Flush(args) => {
            chromie::capture::run_with_config(true, args.config.as_deref()).await?
        }
        Commands::Path { shell } => {
            let shell = shell.map(|shell| match shell {
                ShellKind::Bash => Shell::Bash,
                ShellKind::Nushell => Shell::Nushell,
            });
            println!("{}", chromie::shell_commands::path_help(shell));
            return Ok(());
        }
        Commands::CodexSetup(args) => {
            let codex = args
                .codex_dir
                .map_or_else(chromie::codex_setup::default_codex_dir, Ok)?;
            let data = args
                .data_dir
                .map_or_else(chromie::codex_setup::default_data_dir, Ok)?;
            chromie::codex_setup::run(&args.config, &codex, &data, args.replace)?
        }
        Commands::CodexCheck {
            config,
            test_capture,
        } => {
            let path = config
                .config
                .or_else(|| std::env::var_os("CHROMIE_CLIENT_CONFIG").map(PathBuf::from))
                .map_or_else(
                    || chromie::codex_setup::default_data_dir().map(|dir| dir.join("client.json")),
                    Ok,
                )?;
            chromie::codex_check::run(&path, test_capture).await?
        }
        Commands::Version => {
            println!("chromie {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
    };
    println!("{value}");
    if value["execution"] == true
        && (value["outcome"] != "succeeded"
            || value["temporary_files_removed"] != true
            || value["audit_reported"] != true)
    {
        return Err(chromie::error::Error::invalid(
            "执行、清理或审计上报未全部成功，详见执行回执",
        ));
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli: Cli = chromie::command_line::parse();
    if let Err(error) = run(cli.command).await {
        eprintln!("{}：{}", error.code, error.message);
        std::process::exit(1);
    }
}
