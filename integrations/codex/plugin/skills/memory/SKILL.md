---
name: chromie-memory
description: Use when 用户希望通过 Chromie 记住、找回、更正跨对话或跨 Agent 的知识，整理一组记忆与主题入口，或保存、取用 Secret。提供 MCP 与本机 CLI 的配合方式；安装接入问题使用 chromie-setup。
---

# 使用 Chromie 接续与整理记忆

Chromie 是跨对话、跨设备、跨 Agent 共享的记忆空间。让未来没有当前聊天背景的 Agent 也能理解和使用；具体组织、拆分和必要背景由你判断，不强求固定模板或机器字段，不编造未知信息。

- **接续任务**：先了解实时能力，围绕当前需要少量召回，再展开相关正文与必要依据。需要选择关键词、处理宽泛问法／无结果、沿引用补读或反馈召回效果时，读 [检索与接续](references/retrieval.md)。
- **保存或更正**：留下值得复用的结论、适用范围和必要的已知上下文。更正同一事实优先读取并修订原记忆，避免复制多个互相矛盾的版本。整理长主题、建立引用或核对冲突时读 [记忆组织](references/organization.md)。
- **保存或取用密码、Token 等 Secret**：先调用 `get_setup_guide(topic="secret")` 核对当前权限与 CLI 指南，再读 [本机 Secret 工作流](references/secrets.md)。只在模型中处理公开说明、格式契约与引用，原文由 CLI 和本地程序处理。

MCP 的实时工具 Schema、能力和权限是当前服务的依据；Skill 不保存服务地址、接入密钥或模型配置。采集不等于长期记忆，记忆也不能授予用户尚未授权的执行权限。只有用户明确要求遗忘时才删除已定位内容。
