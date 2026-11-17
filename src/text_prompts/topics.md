主题约束：notes最多5项，每项仅是简短topic入口，content最多16000字节，第一行写稳定主题名称，其余说明适用场景和阅读去向，详情留在引用记忆中。不能用未展开的“第1/3/5项”等会话编号作名称。conflicts最多10项，只能引用已读真实ID并说明矛盾。

每个notes草稿格式：{"memory_id":null,"expected_revision":null,"content":"主题名称\n适用场景与阅读去向","memory_type":"topic","source_refs":[],"links":[{"memory_id":"输入记忆UUID","relation":"derived_from","revision":1}],"merge_from":[]}。
至少一条derived_from依据，目标ID和revision来自输入，不引用自身或未知对象。相同目标和关系只输出一次，不修改普通note或创建topic_alias。

优先更新已读相关topic，填真实memory_id和expected_revision，不能因本批只处理部分分支就新建近义目录。既有主题未读分支会由后端补回，不能假定没输出就会删除。每个主题的link_capacity说明当前关系总数与剩余容量；最终上限32。容量不足时保留原主题，按真实事项建立明确细分入口，不丢旧引用或通过改名制造重复主题。

确实重叠、同范围且已完整读到的topic可合并：保留稳定主题ID，merge_from最多8个被合并topic真实ID。完整保留有效入口并集，不能把同领域但用途不同的事项强行合并；并集超过32则保留分层入口，不强行折叠。不把被合并主题当新派生依据，引用其原有普通记忆。旧入口保留跳转和历史，不删除普通note，不能跨项目合并或合并note。无变化可空notes，不反复同义改写。
