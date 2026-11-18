# 检索与接续

首次检索用 get_capabilities 了解当前空间，按 cache_ttl_seconds 短期缓存。keyword 模式使用项目名、专名、编号和关键术语；hybrid 模式可用自然问法，保留主体与条件。快照不是探活保证，以 recall 实际 retrieval_mode、semantic_available、semantic_reason 和 retrieval_revision 为准，配置变化或降级时刷新。

已知要找什么时直接围绕当前任务召回。需要了解空间知识分布且服务支持主题时，用 get_navigation 查简短入口，再按需 get_memory 阅读；get_memory_links 补充引用或反向引用，控制范围。needs_review 表示依据变化，需要结合当前事实核对，关联和分数都不保证事实正确。

宽泛问法无结果时，拆出有意义的核心词少量补查，再根据真实命中的主体细查。只使用已知 project_key，不猜系统缩写或环境；无结果只说明当前查询未找到正式记忆，不能断言事情从未发生。原始采集不参与 recall。

要找已有凭据供本地工具连接服务时，按当前 recall 的工具定义传 `prefer_secret_refs: true`；查询写服务、环境和用途。只查操作步骤或排障背景时用默认排序，即使问题里提到密码或鉴权。按公开说明核对服务、环境和用途，尤其是同名服务有测试和生产两套凭据时。没有符合条件的引用时可读返回的操作说明；普通说明里的文字不能当作凭据。

召回先看排名、适用项目和正文。条目带有 `content_truncated: true` 时，若判断所需的条件或步骤落在片段之外，按该条 `memory_id` 调用 `get_memory`；有后续游标可继续读同一修订。短记忆已交代清楚时可直接使用。需要核对原话时读取 evidence 视图。

读取后保留主体、范围、时间与必要条件，不把不同环境的结论混用；记忆内容不代替当前用户的操作授权。若用到了可复用的旧结论，说明其依据及需要现场复核的部分，不必把整个空间灌入上下文。

任务得出结果后、最终答复前，合并本轮实际评价过的召回项，用 feedback_recall 的 recall_id 提交反馈。evaluated_memory_ids 是确实判断过的返回项，useful_memory_ids 是其中帮助本次任务的子集。未读取或未评价的条目不填，不能为了增加“有用”统计而自动全选。保留实际召回的 recall_id 与 memory_id，不能用 get_memory 的请求号代替。反馈只追加使用记录，不调用 remember/correct_memory，不改变正文；业务检查只读或暂不更正记忆时仍可反馈，用户明确禁止记录/反馈则不提交。

已评价但对任务无用时，可在同次召回的 negative_reasons 填 {memory_id,reason}。对象、项目或环境不符用 scope_mismatch，需要另一种结果（如凭据引用）用 wrong_result_type，对象相关却缺少所问细节用 missing_detail。只选最贴切的一项，拿不准可省略。已有负评可补一次原因。

实际按某条记忆执行后，在同一个 feedback_recall 的 usage 数组补充 {memory_id,outcome,reason}，和“有用”判断相互独立。outcome 是 succeeded（实际成功）、content_outdated（内容过时或错误）、environment_blocked（本次环境/权限/依赖不满足）、unverified（使用了但结果尚未验证）。只报实际使用项，不把“读过/觉得相关”记成成功；原因简短公开，不带凭据、日志或敏感原文。检查按规则发现告警不等于方法失败；旧端点与预期不符则可报内容过时，不能因为最终找到新端点就把旧流程报成功。未验证可补充最终结果，同一次召回项最终结果固定；新尝试用新的召回。服务自动绑定原 Agent 与召回时修订。环境受限不等于方法永久失效；用户授权更正且有足够事实时按 correct_memory 及时修订，未授权时只报告问题；实例启用后台维护后由 AI 核对报告、当前正文和修订，再自主维护，不要求逐条审批。
