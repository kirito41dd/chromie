---
name: chromie-setup
description: 用户希望连接 Chromie、启用 Codex 消息采集、更新接入配置或诊断连接时使用；不用于日常记忆召回与保存。
---

# Chromie 初始化与诊断

本 skill 只处理接入与诊断。日常记忆与 Secret 规则来自 Chromie MCP instructions 和按主题返回的 get_setup_guide，不重复注入教程。

## 统一 CLI 接入

1. 用户要求连接时，先取得公开的服务 Origin 与空间短标识。不要索取密码、密钥或私有配置正文。
2. 优先采用 Web 接入页的安装与 init 命令。首次浏览器配对由用户登录、核对确认码、选择接入并确认；密钥由 CLI 保存至私有文件，不经对话。不要默认要求下载凭据 JSON。
3. 若已经连上 MCP，调用 get_setup_guide(topic="setup") 获取当前步骤。CLI 已有同服务同空间接入时默认复用；重新选择用 init --reconnect。已有配置冲突先由用户确认，非交互覆盖才显式 --replace，不删除其他 MCP 或重建根私钥。
4. 以下命令在 Bash 与 Nushell 中相同；CLI 不在 PATH 时先按接入页说明安装或配置 PATH：

```shell
chromie doctor
chromie hook install
```

5. 用户在 Codex /hooks 审核信任后，重载或新开对话。不得修改信任记录、绕过审核或另注册一套重复 hook。旧手工 hook 由用户确认停用。
6. 工具发现、测试上传与真实 hook 触发分别核对。用户同意时可用 doctor --test-capture 上传明确标注的虚构消息，再发送真实新消息，核对 Web“采集消息”页的 Agent、角色与接收时间。

旧插件包的 bin/chromie 保留兼容；直接调用包内程序前核对 BUILD.json 的平台。已有私有文件导入可用 chromie init --config 绝对路径，不读取或打印文件正文。默认配置为 ~/.config/chromie/codex/client.json，队列在相邻 queue/，私有文件不打进插件包或源码。

## 按主题诊断

- capture：消息采集与通用 stdin 适配。Bash 暂停变量为 export CHROMIE_CAPTURE=off；Nushell 为 $env.CHROMIE_CAPTURE = 'off'。不擅自改动用户的原始采集策略。
- diagnostics：连接、权限、重试和召回为空的排查。MCP 与 hook 密钥分开，空间密码不替代 Agent 密钥；不读取凭据原文排障。
- secret：先读取当前 Secret 权限与 CLI 指南。尚未提供原文时引导用户终端隐藏输入或已有私有文件；已主动提供且明确要求代存时，Agent 通过私有临时文件与 secret put --file 直接保存，不要求重复录入。使用内置 secret put/get/update，不依赖源码或 Python 示例；MCP 参数、公开说明和执行输出不包含原文。
