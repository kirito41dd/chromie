# Chromie 通用采集程序

hook 调用本包的 bin，输入由客户端通过标准输入传入。鉴权、HTTPS 上传、UUID／时间生成、离线暂存与 flush 补传均由 Chromie 完成，不需要自己实现 HTTP 上传。

先核对 BUILD.json 的 target 与运行 agent 的电脑一致，将程序解压到长期保留目录。在 Web 下载当前接入的私有 JSON，移到私有目录并 chmod 600；只向 agent 提供文件路径，不读取或打印其中密钥。

## hook 命令

Bash：

```bash
CHROMIE_QUEUE_DIR="/私有目录/chromie/queue" "/程序绝对路径/chromie-client/bin/chromie" capture --config "/私有目录/chromie-client.json"
```

Nushell：

```nu
with-env {CHROMIE_QUEUE_DIR: "/私有目录/chromie/queue"} { ^"/程序绝对路径/chromie-client/bin/chromie" capture --config "/私有目录/chromie-client.json" }
```

将这条命令注册到客户端的用户消息提交、助手最终回复 hook。支持 shell 命令且原生 stdin 与 Codex UserPromptSubmit／Stop 格式兼容时，可直接使用；采用 exec 配置的客户端将环境变量、程序与参数分别填入对应字段。

原生字段不同，仅需在本地将输入转换为下面的 JSON，再通过管道交给相同命令。输入不包含密钥、空间、接入 ID、事件 UUID 或时间：

```json
{
  "session_id": "客户端会话ID",
  "role": "user",
  "body": "消息正文",
  "turn_id": null,
  "project_key": null
}
```

最终回复使用 role=assistant；不要传中间推理、工具输出、密码或密钥原文。turn_id、project_key 可省略；project_key 未指定时可从 CHROMIE_PROJECT 读取。正文最多 60,000 字节，其他标识最多 128 字节。通用输入的 stdout 为 `{}`，不附加 Codex 专属上下文；客户端有特殊输出要求时由本地薄包装处理。

## 暂存与验证

网络失败会在私有队列保存原事件，保留其 UUID 与时间。恢复后使用相同的配置和队列补传：

Bash：

```bash
CHROMIE_QUEUE_DIR="/私有目录/chromie/queue" "/程序绝对路径/chromie-client/bin/chromie" flush --config "/私有目录/chromie-client.json"
```

Nushell：

```nu
with-env {CHROMIE_QUEUE_DIR: "/私有目录/chromie/queue"} { ^"/程序绝对路径/chromie-client/bin/chromie" flush --config "/私有目录/chromie-client.json" }
```

bin 仅读取独立的 hook_key，不使用 MCP 密钥或空间密码。队列目录权限 0700、文件 0600，内容不输出给模型。CHROMIE_CAPTURE=off 可暂停采集。更换空间或接入不会把旧队列改投。

先用虚构消息检查上传，再在客户端发送新消息核对 Web 的接入来源和上传时间。测试 bin 上传成功不等于原生 hook 已触发；原始消息采集也不会自动生成长期记忆。
