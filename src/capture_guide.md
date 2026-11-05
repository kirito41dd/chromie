# 消息采集

推荐用 CLI 安装 hook。Codex 在自己的终端运行以下命令，两种 Shell 通用：

```shell
chromie hook install
chromie doctor --test-capture
```

首次需要用户在 Codex /hooks 信任后重载或新开对话。测试上传只证明 HTTP 通道，不表示原生 hook 已触发。发送一条新消息，再在 Web“采集消息”页按 Agent 查看会话、角色、消息与实际接收时间。

通用适配将用户消息和最终助手回复转换为 stdin JSON，交给 chromie capture --config 绝对私有配置路径：session_id、role（user／assistant）、body，可选 turn_id、project_key。不要自行实现上传、鉴权或重试。bin 负责 UUID、时间、私有队列和 flush 补传；接入身份由服务端验证，不信任自报 Agent。

暂停当前启动环境采集：Bash 用 export CHROMIE_CAPTURE=off，Nushell 用 $env.CHROMIE_CAPTURE = 'off'。暂停不撤回已提交消息，也不清除聊天历史。原始采集不自动生成长期记忆。
