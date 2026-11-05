# 第三方许可说明

Chromie 自有源码与文档采用 [Apache License 2.0](LICENSE)。依赖、字体和其他第三方材料保留各自许可证，不能将本仓库的 Apache-2.0 声明理解为对所有依赖重新授权。具体 Rust 依赖与版本由 Cargo.lock 记录。

## Slint

前端使用 Slint 1.18.1。Slint 提供 GPLv3、Royalty-free 和商业许可三种路径，官方明确允许应用自有源码保持 Apache-2.0。

本仓库对开源分发采用 Slint 的 GPLv3 路径：**包含 Slint 的前端组合产物按 GPLv3 要求分发**，Chromie 自有文件仍保留 Apache-2.0 声明。分发 Wasm、相关前端程序或包含它们的镜像时，应一并提供对应版本的完整可构建源码、构建脚本和许可材料；不能只将该组合产物标作 Apache-2.0。与 Slint 独立的服务端和 CLI 不因同仓库而自动改成 GPLv3。

- [Slint 1.18.1 官方许可说明](https://github.com/slint-ui/slint/blob/v1.18.1/LICENSE.md)
- [GPL-3.0-only 正文](licenses/GPL-3.0-only.txt)

## 中文字体

看板随构建产物携带 Noto Sans SC，版权归原作者，使用 SIL Open Font License 1.1。原文件未修改，完整许可与字体来源保存在 [web/fonts](web/fonts/README.md)；分发时保留 [OFL.txt](web/fonts/OFL.txt)。

## 其他依赖与分发

Rust 直接及传递依赖、Docker 基础系统、MySQL 和 Qdrant 均保留各自许可。Cargo.lock 不是许可证清单；发布者应根据实际目标和 feature 核对依赖包自带的许可、版权与 NOTICE，保留适用通知。系统镜像中的既有许可文件也不应删除。

项目 Docker 镜像将本说明、Apache-2.0、GPLv3 和字体许可放在 /opt/chromie/licenses；Codex 与通用兼容 ZIP 保留相同材料。下载单个 CLI 程序时，源码和许可仍应随该版本发布资料一起提供。目前仓库没有预设公共代码或镜像仓库地址，正式发布时须使用真实的源码地址与版本。
