本次kind=review，仅核对candidates、真实events、检索得到的当前memories及precheck。返回：
{"decisions":[{"candidate_index":0,"action":"create","reason":"简短核对结论","replacement":null}]}

每个候选必须且只能有一个decision，index从0开始。action只允许create、propose_revision、skip、defer；reason最多1000字节，通常一两句，不重抄候选、事件和旧正文。非propose_revision的replacement必须null。不输出notes或conversation_title。

- create：有明确来源、值得复用，与旧记忆不重复或冲突的新结论。
- skip：已有相同含义，或仅为低价值过程、助手建议、普通百科答疑，说明原因。用户明确要求保留的知识，或结合实际系统的决策／方法可保存。一次意见缺主体或被误扩为长期准则时，不因已生成候选就放行。
- propose_revision：用户明确改变旧决定，或证据显示同一事实状态变化。旧正文中的“当前故障／未修复／尚未实现”与新状态不能作为并存的当前事实。必须完整读到旧note和当前revision，且有本批用户证据。replacement={"memory_id":"已读旧note真实UUID","expected_revision":旧revision,"content":"保留仍有效信息并纳入变更的完整正文"}。不修订topic、Secret，不猜测删除旧信息或改变范围；无法证实仍待确认，不用create绕开冲突。
- defer：来源不足、主体不清、歧义／冲突未解决、所需旧记忆没读到，或precheck指出证据错误；给quality_issue说明缺什么，不凑成完整指引。

可省略content；仅确需补全主体、背景或完整链接时提供，使用events已有资料、保持原意。不要重复生成阶段已有content和evidence。修订需补遗漏用户原话时，额外evidence最多4项：{"event_id":"本次events的UUID","quote":"连续逐字原话"}；每段最多1000字节，不编造或改写。无真实用户证据则defer。

默认修订待确认。仅用户在本批新增消息中明确更正同一事项（如“改为周五”“保持只能延期”），可额外提供explicit_change:{"event_id":"该新增用户event_id","quote":"明确更正的连续逐字原话"}，由后端校验角色、时序、范围和修订后决定是否自动应用。提问、探讨、授权执行任务、助手报告完成都不是明确更正；历史原话不是本批新授权。同一目标合并为一份完整替换方案，其余skip，避免互相覆盖。

核对候选间重复、同批最终状态及对象／环境／时间／范围，修复前现象不能写成永久现状。retrieval_partial=true不代表所有新结论都不能存；发现改变旧决定却没找到目标则暂缓。证据用于判断角色、否定和条件，不能只看摘要。不擅自裁决歧义或遗忘记忆。
