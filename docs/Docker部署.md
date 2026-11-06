# Docker 部署

首次部署不需要 `.env` 或手写密码。Compose 自动准备 MySQL 8.4、Qdrant 1.20、Chromie 与 Slint/Wasm 看板，执行 SeaORM 迁移，并创建 `personal` 个人空间。

## 启动

从 [GitHub Releases](https://github.com/kirito41dd/chromie/releases) 下载目标版本的 `compose.yaml`，保存在独立的部署目录中，再进入这个目录运行。发行 Compose 从 `docker.io/kirito41dd/chromie` 拉取固定版本镜像，支持 Linux AMD64／ARM64；macOS 的 Docker 容器同样运行 Linux 镜像。

以下外部命令在 Bash 与 Nushell 中相同：

```shell
docker compose pull
docker compose up -d --wait
docker compose ps
```

首次拉取需要下载 Chromie、MySQL 和 Qdrant 镜像，随后服务自动完成初始化。公开镜像可匿名拉取；发布者使用的 Docker Hub Token 不需要配置到用户设备。仓库根目录的 `compose.yaml` 仍用于源码构建，与 Release 的纯镜像 Compose 分开，见下方[镜像构建与客户端平台](#镜像构建与客户端平台)。

浏览器打开 [个人空间](http://127.0.0.1:8732/spaces/personal/dashboard)。在**自己的终端**查看初始空间与密码；不要将输出发给 Agent 或贴入聊天：

以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose exec chromie chromie-server initial-login
```

同样的信息保存在 `config/initial-space.txt`，不会自动写入容器日志。Linux 上自动创建的私有配置可能由容器用户所有，使用上面的容器命令查看即可，不要通过放宽整个目录权限来读取。这个文件只记录最初生成的密码；以后改密不会更新它，不能靠编辑文件修改数据库密码。确认不再需要后可以自行删除该文件，后续正常启动不依赖它。

登录后在顶部“接入”创建 Agent，并按页面指引安装 CLI、连接 MCP 与 hook。详见 [Codex 接入](Codex接入与使用.md)。

## 客户端与服务端

用户安装的是独立的 `chromie` 客户端；容器默认运行 `chromie-server container-serve`。接入、采集、Secret 本地取用属于客户端，数据库迁移、空间管理和容器初始化属于服务端，不在同一个命令入口混用。两者都使用 Clap 提供中文分层帮助。

以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose exec chromie chromie-server --help
```

客户端发布与服务端构建使用独立 feature；默认同时启用仅为开发及集成测试方便。细节见 [客户端与服务端拆分](技术选型.md)。

## 只有两个持久目录

| 目录 | 内容 | 处理要求 |
| --- | --- | --- |
| config/ | 服务配置、实例根私钥、服务凭据、初始登录信息与初始化标记 | 私密保存，保留原文件并安全备份 |
| data/ | data/mysql/ 权威数据、data/qdrant/ 检索索引 | 持久保存，与对应配置一同恢复 |

普通启动会保留原根私钥、服务凭据、空间密码和已有数据，不会重置为初始密码。已有数据却缺少完整配置或根私钥时拒绝重新初始化，应恢复原文件。实例根私钥丢失后历史 Secret 无法解密，不能用新私钥替代。

MySQL 与 Qdrant 不发布宿主端口，默认仅将 Chromie 发布到 `127.0.0.1:8732`。长期服务使用自动重启与有限日志轮转；Docker 引擎本身是否开机启动由宿主系统负责。

## 状态、停止与升级

以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose ps
docker compose logs --tail 80 chromie
docker compose stop
docker compose up -d
```

`/health` 表示进程存活；`/ready` 同时检查数据库与索引连通。应用容器健康检查使用 `/ready`。`bootstrap` 容器成功退出是正常状态，首次准备完成后由其他服务长期运行。

先做好下述一致性备份，再下载目标版本 Release 的 `compose.yaml` 替换部署文件；保留原 config 和 data，升级会自动执行向前迁移：

以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose pull
docker compose up -d --wait
```

发行 Compose 固定到明确版本；仅运行 `pull` 不会将旧 Compose 自动切换到新版本。发布与校验材料说明见[持续集成与发布](持续集成与发布.md)。

项目也提供不依赖源码目录的镜像分发包。开发者生成镜像压缩文件、独立 Compose 和校验值，以下命令在 Bash 与 Nushell 中相同：

```shell
python3 scripts/build-docker-release.py
```

接收方按包内说明先 `docker load -i` 导入对应架构镜像，再运行 `docker compose up -d`，无需创建 .env；MySQL 和 Qdrant 仍使用其官方镜像。分发包不含任何实例配置或个人数据。

备份时先停止整套服务，再以有权限的本地工具分别保存 config 与 data。恢复到空目录时放回**对应的一组原配置和数据**，再启动；不得只恢复数据库后重新生成私钥。单独复制正在写入的 MySQL 数据目录不能作为一致性备份。

## 修改端口与外部访问地址

端口可在 Compose 文件中直接改。临时使用其他本机端口的示例：

Bash：

```bash
CHROMIE_PORT=18732 docker compose up -d
```

Nushell：

```nu
with-env {CHROMIE_PORT: '18732'} { ^docker compose up -d }
```

远程使用时，在 config/chromie.json 中将 public_origin 设置为浏览器和 Agent 实际使用的 HTTP 或 HTTPS Origin，并调整 Compose 端口发布或代理网络。TLS 由部署与网关决定，程序不区分公网和私网，也不根据网段禁止 HTTP。HTTPS Origin 会使用 Secure Cookie；HTTP Origin 则不设置 Secure，保留 HttpOnly、SameSite 和 Origin 校验。

配置文件示意，不是密码文件：

```json
{
  "version": 1,
  "public_origin": "https://chromie.example.com"
}
```

修改后重新启动 Chromie。服务凭据、数据库密码与根私钥不是普通可随意替换的设置；不要手动改这些文件来完成轮换。

## 镜像构建与客户端平台

正式发行的每个服务镜像均携带 macOS／Linux 的 ARM64 与 x86_64 四份 CLI。安装脚本根据用户设备的系统与 CPU 选择下载文件，校验 SHA-256 并试运行后安装；缺少对应产物或程序不能运行时保留原有安装。

修改源码或自行构建时，从仓库根目录运行。以下命令在 Bash 与 Nushell 中相同：

```shell
docker compose up -d --build
```

源码版 Compose 优先使用本地镜像，不拉取 `chromie:local`；更新源码后用 `docker compose build --pull` 重新构建，再 `docker compose up -d`。

Dockerfile 在构建阶段生成 Linux 服务、Wasm 页面和当前 Linux 架构的 CLI。源码构建所需的 macOS 等额外 CLI 产物须预先放入 packaging/clients/，才会一同打包；它们只包含程序和 SHA-256，不包含本机配置。不能将同一个 Linux 程序标记为 macOS 产物。

构建层使用 `rust:1.99.1-bookworm`，降低 Linux CLI 的 glibc 基线；运行层仍为 `debian:trixie-slim`。缓存按构建发行版和目标架构分开，避免复用较新系统的编译产物。发布客户端时还须在目标宿主实际执行验证，容器内可运行不代表宿主运行库兼容。

在对应平台构建额外客户端，以下命令在 Bash 与 Nushell 中相同：

```shell
cargo build --release -p chromie --no-default-features --features client --bin chromie
python3 scripts/build-cli.py --binary target/release/chromie --output packaging/clients
docker compose build
```

发布时按实际平台验证镜像、客户端执行、重启与原数据保留；独立验收方法见 [开发运行](开发运行.md)。其他架构未经构建验证时不宣称可用。

开发者继续使用 [开发运行](开发运行.md) 中的 compose.dev.yaml。不要将面向用户的默认 Compose 与原 chromie-dev 数据卷混用。
