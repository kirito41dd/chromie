本次kind=extract，仅生成候选，不读取旧决定，不输出notes、decisions或revisions。返回：
{"candidates":[{"content":"独立可理解的结论","category":"fact","search_query":"关键实体 主题 目的","source_refs":["真实event_id"],"evidence":[{"event_id":"真实event_id","quote":"连续逐字引文"}]}],"conversation_title":null}

最多5条候选。category只能是fact（已验证事实）、decision（用户决定）、preference（明确持久偏好）、method（可复用方法）。每条content最多16000字节，通常数句表达一个结论，不照抄会话。search_query用2～5个辨识主题或实体词、空格分隔，最多256字节。每条最多16个source_refs、4段引文；quote最多1000字节，必须连续逐字存在于该event.body，不能拼接、改写或省略，evidence事件必须在source_refs中。

优先明确决定、稳定约定、解决过的典型问题和有条件的方法。同批同一事项取最后确认状态；修复前现象标为历史，故障／部署状态写清发生时间与当时范围。不逐轮记账，不保存一次点击、临时失败、分支/MR编号、孤立测试计数或助手计划。“用户要求保存X”不是X本身，须有实质内容。

一般技术答疑、百科定义不自动变成个人记忆；用户明确希望长期保留，或结合具体系统形成可复用决策、约束、排障经验时才保存。一次任务的表达要求不能升级为永久偏好。一次候选人评价须能确认对应对象和事件，不能保存成“本次候选人应该如何评价”的通用准则；缺主体且无法补齐时不产出，不猜身份。

preference须有用户明确原话。反问、否定、抱怨、批评不等于偏好，不能从助手建议推断喜好。不能把建议写成执行事实，或把许可缩成只限测试。方法保留前提、关键步骤和完整必要链接／路径，不能把“.../install.sh”“按上文第3点”当实际步骤。缺来源不编造，可空candidates。

带conversation时，new_event_ids是本批新增消息，其余events仅为历史，每条候选至少引用一条新增event。conversation_title是近期主要事项单行标题，最多240字节，通常8～20个汉字；同事项追问或临时插话保持current_title，明确转向才更新；title_locked为true保持current_title。没有conversation或没有明确事项时返回null。空候选也可命名会话。
