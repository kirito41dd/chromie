# Chromie Codex 插件

本插件面向本机运行的 Codex，提供初始化指导、消息采集和 `chromie-memory` 日常使用 Skill。MCP 提供实时能力与关键边界，Skill 按需补充组织记忆、主题导航和 Secret 工作流。

## 推荐安装

在 Web 接入页复制安装和 init 命令。下面用 `https://chromie.example` 和 `personal` 示意，替换成自己的服务 Origin 与空间标识；安装器按运行命令的电脑选择 macOS／Linux、arm64／x86_64 程序，并校验 SHA-256。

Bash：

```bash
curl -fsS 'https://chromie.example/install.sh' | sh -s -- 'https://chromie.example'
"$HOME/.local/bin/chromie" init --url 'https://chromie.example' --space personal
```

Nushell：

```nu
^curl -fsS 'https://chromie.example/install.sh' | ^sh -s -- 'https://chromie.example'
^($nu.home-dir | path join '.local' 'bin' 'chromie') init --url 'https://chromie.example' --space personal
```

在浏览器登录空间，核对确认码并明确选择接入。CLI 将凭据保存在本机私有文件，配置 MCP 并安装包含 setup、日常记忆 Skill 与 hook 的 `chromie@chromie-cli` 插件；保留其他 MCP 和注释。密钥不经对话传递，也不用手工下载 JSON。

完全退出并重新打开 Codex，在 `/hooks` 审核信任 Chromie hook 后发送新消息。不要同时保留旧手动 hook 与重复插件；安装器不代替用户修改信任记录。

## 更新

重新运行上面的安装命令下载新 CLI，然后运行 init 刷新本机插件、Skill 和采集程序。同一接入默认复用已有 Agent 与凭据，不需要重新配对。

Bash：

```bash
"$HOME/.local/bin/chromie" init
```

Nushell：

```nu
^($nu.home-dir | path join '.local' 'bin' 'chromie') init
```

更新后重启 Codex，再开始新对话；已打开对话可能仍使用旧工具或 Skill 快照。需要重新选择 Agent 时才使用 `init --reconnect`。

## 验证

以下命令在 Bash 与 Nushell 中相同；需先把 CLI 加入 PATH（方法见 `chromie path`）：

```shell
chromie doctor
chromie doctor --test-capture
```

doctor 检查 MCP 初始化、工具发现和插件文件。测试采集仅上传明确标注的虚构事件，不能证明真实 hook 已生效；`native_hook_verified=false` 不能解读为上传失败，也不能据此宣称真实采集已通过。真实触发须在审核信任、新消息之后，核对 Web 来源与接收时间。原始采集不会自动提取长期记忆。

默认接入配置在 `~/.config/chromie/codex/client.json`，权限 0600；离线队列在相邻 `queue/`，权限 0700。`CHROMIE_CLIENT_CONFIG` 可指定另一份私有文件，`CHROMIE_CAPTURE=off` 暂停采集。

日常记忆 Skill 与 MCP 的分工见[源码仓库的开发 Wiki](https://github.com/kirito41dd/chromie/tree/main/wiki)。需要自行导出给其他支持 Skill 的 Agent，可用 `chromie skill export --out ./chromie-memory`（Bash／Nushell 通用）。

含 hook 的包走本地／仓库 marketplace；手工 ZIP 包需要先核对 BUILD.json 的 target，不代表同时包含各平台程序。高级手动安装和源码构建见[源码仓库的使用文档](https://github.com/kirito41dd/chromie/tree/main/docs)。
