# Chromie（克罗米）

**让 Agent 的记忆，跟着你走。**

Chromie 是面向个人私有化部署的记忆服务，为不同对话、设备和 Agent 提供一个共享的记忆空间。项目约定、个人偏好、排查经验和持续推进的事情，都可以由 Agent 保存，在需要时召回、补充或更正。

你掌握服务与数据，Agent 通过 MCP 使用记忆，Web 控制台让这些过程看得见。目前项目与文档均面向中文用户。

## 核心特性

- **跨对话、跨设备共享**：支持 Codex 自动接入，也为其他 Agent 提供通用 MCP 配置；每个接入有独立身份和密钥，可区分记忆来源。
- **记忆可追溯、可更正、可遗忘**：保留来源与修订，支持 Agent 反馈召回结果是否有用，在看板查看热门和有用的记忆。
- **对话自动采集**：通过 hook 收集新增用户消息与可取得的最终回复，支持本地队列和离线补传。原始对话与正式记忆分开，重要结论由 Agent 主动保存。
- **关键词与语义召回**：开箱即可使用关键词检索；可选配置 OpenAI 兼容的 embedding 接口，启用混合检索。不配置模型也能正常使用。
- **加密 Secret**：密码、Token 等凭据独立加密保存。MCP 只返回公开说明、格式契约和引用，原文由本地 CLI 或脚本取用，支持托管执行与审计。
- **可查看的 Web 控制台**：浏览记忆和采集消息，手动召回并查看检索过程，管理 Agent 接入、模型配置和 Token 用量。
- **炼化／夺舍**：用口令加密导出一个空间的核心记忆与 Secret，迁移到其他空间或实例；导入先预览，相同内容跳过，冲突保留现状。
- **个人部署，空间隔离**：用空间区分工作、生活等资料，不引入复杂的用户或租户管理；Docker Compose 自动准备配置、数据库和检索服务。

## 自行部署

### 1. 启动服务

准备好 Docker Engine 和 Docker Compose，获取源码并进入包含 `compose.yaml` 的仓库目录。主机无需另装 Rust、Node、MySQL 或 Qdrant。

以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose up -d --build
docker compose ps
```

首次启动会在 Docker 内构建 Chromie、下载依赖并启动 MySQL 与 Qdrant，耗时较长。无需预先创建 `.env`、填写数据库密码或配置模型；服务会自动生成初始配置、执行数据库迁移并创建 `personal` 空间。

`chromie` 服务就绪后即可登录；一次性的 `bootstrap` 容器成功退出是正常状态。当前 Compose 使用源码构建。项目已准备 GitHub Actions 向 `kirito41dd/chromie` 发布镜像的配置；首次发行成功后，可从 [GitHub Releases](https://github.com/kirito41dd/chromie/releases) 下载只拉取镜像的 Compose，使用步骤见 [持续集成与发布](docs/持续集成与发布.md)。

### 2. 查看初始密码并登录

在自己的终端运行，以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose exec chromie chromie-server initial-login
```

打开 [Web 控制台](http://127.0.0.1:8732/spaces/personal/dashboard)，使用输出中的空间与密码登录。初始登录信息也保存在 `config/initial-space.txt`，不会自动打印到容器日志；请不要将其贴入公开问题或 Agent 对话。

### 3. 接入 Agent

1. 在 Web 顶部进入“接入”，创建并命名一个 Agent 接入。
2. 按页面指引安装 `chromie` CLI，执行页面生成的 `init` 命令。
3. 在浏览器确认本机配对。Codex 会自动配置 MCP 并安装采集 hook，其他 Agent 可使用通用接入配置。
4. Codex 用户在 `/hooks` 中审核信任；如未看到新 hook，完全退出并重新打开 Codex，再发送一条消息验证采集。

之后可以在对话中明确告诉 Agent“记住这个项目约定”，在新对话里再让它查找相关背景。采集消息可在 Web 单独浏览，只有正式保存的记忆会参与召回。

详细步骤见 [Codex 接入与使用](docs/Codex接入与使用.md) 和 [通用接入](docs/通用接入.md)。直接从源码构建的镜像默认只带当前架构的 Linux CLI；发行流水线会额外打包 macOS／Linux 的四平台客户端，见 [客户端平台说明](docs/Docker部署.md#镜像构建与客户端平台)。

### 4. 按需启用语义检索

在初始空间的“系统设置”中填写 OpenAI 兼容的 embedding 服务地址、模型和 API Key，测试连接后保存启用。后台会为已有记忆建立向量索引，页面可查看进度和按日 Token 用量。

Embedding 默认关闭；模型不可用时保留关键词检索。更换模型、维度或编码方式会触发历史记忆重建，详见 [模型设置与索引重建](docs/模型设置与索引重建.md)。

## 数据保存与升级

部署只使用两个持久目录：

| 目录 | 保存内容 |
| --- | --- |
| `config/` | 服务配置、实例根私钥、服务凭据与初始登录信息 |
| `data/` | MySQL 权威数据与 Qdrant 检索索引 |

**请备份原配置与数据，尤其是 `config/instance-key.pem`。根私钥丢失后，历史 Secret 无法恢复，不能通过生成新私钥来修复。** 重启、升级或恢复时保留原文件；初始密码文件只记录首次生成的密码，不能靠编辑它改密。

查看状态与应用日志，以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose ps
docker compose logs --tail 80 chromie
```

升级前做好一致性备份，更新源码后执行。以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose build --pull
docker compose up -d
```

升级会自动执行向前迁移。完整的停止、备份、恢复和镜像分发说明见 [Docker 部署](docs/Docker部署.md)。

## 从其他设备访问

默认仅监听宿主的 `127.0.0.1:8732`。部署在服务器上时，需要按实际网络环境完成两项配置：

1. 通过反向代理提供访问，或调整 Compose 中的端口发布地址，使目标设备能够连接。
2. 在自动生成的 `config/chromie.json` 中，将 `public_origin` 改为浏览器和 Agent 实际访问的 Origin，例如 `https://chromie.example.com`，保留其他配置字段，然后重启应用。

同机反向代理可继续使用默认回环端口。HTTP 和 HTTPS 均支持，TLS 由部署或前置网关处理；具体配置见 [外部访问说明](docs/Docker部署.md#修改端口与外部访问地址)。

## 当前范围

Chromie 仍在根据实际使用反馈迭代。目前由 Agent 整理并写入正式记忆，尚未实现后台自动提取、个人背景自动整理或附件／历史对话导入。CLI 安装流程面向 macOS 与 Linux，暂未提供 Windows 原生安装流程。

## 更多文档

- [文档导航](docs/README.md)：使用、开发与设计资料。
- [Secret 记忆](docs/Secret记忆.md)：录入、查看、本地取用与安全边界。
- [炼化与夺舍](docs/炼化与夺舍.md)：加密导出和追加导入。
- [开发运行](docs/开发运行.md) / [贡献指南](CONTRIBUTING.md)。
- [持续集成与发布](docs/持续集成与发布.md)：维护者配置 Actions 与 Docker Hub。

## 技术与许可

Rust 2024、Axum、官方 MCP Rust SDK、SeaORM 2、MySQL 8.4、Qdrant 1.20 和 Slint / Wasm。架构与组件职责见 [技术选型](docs/技术选型.md)。

Chromie 自有代码与文档采用 [Apache-2.0](LICENSE)。第三方组件保留各自许可；含 Slint 的前端组合产物遵循 GPLv3 分发要求，字体采用 OFL-1.1，详见 [第三方许可说明](THIRD_PARTY_NOTICES.md)。
