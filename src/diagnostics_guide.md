# 接入与记忆诊断

以下命令在 Bash 与 Nushell 中相同：

```shell
chromie --version
chromie doctor
chromie hook install
```

MCP 失效先核对服务地址、空间、Agent 是否停用／重置，以及客户端是否重载。需要重新选择接入时使用 chromie init --reconnect；仅修复 hook 用 hook install。已有配置先由用户确认，不默认添加 --replace，不删除其他 MCP、信任记录或根私钥。

没有采集：检查 /hooks 信任与是否启动新会话，再看 Web“采集消息”页。doctor --test-capture 上传明确标注的虚构消息，只证明上传链路。通用接入的 doctor 需 --agent generic 或 --config 绝对私有文件路径。

召回为空：先用 get_capabilities 读取当前模式；关键词模式尝试原文词、已知别名、服务名或关键术语，混合模式可保留对象和条件用自然问法。以 recall 返回的实际模式和降级原因为准，降级时可改关键词补查一次。使用已知准确的 project_key，不编造标识。短缩写仍有歧义时先澄清，不把空结果解释成某一种缩写含义，也不据此查询无关系统。用 get_memory 核对命中对象、范围、修订与来源；分数只表示相关，不保证有答案。Web 召回调试页可看实际检索路径、过滤、兜底与耗时。不要为了排障读取或打印原始凭据、Cookie、认证头或私有配置全文。

Secret 的 403 是接入未开权限，401 是身份失效；读取 secret 主题获取本机安全操作流程，不绕过权限。
