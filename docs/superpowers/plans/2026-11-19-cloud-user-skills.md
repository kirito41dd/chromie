# 云端用户 Skill 首版实施

依据[设计稿](../specs/2026-11-17-cloud-user-skills-design.md)，所有写入都使用本空间的 Agent MCP 身份。运行验收使用本地虚构资料及隔离数据库。

1. 包格式及存储。校验标准入口、路径、大小、清单和摘要。先迁移隔离 MySQL，再反向生成 Entity。对象在 RustFS，发布以 MySQL 为准。保护并发修订、传输中断、历史回退和引用回收。
2. CLI 和 HTTP。沿用配对凭据，支持 validate、pull、push、rollback。纯文件工作副本可分享，操作记录保存在外部。上传提供单文件原始字节，再提交完整清单；拉取按修订验证整包。
3. MCP。原生清单分页、稳定入口、标准资源读取、按权限缓存；Codex 通过分页目录、按需读取和报告过渡接入。更新产品自带 Skill 的发现和本地修订说明。
4. 部署和验收。Compose 私网提供 RustFS，凭据持久化。检查项目 lint，验证隔离 MySQL 与 RustFS、两个 Agent 的冲突/重试、标准 MCP 消息和同事独立使用。独立报告真实宿主、真实模型与模拟客户端的覆盖范围。

### 模块间接口

HTTP 前缀为 `/spaces/{space}/skills`，全部使用 MCP Bearer。POST `/objects` 接受单文件字节并返回 `{object_id,digest,size}`。POST `/publish` 传入 `{request_id,skill_id?,expected_revision?,reason,files:[{path,object_id,digest,size}]}`。POST `/rollback` 传入 `{request_id,skill_id,expected_revision,target_revision,reason}`。两者回执为 `{skill_id,revision}`。GET `/receipts/{request_id}` 获取相同 Agent 的已提交回执。GET `/lookup?name=...` 按包名取当前清单，GET `/{id}` 按 ID 取当前清单，包含 `{skill_id,revision,frontmatter,files:[{path,digest,size}]}`。GET `/{id}/file?path=...&revision=N` 取当前版原始字节，修订变化返回 409。摘要统一采用 `sha256:` 加小写十六进制。

CLI、本机验收与服务端共享 `crate::skill_package` 的 `validate_path(&str)`、`validate_package(name: &str, files: &BTreeMap<String, Vec<u8>>)`（返回完整 frontmatter）、`digest(&[u8]) -> String` 及 `MAX_FILES`、`MAX_BYTES`。MCP 存储入口通过已鉴权的 Store 提供 原生清单分页 `skill_manifests_page`、兼容摘要分页 `skill_directory`、`skill_manifest(space,id) -> Result<Value>`、`skill_file(space,id,path,revision: Option<i32>) -> Result<Vec<u8>>`、`skill_report(space,id,revision: Option<i32>,request_id,outcome: &str,reason: Option<&str>) -> Result<Value>`。清单返回结构同 HTTP；列表按 ID 升序。

部署配置约定为 CHROMIE_S3_ENDPOINT、CHROMIE_S3_BUCKET、CHROMIE_S3_ACCESS_KEY、CHROMIE_S3_SECRET_KEY。发行包以已持久化的桶级应用凭据连接，目录或凭据缺失时保护既有数据。
