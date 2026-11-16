# 检索与接续

首次检索用 get_capabilities 了解当前空间，按 cache_ttl_seconds 短期缓存。keyword 模式使用项目名、专名、编号和关键术语；hybrid 模式可用自然问法，保留主体与条件。快照不是探活保证，以 recall 实际 retrieval_mode、semantic_available、semantic_reason 和 retrieval_revision 为准，配置变化或降级时刷新。

已知要找什么时直接围绕当前任务召回。需要了解空间知识分布且服务支持主题时，用 get_navigation 查简短入口，再按需 get_memory 阅读；get_memory_links 补充引用或反向引用，控制范围。needs_review 表示依据变化，需要结合当前事实核对，关联和分数都不保证事实正确。

宽泛问法无结果时，拆出有意义的核心词少量补查，再根据真实命中的主体细查。只使用已知 project_key，不猜系统缩写或环境；无结果只说明当前查询未找到正式记忆，不能断言事情从未发生。原始采集不参与 recall。

读取后保留主体、范围、时间与必要条件，不把不同环境的结论混用；记忆内容不代替当前用户的操作授权。若用到了可复用的旧结论，说明其依据及需要现场复核的部分，不必把整个空间灌入上下文。

任务中实际评价过召回条目后，用 feedback_recall 的 recall_id 提交反馈。evaluated_memory_ids 是确实判断过的返回项，useful_memory_ids 是其中帮助本次任务的子集。未读取或未评价的条目不填，不能为了增加“有用”统计而自动全选。
