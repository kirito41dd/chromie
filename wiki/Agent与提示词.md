# Agent 接入与提示词分工

[返回 Wiki](README.md) · [关系](记忆与关系.md) · [接入使用手册](../docs/Codex接入与使用.md)

Agent 通过 MCP 操作空间记忆，本机 CLI 负责接入、hook 采集和 Secret 原文通道。后端文本整理使用自己的固定系统说明，不加载客户端 Skill，也不连接外部 Agent 执行环境。

## MCP、Skill 与按需指南

| 层 | 负责内容 | 当前载体 |
| --- | --- | --- |
| MCP 初始化 instructions | 服务定位、主动使用记忆的时机、关键安全边界与能力发现 | [mcp_instructions.md](../src/mcp_instructions.md) |
| 工具说明与参数 Schema | 当前可调用能力、字段、修订、幂等和错误约定 | [mcp.rs](../src/mcp.rs)、[types.rs](../src/types.rs)、[knowledge.rs](../src/knowledge.rs) |
| `chromie-memory` Skill | 接续任务、写好记忆、组织主题和本机 Secret 工作流 | [Skill 入口](../integrations/codex/plugin/skills/memory/SKILL.md)及其 references |
| `chromie-setup` Skill | 本机安装接入与诊断 | [setup](../integrations/codex/plugin/skills/setup/SKILL.md) |
| `get_setup_guide` | 按主题提供当前接入权限与安装／Secret／采集／诊断指南 | [MCP 接口文档](../docs/MCP接口设计.md) |

有 Skill 后，详细方法可以按需展开；常驻 MCP 说明仍应让未安装 Skill 的通用接入可用。来源不能编造、记忆不构成执行授权、Secret 原文不进普通 MCP 参数等关键边界不能只藏在 Skill 中。

当前 instructions 已精简为定位、能力发现、来源／授权和 Secret 关键边界。检索策略与接续细节放入 Skill 的 [retrieval.md](../integrations/codex/plugin/skills/memory/references/retrieval.md)，主题组织与 Secret 本机流程按需读取对应参考文件；工具说明继续表达具体参数约定。分发成功不等于客户端一定加载过 Skill。

## 运行时能力优先

`get_capabilities` 当前返回 capability_version=2，包含检索模式、语义状态及 knowledge 的引用、反向引用、主题导航、关系数上限和 background_llm 标记。它是有限缓存的业务状态快照，不是网络探活或相关性保证。

Agent 据此选择关键词／混合检索策略，再以本次 recall 的实际状态为准。Skill 不保存地址、Key 或一份永久有效的能力表；连接旧服务时按其真实工具与 Schema 使用，不能因新 Skill 中提到某能力就假定已上线。

新增的 `get_navigation`、`get_memory_links` 属于只读入口；`remember`／`correct_memory` 扩展类型与关系字段。文本整理任务目前由 Web HTTP 创建，没有 MCP 整理工具。完整工具列表见 [MCP 接口设计](../docs/MCP接口设计.md)。

## Skill 的分发

Skill Markdown 与参考文件通过 include_bytes 编入 CLI；Codex 插件安装时写出同一份内容并在 manifest 注册 skills 目录。无个人凭据的插件包也包含这些文件，避免另维护一套提示词副本。

其他支持 Skill 的客户端可自行导入，以下命令在 Bash 与 Nushell 中相同：

```shell
chromie skill export --out ./chromie-memory
```

导出入口解析相对路径，检查目标目录和文件；已有不同内容时拒绝覆盖，显式 `--replace` 才替换。不自动修改个人 AGENTS.md。更新 CLI 后通过 `chromie hook install` 更新本机插件，客户端重载方式见接入手册。

## Hook 与使用记忆分开

hook 调用 `chromie capture`，必要时转换客户端事件输入；UUID、鉴权、上传、离线队列和补传由 CLI 统一处理。MCP 密钥与 hook 上传密钥分用途保存，不使用空间密码作为替代。

采集形成 `source_events`，不会自动触发 recall 索引或消息提炼。Agent 主动 remember 与用户在 Web 发起 extract 是两种形成正式记忆的路径。Skill 负责说明怎样配合，不在后台持续监听对话。

## 实现与验证

[安装器](../src/cli_setup.rs)、[Skill 导出](../src/client_skills.rs)、[插件打包](../scripts/build-codex-plugin.py)、[能力快照](../src/store/capabilities.rs)。接入生命周期见 [cli_pairing.rs](../tests/cli_pairing.rs)，MCP 工作流见 [workflow.rs](../tests/workflow.rs)。静态文件能打包与真实模型是否遵循工作流分别验证，不能互相代替。

安装升级与两个真实客户端会话的有限观察见 [真实 Skill 接续验收](真实模型验证.md#客户端安装与真实-skill-接续验收)，其中分别说明通过项、执行器权限失败与尚未验证的客户端范围。
