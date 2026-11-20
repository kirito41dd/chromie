# 云端 Skill

Chromie 可以托管标准 Agent Skills 目录。每份 Skill 的入口是 `SKILL.md`，可以附带脚本、模板、许可证和参考资料。接收者可以使用自己的环境和凭据执行流程。作者需要把这些运行条件写清楚，自己写进包里的文字和文件会跟着包一起分享。

## 发布、使用和分享

先用 `chromie init` 完成本机接入。下面以目录名和入口中 `name` 都为 `report-check` 的包为例。以下命令在 Bash 和 Nushell 中通用。

```shell
chromie skill validate ./report-check
chromie skill push ./report-check --reason '首次发布报表核查流程'
chromie skill pull report-check --out ./shared/report-check
```

拉取前创建好父目录 `shared`，目标 `shared/report-check` 必须是新目录。分享这个目录即可。接收者可以离线交给自己的 Agent，也可以用自己的接入把副本发布到另一个空间。工作副本的来源、基础修订与待确认请求保存在用户配置目录里。

修改已发布的工作副本后，再次运行 `push`。两个 Agent 从同一版提交修改时，先提交者获得新修订。后提交者保留本地编辑，另拉一份最新版本再合并。回退命令通过一个新修订重新发布旧清单。

```shell
chromie skill rollback ./report-check --revision 1 --reason '恢复已验证的报表模板'
```

## 在 Web 查看

登录空间后，点顶栏的「Skill 库」。列表按创建时间从新到旧显示名称、适用说明和当前修订，每次加载 20 项。点「查看内容」打开包内文件列表，默认显示 `SKILL.md`。点选路径可查看当前修订的任意文件。UTF-8 文本保留源码并可选中复制，长文件可逐页读完；二进制文件提供十六进制视图。点「下载原文件」可取回完整字节。切换文件时按需读取，遇到新修订时提示刷新。

点 Skill 卡片上的「下载整包」，可直接保存当前显示修订的 `<name>.zip`。解压后是与 frontmatter 的 `name` 一致的顶层目录，`SKILL.md` 位于其中。脚本、参考文档、模板和附件保留发布时的相对路径和原始字节；包内只收录该修订发布的文件。Chromie 不记录上传端的 POSIX 权限，导出时为 `scripts/` 下带 shebang 的脚本设置可执行权限。详见 [Agent Skills 目录规范](https://agentskills.io/specification)。

## Agent 如何使用

原生宿主通过空间 MCP 的 `skills/list`、`skills/get` 和 `resources/read` 逐步发现、读取入口及附件。稳定 URI 指向当前版，每份清单给出文件原始长度和 SHA-256。宿主负责比对摘要、缓存隔离和执行授权。兼容宿主通过产品随附的使用 Skill 调用 `list_skills({})`。每页最多 10 项，包含 `skill_id`、`revision` 和作者 frontmatter 中的 `name`、`description`。`next_cursor` 非空时传给下一次的 `cursor`。两条路径共享空间隔离、ID 升序及分页边界。兼容目录只读当前元数据，选中后才用 `read_skill` 按修订读取入口和所需附件，遇到冲突就重读当前入口。脚本在获得授权的接收端运行。CLI 拉取的文件默认只有当前用户可读写。包内可用 `python3 scripts/check.py` 或 `sh scripts/check.sh` 显式指定解释器。

重复任务或跨人交接时，Agent 先交付结果，再建议将流程存入共用库。用户确认发布或已授权维护后，Agent 可以拉取原版，验证受影响和应维持原状的样例，再发布。

`report_skill_use` 按实际观察归属修订。例如第 1 版漏算免票，第 2 版修复并通过样例，分别用两个请求记录第 1 版的 `corrected` 和第 2 版的 `verified`。同一请求重试只计一次。修订未知时只计入 Skill 总数。发布回执与使用反馈回执独立，宿主拒绝反馈时仍可交付已完成的任务。

## 存储与恢复

| 记录 | 用途 |
| --- | --- |
| `skills` | 空间内唯一包名、当前修订与使用计数 |
| `skill_revisions` | 入口元数据、发布者、原因、版本计数与可回退状态 |
| `skill_revision_files` | 一次完整修订所包含的路径与对象 |
| `stored_objects` | 空间内文件摘要、字节数、上传归属与回收状态 |
| `skill_usage_reports` | 带版本或版本未知的使用反馈明细 |

RustFS 保存文件原始字节。发布时由 MySQL 事务一起提交当前修订、清单与幂等回执，读取时按清单验证对象。每包最多 512 个文件，合计 16 MiB。文件历史保留最近五版，刚被替换的版本另有 24 小时保留期。回退和发布共享并发检查。

实例冷备应停服后一起保全 `config/` 与 `data/`，包括实例根私钥、RustFS 应用凭据、MySQL 和 `data/rustfs/`。恢复后选一份包含附件的 Skill，用 CLI 拉取并核对逐文件摘要。空间的“炼化／夺舍”仅迁移记忆与 Secret。
