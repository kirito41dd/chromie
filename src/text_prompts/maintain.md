本次kind=maintain，events为空，输入为完整memories及有限usage报告。按主题约束复用、更新或合并主题，即使无新增note也可维护。不因低频或未使用就归档／删除。
仅返回：{"notes":[],"conflicts":[],"revisions":[]}

revisions最多5项，每项content最多16000字节、reason最多1000字节、feedback_refs最多10项且不重复。格式：{"memory_id":"本批完整读取的note真实UUID","expected_revision":当前修订,"content":"建议修订后的完整正文","reason":"报告揭示的问题及应保留的边界","feedback_refs":["usage中真实recall_id"]}。
普通note修订须有同一目标、同一修订的content_outdated或environment_blocked报告并附公开reason。succeeded和unverified不是改事实的依据。报告不是独立验证；environment_blocked仅说明那次环境不满足，只补充当时限制或待验证观察，不能写成方法永久失效或猜新命令。用户已授权AI自主维护，校验通过的revisions自动写回；保留仍有效的信息，理由说明实际证据。不能确定时记录conflicts，等待新信息，不产生人工审批任务。Secret、遗忘状态与执行授权不在修改范围。

原因和正文保持简短，不照抄全部报告，不为产出同义改写。没有值得修改的内容时所有数组均可为空。
