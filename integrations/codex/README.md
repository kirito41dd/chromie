# Chromie Codex 插件

本插件面向本机运行的 Codex，提供初始化指导和消息采集。日常记忆使用规则直接来自 MCP，不另安装记忆 skill。

## 安装

从 Chromie Web 接入页下载插件包与私有接入 JSON，解压插件到长期保留目录。先检查 `BUILD.json` 的 target 与运行 Codex 的电脑一致；当前构建包不代表同时支持其他平台。

以下命令在 Bash 与 Nushell 中相同：

```shell
codex plugin marketplace add "/解压后的绝对路径/chromie-codex"
codex plugin add chromie@chromie-local
```

安装后启动新会话，运行插件 setup 或说“帮我连接 Chromie，启用消息采集”，只提供私有 JSON 的文件路径。初始化会合并用户级 HTTP MCP 配置，保留其他服务与注释。插件包本身没有个人凭据，也不通过对话返回密钥。

可直接执行同一初始化程序：

Bash：

```bash
chmod 600 "/私有目录/chromie-client.json"
"/插件绝对路径/plugin/bin/chromie" codex-setup --config "/私有目录/chromie-client.json"
"/插件绝对路径/plugin/bin/chromie" codex-check
```

Nushell：

```nu
^chmod 600 "/私有目录/chromie-client.json"
^"/插件绝对路径/plugin/bin/chromie" codex-setup --config "/私有目录/chromie-client.json"
^"/插件绝对路径/plugin/bin/chromie" codex-check
```

在 `/hooks` 审核信任插件 hook，再按客户端提示重载 MCP／新建对话。不要同时保留旧手动 Chromie hook 与插件 hook，避免重复采集。插件不改信任记录，不绕过客户端审核。

## 验证

`codex-check` 检查 MCP 初始化和工具发现。`codex-check --test-capture` 额外上传一条标记为虚构的测试事件，仅证明 HTTP 上传链路可用。真实 hook 是否触发，须在新消息后核对 Web 时间与来源。原始采集不会自动提取长期记忆。

默认接入配置在 `~/.config/chromie/codex/client.json`，权限 0600；离线队列在相邻 `queue/`，权限 0700。`CHROMIE_CLIENT_CONFIG` 可指定另一份私有文件，`CHROMIE_CAPTURE=off` 暂停采集。

含 hook 的包按 Codex 官方规则走本地／仓库 marketplace，不通过公共插件目录发布。源码构建见项目 `docs/Codex插件接入实施.md`。
