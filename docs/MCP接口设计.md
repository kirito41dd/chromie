# MCP 接口设计

当前使用官方 rmcp + Axum 提供八个工具：五个核心记忆工具，以及检索能力查询、召回反馈和分主题指南。MCP／hook 使用每 Agent 独立密钥，见 [Agent 接入设计](Agent接入设计.md)。正式记忆支持 BM25 和可选 embedding 混合召回，后台自动提取尚未实现。运行与验证方法见 [开发运行](开发运行.md)。

## 参考与边界

| 参考项目 | 吸收的思路 |
| --- | --- |
| [Signet](https://docs.signetai.sh/mcp/) | hooks 生命周期采集与 MCP 操作分工；先召回少量内容，再展开来源；支持修改与遗忘 |
| [Hindsight](https://hindsight.vectorize.io/developer/mcp-server) | 区分写后可读和后台处理；控制召回预算；深入归纳独立于常规检索 |
| [Mem0](https://github.com/mem0ai/mem0/blob/main/docs/platform/mem0-mcp.mdx) | 少量直接的保存、搜索、详情、更新和删除工具 |

以上属于接口研究，没有部署对比测评。Chromie 的空间边界、一致性和来源清理是本项目要求，不能据此宣称参考项目有同样保证。

```text
Agent ── MCP 记忆工具 ──────────┐
Codex hooks → 本地命令 → HTTP ─┤→ 空间鉴权 → MySQL 权威记忆 / 原始证据
                             └→ 持久化索引任务 → BM25 / 可选语义索引
```

MCP 地址为 `/spaces/{空间标识}/mcp`，初始化、发现和调用均校验当前 Agent 的 MCP Bearer 密钥，采集入口使用独立 hook 密钥。端点固定空间，工具参数不接受密码或 `space_id`；项目范围只是空间内筛选。原始事件经 POST `/spaces/{空间标识}/events` 上传，不消耗 MCP 工具调用。

MCP 不监听完整对话，也不自动把记忆注入每个回合。当前没有背景 HTTP 接口、话题摘要、`reflect`、resources、prompts、订阅或 MCP Tasks。Slint 看板与手动召回已作为独立 Web 页面实现；Secret 专用 HTTP 已实现，普通工具只处理安全说明和引用，不录入或输出凭据原文。接入和日常使用见 [Codex 接入与使用](Codex接入与使用.md)。

## 五个记忆工具与实际参数

| 工具 | 输入 | 行为与输出 |
| --- | --- | --- |
| `recall` | `query`；可选 `project_key`、`max_chars` | 返回最多 10 条已保存记忆及 `index_available`、`partial`、`truncated` |
| `get_memory` | `memory_id` 或 `event_id` 二选一；可选 `view`、`cursor`、`max_chars` | 当前正文、修订历史或有限来源；分页时返回 `next_cursor` |
| `remember` | `content`、`request_id`；可选 `scope`、`source_refs` | 事务提交后返回 `memory_id`、`revision`、`readable` 与 `processing_state` |
| `correct_memory` | `memory_id`、`expected_revision`、`request_id`、`reason`；可选 `content`、`scope`、`source_refs` | 至少修改正文或范围一项，追加修订，返回新版本；旧版本修改发生 `revision_conflict` |
| `forget_memory` | `memory_id` 或 `event_id` 二选一、`request_id`；记忆目标另需 `expected_revision` | 清理并隐藏内容，返回 `hidden`、`purge_state` 与 `affected_memories` |

其余三个工具：

| 工具 | 用途 |
| --- | --- |
| `get_capabilities` | 首次检索前查询关键词／混合模式、语义状态与配置修订；不调用模型，不返回地址或密钥 |
| `feedback_recall` | 按 recall_id 批量反馈实际评价过的条目与其中有用的条目；不能提交未返回的记忆或代其他 Agent 评价 |
| `get_setup_guide` | 按 setup、capture、secret、diagnostics 主题读取指南与权限，可携带已知指南版本减少静态正文 |

ID 与 `request_id` 都是 UUID。`request_id` 是业务幂等键，不是 JSON-RPC ID；同一次写重试必须复用，参数改变却复用同一键会发生 `idempotency_conflict`。冲突后重新读取判断，不自动换成最新修订覆盖。更正可替换来源，也可保留上一修订来源；只改来源而不改正文 / 范围不是当前支持的操作。

`scope` 默认 `{"kind":"global"}`，项目范围为 `{"kind":"project","project_key":"chromie"}`。这里的 global 仅指当前空间通用，不跨空间。已知项目的 recall 返回通用内容与该项目内容；不指定项目时可命中本空间其他项目，结果会带 `project_key`。

记忆正文最多 16,000 字节，更正理由最多 4,000 字节，来源最多 16 个事件 ID。查询最多 256 字节；项目、会话、轮次标识最多 128 字节。参数结构拒绝未知字段，最终 JSON Schema 由实际 DTO 与 rmcp 生成。UUID 字段及来源数组元素携带 `format: uuid`，召回／详情预算携带 1024～8000 的上下限，避免模型误用文字幂等键或超大预算；服务仍独立校验输入。Secret 的安全说明另限 256 字节，原文使用独立 HTTP。

### recall：只找已保存记忆

当前是明确关键词检索，例如 `Chromie MySQL`，不承诺换一种自然说法仍能命中。服务先按空间检索，再校验 MySQL 当前修订和遗忘状态；已更正的旧索引候选不返回旧内容。默认不搜索、夹带或自动归纳全量原始对话，没有记忆就返回空结果。

待索引的新增 / 更正记录有大小写归一后的关键词子串兜底，最多检查 500 条。索引不可用时只检查最近 500 条有效记忆，返回 `index_available: false`、`partial: true`；不把有限兜底说成完整检索。Qdrant 每次取最多 30 个候选，最终最多返回 10 条，因此结果不是匹配总数或全量枚举。

卡片带 ID、修订、内容、适用项目、时间、来源引用与处理状态。`source_linked` 只表示关联了原始证据，**不表示用户确认或执行验证**；没有来源则为 `unverified`。当前没有模型置信度、自动冲突发现或自动话题摘要。

### get_memory：按引用展开并分页

`view` 默认为 `current`；另有 `history`、`evidence`：

- `current` 返回正文，正文游标为 `修订号:字符偏移`。续页固定同一修订，即使中途更正也不会拼接不同版本；历史版本标为 `is_current: false`。
- `history` 按修订倒序分页，游标为上一页最后的修订号。条目带 `content_cursor`，可用 current 视图展开该历史正文，不重新视为当前结论。
- `evidence` 读取当前修订关联的来源预览，按事件 ID 分页；再用 `event_id` 读取明确的原始事件，游标为字符偏移。不搜索或附送相邻整段会话。

历史与证据每页最多 10 项，预算可能进一步减少。已遗忘记忆只返回 ID 与隐藏标记；已遗忘来源不可取回。证据保留原始角色、时间与会话定位，历史文本不成为当前执行授权。

`max_chars` 默认 3000，范围 1024～8000，按序列化 JSON 的 Unicode 字符数计算，并预留 MCP 封装空间。元数据与正文共同占预算，不是 token 计数。结果通过 `structuredContent` 返回，不再在 `content` 里复制一份全文；预算不足时标记截断并保留 ID / 可展开游标，单条元数据都放不下时明确报错。

### remember / correct_memory：先持久化，再同步索引

一个 MySQL 事务写入记忆、修订、来源关系、幂等回执和索引任务，成功后按 ID 可读。`processing_state: index_pending` 只描述索引状态，不表示内容已经核实或后台提取完成。

每空间的写入和索引发布共用数据库行锁，当前选择串行写来简化一致性。后台每次重新读取权威头部，不信任旧任务的正文；同一记忆的任务合并为最新目标，失败重试、重启继续处理。索引状态可由 `get_memory` 查看，本地 `rebuild` 可重新登记任务。没有引入 Redis 或额外任务服务。

成功写入后新发起的读取按当前修订处理，已经进入其他对话的旧上下文无法撤回。幂等回执保留 ID、参数摘要和修订，不复制正文；遗忘后重试会反映隐藏状态，不重新写回旧内容。

### forget_memory：明确目标，清理来源

只接受已定位的记忆或原始事件，不提供自然语言批量清空。是否符合用户意图由宿主与接入流程保证，记忆文本或模型填写的确认字段不构成执行授权。服务校验接入权限、空间、目标和修订。

事务内隐藏目标、清空相关修订正文 / 更正理由 / 项目字段与对应原始正文；若为 Secret，同时清空密文并禁止后续取用，再登记索引删除任务。返回 `purge_state: index_pending`，不提前声称索引已清理。MVP 以整条事件作为来源单位；遗忘记忆时清理其所有历史来源，共享这些来源的其他记忆也会隐藏，故 `affected_memories` 可能超过一条。不会递归删除这些其他记忆的全部独立来源。

保留无正文的删除标记和幂等记录，旧事件补传、旧写请求及旧任务不能恢复在线内容。保证范围是 Chromie 管理的在线数据；数据库备份、binlog、本地暂存和外部客户端副本没有随回执消失。

## Secret 配合方式

五工具保持不变。Secret 必须通过独立 HTTP 原子创建密文与安全说明；`remember` 仍用于普通记忆。`recall` 和 `get_memory` 对 Secret 返回 `kind: secret_ref`、`secret_ref`、`fetch_path` 与说明，不返回原文或密文。公开说明可进入关键词与语义检索，原文不进入来源、索引或 MCP 响应。

`correct_memory` 仅修改安全说明、范围和理由，不允许挂接原始对话来源；凭据更新使用 HTTP PUT。`forget_memory` 同时清空在线密文，旧请求重试不会复活内容。本地帮助程序使用已开启 Secret 权限的 MCP 密钥取用，将 HTTP 响应直接写私有文件，Web Cookie 不能代替鉴权。流程与命令见 [Secret 记忆](Secret记忆.md)。

## 错误与验证

业务错误使用 MCP 工具错误结果与稳定代码，如 `revision_conflict`、`idempotency_conflict`、`not_found`；参数和协议错误沿用 MCP。中文说明不带 SQL 参数、密码或记忆正文。读工具声明 `readOnlyHint`，更正 / 遗忘声明 `destructiveHint`，写工具具备幂等后声明 `idempotentHint`；这些只是客户端提示，不能替代鉴权。

真实服务测试覆盖保存、默认不搜原始文本、来源展开、预算分页、并发更正、空间拒绝、索引失败恢复、遗忘与旧请求重放。2026-10-28 原生 Codex 客户端已通过五工具调用与跨会话召回；本机持久化配置也已用两个独立客户端验证。未启动模型回合；自然对话中的自动调用、自动 hook 加载、远程 HTTPS 和实际语料效果保持独立待验收。

## 安装指南

`get_setup_guide` 接受 topic、known_guide_version 及可选公开用途，沿用当前 MCP 身份，返回空间、Agent ID、Secret 权限和指南版本；版本命中可省略静态正文。不返回接入密钥，不在服务端执行本机安装。日常记忆规则由初始化 instructions 和工具／参数描述提供，个人 AGENTS.md 片段为可选补充，不自动修改用户文件。接入见 [Codex 接入与使用](Codex接入与使用.md)。
