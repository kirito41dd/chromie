# Secret 取用、展示与审计

## 按服务保存

新增凭据默认按服务、环境分别保存，MySQL 和 ClickHouse 不再因为同一任务就放进一个包；Agent 整理各自公开说明和契约。现有混合包不自动拆分，取用仍交付整包，需要用户确认后再整理。首版没有子字段投影；契约只公开结构，不是字段级访问授权。

## 本机托管执行

以下命令在 Bash 与 Nushell 中相同：

```shell
chromie secret exec --help
chromie secret exec --ref secret://work/01900000-0000-7000-8000-000000000000 --revision 2 --purpose '检查测试 MySQL 连通性' -- python3 /私有目录/check_mysql.py
```

先通过 get_memory 核对公开契约、引用、当前修订。必须提供修订和简短公开用途；用途不得包含凭据、完整任务或命令。程序及参数经 `--` 直接启动，不经 Shell，不把密码展开为参数或环境变量。子进程从 `CHROMIE_SECRET_FILE` 读取 0600 文件，目录随机、0700，按公开契约自行解析并用于当前授权目标；调用方法与数据库驱动由任务决定。Chromie 接入环境变量不会被继承，仅传递临时文件路径；这不构成子进程沙箱，受信程序仍拥有当前用户通常的文件访问权限。

默认子进程 stdin、stdout、stderr 均不透传。回执包含 outcome、exit_code、revision、audit_id、audit_reported、temporary_files_removed，不含原文、完整命令、临时路径。需要业务查询结果时，程序须自行保证输出不含凭据，再显式使用 `--inherit-output`。透传不会自动脱敏，可能包含敏感数据。

默认 300 秒超时，可用 `--timeout` 指定 1～3600 秒。退出、启动失败、超时、Ctrl+C、SIGTERM 后停止进程组并删除文件，然后上报结果。CLI 对子进程失败、清理失败或审计上报失败返回非零；子进程原退出码保留在回执。文件删除不是物理安全擦除；SIGKILL、掉电及子进程自行复制或守护化不能保证清理。明确需要长期文件时继续使用 `secret get --out`。

首版支持 macOS/Linux，复用已有 Tokio、rustix、Clap，没有新增 Rust 库。旧 CLI 先用 `secret exec --help` 检查，缺少命令时重跑服务提供的安装器升级；配置和接入密钥保留。

## 指南与实时状态

`get_setup_guide` 初次返回 guide、guide_version、当前身份与权限。客户端在当前上下文保存正文与版本，下次携带 `known_guide_version`；主题和内容摘要一致时返回 `guide_cached=true` 并省略正文。正文变更或主题不同会重新返回完整指南，不增加 MCP 工具数量。

身份、启停、密钥有效性和 Secret 权限每次实时校验，不因指南版本命中而复用。缓存只节约已经读过指南的重复调用；首次仍需读完整指南。服务端返回能力协议版本，不猜测远端客户端安装位置。`chromie doctor` 的 `client` 字段报告本机真实可执行路径、版本和能力；`hook.cli_path` 指的是 Codex 程序。

## 取用审计

新增 `secret_access_audit` 表，经 migration 执行后从 MySQL 反向生成 Entity。只在鉴权、修订与契约检查通过、准备交付原文时写审计，审计失败则不交付。普通 GET、CLI get/inspect/exec、Web 查看均覆盖；失败的鉴权或修订请求不生成成功交付记录。`delivered` 表示服务端已批准交付，不能证明客户端完整接收。

记录空间、审计 ID、Secret ID、当时修订、鉴权确定的 Agent ID、可选简短公开用途、时间、执行结果与退出码。空间密码查看的 agent_id 为空。原文、密文、命令、stdout/stderr、完整任务及认证头均不入表。Web 已登录 Cookie 仍不能直接取用，继续要求独立验密。

GET 回应新增 `X-Chromie-Audit-Id`，`secret get` 回执也提供 audit_id。`exec` 清理后向 `POST /spaces/{space}/secrets/access-audit/{audit_id}/result` 上报枚举状态及退出码。仅原取用 Agent 能写，其他 Agent／空间不可改；同值重试幂等，已完成状态不能改写。结果明确为 `client_reported`，并非服务端证实任务执行或业务成功。普通下载、浏览器查看或进程强杀没有执行结果，保持 delivered／reported_at 为空，不能据此推断失败或成功。网络中断时 CLI 回执明确 audit_reported=false。

首版审计保存在 MySQL，尚未新增 Web 审计页面；不自动设置保留期或删除历史记录。

## Web 展示

弹窗加宽，查看区随窗口高度调整，长原文完整滚动。JSON 对象和数组默认格式化，字段、字符串、数字及布尔/null 区分颜色；“查看原文”保留原始文本的选择复制。格式化只改空白，不改大整数、重复字段或转义。普通文本（包括数字密码）不猜测为 JSON。

原文和高亮模型仅在当前浮层内存中；隐藏、关闭、换页、退出或 60 秒到期时一起清空，不写入日志、看板快照或浏览器缓存。独立验密仍固定五分钟。
