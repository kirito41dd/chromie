本次kind=review，仅核对candidates、真实events、检索得到的当前memories及precheck。返回：
{"decisions":[{"candidate_index":0,"action":"create","reason":"简短核对结论","replacement":null}]}

每个候选必须且只能有一个decision，index从0开始。action只允许create、propose_revision、skip、defer；reason最多1000字节，通常一两句，不重抄候选、事件和旧正文。非propose_revision的replacement必须null。不输出notes或conversation_title。

- create：有明确来源、值得复用，与旧记忆不重复或冲突的新结论。
- skip：已有相同含义，或仅为低价值过程、助手建议、普通百科答疑，说明原因。仅讨论备选方案或重申现状、尚未决定，且已有有效结论时，保留原结论，不另外create“仍在讨论／尚未更改”的冗余记忆；有明确跨对话接续价值的未决事项除外。用户明确要求保留的知识，或结合实际系统的决策／方法可保存。一次意见缺主体或被误扩为长期准则时，不因已生成候选就放行。
- propose_revision：自主修订已读普通记忆，适用于有依据的补充、明确更正和事实状态更新；通过服务校验后自动写入。必须完整读取旧note和当前revision。replacement={"memory_id":"已读旧note真实UUID","expected_revision":旧revision,"content":"保留仍有效信息并纳入变更的完整正文"}。保留历史事实的时间范围，避免把当时未完成当成永久状态。偏好和用户决定必须有用户原话；助手报告可记为注明来源和时间的观察，不能写成用户确认或独立验证。不得改Secret、猜测改变范围或遗漏仍有效信息；无法支持完整替换时defer，不用create绕开冲突。
- defer：来源不足、主体不清、歧义／冲突未解决、所需旧记忆没读到，或precheck指出证据错误；给quality_issue说明缺什么，不凑成完整指引。

可省略content；仅确需补全主体、背景或完整链接时提供，使用events已有资料、保持原意。不要重复生成阶段已有content和evidence。额外evidence最多4项：{"event_id":"本次events的UUID","quote":"连续逐字原话"}；每段最多1000字节，不编造或改写。

用户已授权AI自主整理，无须等待逐条审批。同一目标只给一份完整修订，其余重复候选skip。依据尚不足或互相矛盾时，保留有归属的不同说法和待证实状态，或defer等待新证据；不能为消除冲突编造确定结论。来源发生时间决定事实先后，旧消息不能推翻更晚的用户决定。旧建议可能已经不适用，必须对照当前正文重新判断，允许skip，不因曾提出修订就照抄旧方案。

核对候选间重复、同批最终状态及对象／环境／时间／范围，修复前现象不能写成永久现状。retrieval_partial=true不代表所有新结论都不能存；改变旧决定却没找到目标则暂缓。证据用于判断角色、否定和条件，不能只看摘要。不要为每次临时进展制造长期记忆；同一事项优先维护已有结论，允许本次无更新。永久遗忘不属于当前输出能力。

还须检查候选中的专有主体和日期是否来自events，不能因正在使用Chromie就写成“Chromie的业务规则”，也不能把project_key原样当成业务名称。对无依据的主体归属，能保留原意时通过content去掉虚构名称，使用来源已有的描述；影响事实归属而无法确认则defer。不能在reason中重复候选的虚构主体就当成完成核验。
