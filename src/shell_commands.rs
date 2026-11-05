//! Web 与 CLI 共用的交互 Shell 命令；不读取环境变量或凭据。
#![cfg_attr(not(test), forbid(unsafe_code))]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Nushell,
}

impl Shell {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bash" => Some(Self::Bash),
            "nushell" | "nu" => Some(Self::Nushell),
            _ => None,
        }
    }
    pub fn binary(self) -> &'static str {
        match self {
            Self::Bash => "\"$HOME/.local/bin/chromie\"",
            Self::Nushell => "^($nu.home-dir | path join '.local' 'bin' 'chromie')",
        }
    }
    pub fn path_line(self) -> &'static str {
        match self {
            Self::Bash => "export PATH=\"$HOME/.local/bin:$PATH\"",
            Self::Nushell => {
                "$env.PATH = ($env.PATH | prepend ($nu.home-dir | path join '.local' 'bin') | uniq)"
            }
        }
    }
    pub fn path_note(self) -> &'static str {
        match self {
            Self::Bash => {
                "当前终端执行上面一行即可。永久生效可将它加入 ~/.bashrc；macOS 登录 Bash 使用 ~/.bash_profile，Zsh 使用 ~/.zshrc。重开终端后验证 chromie --version。"
            }
            Self::Nushell => {
                "当前终端执行上面一行即可。永久生效可运行 config nu，将它加入 config.nu（路径由 $nu.config-path 给出），重开终端后验证 chromie --version。"
            }
        }
    }
    pub fn install(self, origin: &str) -> String {
        match self {
            Self::Bash => format!("curl -fsS '{origin}/install.sh' | sh -s -- '{origin}'"),
            Self::Nushell => format!("^curl -fsS '{origin}/install.sh' | ^sh -s -- '{origin}'"),
        }
    }
    pub fn initialize(self, origin: &str, space: &str, codex: bool) -> String {
        format!(
            "{} init --url '{origin}' --space '{space}' --agent {}",
            self.binary(),
            if codex { "codex" } else { "generic" }
        )
    }
    pub fn repair(self, codex: bool) -> String {
        let bin = self.binary();
        if codex {
            format!("{bin} doctor\n{bin} hook install\n\n# 重新选择接入\n{bin} init --reconnect")
        } else {
            format!(
                "{bin} doctor --agent generic\n\n# 重新选择接入\n{bin} init --agent generic --reconnect"
            )
        }
    }
    pub fn capture(self, codex: bool) -> String {
        let directory = if codex { "codex" } else { "generic" };
        let bin = self.binary();
        let command = match self {
            Self::Bash => format!(
                "CHROMIE_QUEUE_DIR=\"$HOME/.config/chromie/{directory}/queue\" {bin} capture --config \"$HOME/.config/chromie/{directory}/client.json\""
            ),
            Self::Nushell => format!(
                "with-env {{CHROMIE_QUEUE_DIR: ($nu.home-dir | path join '.config' 'chromie' '{directory}' 'queue')}} {{ {bin} capture --config ($nu.home-dir | path join '.config' 'chromie' '{directory}' 'client.json') }}"
            ),
        };
        format!(
            "{command}\n\n# stdin JSON 示例；最终回复使用 assistant\n# {{\"session_id\":\"会话ID\",\"role\":\"user\",\"body\":\"消息正文\"}}"
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn path_help(shell: Option<Shell>) -> String {
    let selected = match shell {
        Some(shell) => vec![shell],
        None => vec![Shell::Bash, Shell::Nushell],
    };
    selected
        .into_iter()
        .map(|shell| {
            format!(
                "{}：\n{}\n{}\n",
                if shell == Shell::Bash {
                    "Bash / Zsh"
                } else {
                    "Nushell"
                },
                shell.path_line(),
                shell.path_note()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
