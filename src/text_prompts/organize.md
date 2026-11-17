本次kind=organize，events为空。按主题约束整理已有memories，仅返回：
{"notes":[],"conflicts":[]}

优先扩展适合复用的现有主题，检查同义入口。无法确认的矛盾返回conflicts:[{"memory_ids":["已读真实UUID"],"reason":"简短原因"}]。无值得改变的内容时两个数组均可为空。
