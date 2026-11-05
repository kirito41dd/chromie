# 贡献指南

目前按实际使用反馈演进，优先修复可复现的问题，避免在基础流程稳定前增加新功能。

修改前阅读 [开发运行](docs/开发运行.md)、[开发约定](docs/开发约定.md) 和 [AGENTS.md](AGENTS.md)。沟通、文档、注释和面向人的错误说明使用中文；重要依赖变化先讨论。

提交问题时说明操作步骤、预期与实际结果、版本和运行环境。使用虚构数据复现，不上传空间密码、接入密钥、Secret 原文、实例私钥、完整配置或个人记忆。安全问题应通过代码托管平台的私密漏洞报告渠道提交；公开前不要粘贴可直接利用的凭据。

改动保持单一目的，说明行为变化和验证范围。提交说明采用 `feat: 中文描述`、`fix: 中文描述`、`docs: 中文描述` 等格式。数据库变更采用 SeaORM migration → 测试数据库 → Entity 反向生成，保留既有迁移和数据。

基础检查，以下命令在 Bash 与 Nushell 中相同：

```shell
cargo fmt --all --check
cargo check --workspace --lib --bins
cargo clippy --workspace --lib --bins
cargo test --workspace --lib
node --test web/tests/bootstrap.test.mjs
python3 scripts/test-shell-examples.py
```

前端改动另运行 Wasm 检查与相关界面验证，详见开发手册。真实服务测试默认忽略，仅使用单独的测试库、虚构密钥和测试资料；不对开发日用库或生产实例执行。

自有贡献沿用项目 Apache-2.0 许可，第三方代码、字体和资源必须保留原许可与来源；分发要求见 [第三方许可说明](THIRD_PARTY_NOTICES.md)。
