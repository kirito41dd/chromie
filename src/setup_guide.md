推荐通过 Web 接入页的 CLI 指引安装。页面可选择 Bash 或 Nushell，命令预览和复制会同时切换。创建接入只需要名称；日常记忆规则由 MCP 提供，不需要另装记忆 Skill。

这里只讲初始化。Secret 录入／取用请调用 get_setup_guide(topic="secret")，采集用 capture，排障用 diagnostics；会同时返回当前接入的 Secret 权限状态。CLI 已内置 secret put/get/update，无需用户机器存在项目源码或 Python 示例。

首次安装（服务地址可使用 HTTP 或 HTTPS，TLS 由部署和网关决定）：

Bash：

```bash
curl -fsS 'http://127.0.0.1:8732/install.sh' | sh -s -- 'http://127.0.0.1:8732'
"$HOME/.local/bin/chromie" init --url 'http://127.0.0.1:8732' --space demo --agent codex
```

Nushell：

```nu
^curl -fsS 'http://127.0.0.1:8732/install.sh' | ^sh -s -- 'http://127.0.0.1:8732'
^($nu.home-dir | path join '.local' 'bin' 'chromie') init --url 'http://127.0.0.1:8732' --space demo --agent codex
```

示例 Origin、demo 空间必须替换为当前服务与空间；首次安装自动选择本机平台并核对 SHA-256，缺少对应产物会明确失败。当前自动安装入口为 macOS／Linux GNU；Windows 暂未支持。旧插件包与私有 JSON 导出只作兼容。

CLI 在浏览器打开五分钟配对。用户登录空间、核对终端确认码，选择或创建本机接入并明确确认。Agent 可以启动命令并提供公开链接，不代替用户点击确认。密钥由 CLI 直接领取并保存为 0600 私有文件，不要求粘贴到对话，也不回显。

重复 init 默认复用同服务、同空间的本机接入。重新选择使用 --reconnect，已有配置在浏览器授权前交互确认；非交互更新须用户明确同意并显式 --replace。不能默认给所有命令加 --replace。私有 JSON 可用 init --config <绝对路径> 导入，TOML 和输入在授权前预检，保留其他 MCP、注释和私有备份。

加入当前终端 PATH（持久保存由用户编辑对应启动文件）：

Bash / Zsh：

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Nushell：

```nu
$env.PATH = ($env.PATH | prepend ($nu.home-dir | path join '.local' 'bin') | uniq)
```

Bash 的持久配置一般是 ~/.bashrc，macOS 登录 Bash 使用 ~/.bash_profile，Zsh 使用 ~/.zshrc。Nushell 运行 config nu，把上面一行加入 config.nu；其路径由 $nu.config-path 给出。CLI 的 path --shell bash|nushell 可再次查看指引。

加入 PATH 后，两种 Shell 使用相同命令：

```shell
chromie init
chromie init --reconnect
chromie doctor
chromie hook install
```

Codex 初始化通过官方 plugin 命令安装 chromie@chromie-cli 的 UserPromptSubmit／Stop hook。用户在 /hooks 审核信任后重载或新开对话；CLI 不更改信任记录。旧手工 Chromie hook 或 chromie-local 插件应由用户停用以免重复采集。

通用接入的 init 添加 --agent generic，私有配置默认在 ~/.config/chromie/generic/client.json。客户端按自身格式配置 Streamable HTTP MCP，使用本地文件中的 MCP 密钥；hook 调用 chromie capture，由客户端通过 stdin 提交消息。内部固定 sh hook 不受用户交互 Shell 影响。

通用采集命令：

Bash：

```bash
CHROMIE_QUEUE_DIR="$HOME/.config/chromie/generic/queue" "$HOME/.local/bin/chromie" capture --config "$HOME/.config/chromie/generic/client.json"
```

Nushell：

```nu
with-env {CHROMIE_QUEUE_DIR: ($nu.home-dir | path join '.config' 'chromie' 'generic' 'queue')} { ^($nu.home-dir | path join '.local' 'bin' 'chromie') capture --config ($nu.home-dir | path join '.config' 'chromie' 'generic' 'client.json') }
```

兼容 UserPromptSubmit／Stop 输入时直接交给 bin；不同字段只转换为 {"session_id":"会话ID","role":"user 或 assistant","body":"正文"}，可选 turn_id、project_key。bin 负责 UUID、时间、独立 hook 密钥、上传、私有队列及 flush 补传；正文最多 60,000 字节，其他标识最多 128 字节，通用 stdout 仅为 {}。

doctor --test-capture 会上传明确标注的虚构消息，只能证明 HTTP 上传链路。必须通过真实客户端发送新消息，并核对 Web 来源和上传时间，才能确认真实 hook 触发。Bash 暂停变量为 export CHROMIE_CAPTURE=off，Nushell 为 $env.CHROMIE_CAPTURE = 'off'。

凭据不进入 URL、命令参数、模型上下文、截图或日志；本机全权限程序仍可能主动读取私有文件。Secret 原文只经本地帮助程序的独立 HTTP 通道进入私有文件，不交给 MCP 工具。服务端持久根私钥禁止因安装、更新或重启重建。
