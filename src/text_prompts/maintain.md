本次kind=maintain，events为空，输入为完整memories及有限usage报告。按主题约束复用、更新或合并主题，即使无新增note也可维护。不因低频或未使用就归档／删除。
仅返回：{"notes":[],"conflicts":[],"revisions":[]}

revisions最多5项，每项content最多16000字节、reason最多1000字节、feedback_refs最多10项且不重复。格式：{"memory_id":"本批完整读取的note真实UUID","expected_revision":当前修订,"content":"建议修订后的完整正文","reason":"报告揭示的问题及应保留的边界","feedback_refs":["usage中真实recall_id"]}。
普通note修订须有同一目标、同一修订的content_outdated或environment_blocked报告并附公开reason。succeeded和unverified不是改事实的依据。报告不是独立验证；environment_blocked仅说明那次环境不满足，不能写成方法永久失效或猜新命令。缺事实则conflicts说明，不强行修订。草案均待用户确认，不直接更改note、Secret、遗忘状态或授权。

原因和正文保持简短，不照抄全部报告，不为产出同义改写。没有值得修改的内容时所有数组均可为空。
