//! 只观察从对话提炼新建的普通记忆；提炼任务触碰过的旧记忆不进入分母。
use super::*;
use sea_orm::{FromQueryResult, raw_sql};

#[derive(FromQueryResult)]
struct Usage {
    generated_count: i64,
    recalled: i64,
    evaluated: i64,
    scope_mismatch: i64,
    wrong_result_type: i64,
    missing_detail: i64,
    unspecified: i64,
}

impl Store {
    pub async fn extraction_usage(&self, space: SpaceId) -> Result<Value> {
        let end = now();
        let start = end - time::Duration::days(7);
        let space_id = space.bytes();
        // 首版 change_reason 只能由服务端写入。与任务主键核对可区分提炼和主题整理，
        // 即使任务详情已清理，或该记忆后来修订过，创建来源仍可辨认。
        let usage = Usage::find_by_statement(raw_sql!(
            MySql,
            r#"
            WITH cohort AS (
                SELECT m.space_id, m.id, m.created_at
                FROM memories m
                JOIN memory_revisions v
                  ON v.space_id = m.space_id AND v.memory_id = m.id AND v.revision = 1
                JOIN memory_revisions cur
                  ON cur.space_id = m.space_id AND cur.memory_id = m.id AND cur.revision = m.current_revision
                WHERE m.space_id = {space_id} AND m.created_at BETWEEN {start} AND {end}
                  AND m.forgotten_at IS NULL AND v.agent_id IS NULL
                  AND v.memory_type = 'note' AND cur.memory_type = 'note'
                  AND NOT EXISTS (SELECT 1 FROM secrets s WHERE s.space_id = m.space_id AND s.memory_id = m.id)
                  AND (EXISTS (
                      SELECT 1 FROM knowledge_jobs j
                      WHERE j.space_id = m.space_id AND j.kind = 'extract'
                        AND j.id = IF(IS_UUID(RIGHT(v.change_reason, 36)), UUID_TO_BIN(RIGHT(v.change_reason, 36)), NULL)
                        AND v.change_reason = CONCAT('后台 LLM 整理：任务 ', BIN_TO_UUID(j.id))
                  ) OR (v.content IS NULL AND v.change_reason = '后台 LLM 整理（历史正文已清理）' AND EXISTS (
                      SELECT 1 FROM knowledge_jobs j
                      WHERE j.space_id = m.space_id AND j.kind = 'extract'
                        AND j.created_at <= v.created_at AND j.updated_at >= v.created_at
                        AND (JSON_CONTAINS(j.result, JSON_OBJECT('action','create','published_memory_id',BIN_TO_UUID(m.id)), '$.decisions')
                             OR EXISTS (
                                 SELECT 1 FROM write_receipts w
                                 WHERE w.space_id = m.space_id AND w.operation = 'background_revision'
                                   AND w.target_id = m.id AND w.revision = 1 AND w.agent_id IS NULL
                                   AND w.input_hash = UNHEX(SHA2(CONCAT('["',BIN_TO_UUID(m.id),'",1,"',BIN_TO_UUID(j.id),'"]'),256))
                             ))
                  )))
            ), hits AS (
                SELECT i.space_id, i.memory_id, r.created_at,
                       CASE WHEN i.evaluated_at BETWEEN {start} AND {end} THEN i.useful END AS useful,
                       i.negative_reason
                FROM recall_runs r JOIN recall_items i ON i.space_id = r.space_id AND i.recall_id = r.id
                WHERE r.space_id = {space_id} AND r.channel = 'agent' AND r.created_at BETWEEN {start} AND {end}
            )
            SELECT COUNT(DISTINCT c.id) AS generated_count, COUNT(DISTINCT h.memory_id) AS recalled,
                   COUNT(DISTINCT CASE WHEN h.useful IS NOT NULL THEN c.id END) AS evaluated,
                   COUNT(CASE WHEN h.useful = 0 AND h.negative_reason = 'scope_mismatch' THEN 1 END) AS scope_mismatch,
                   COUNT(CASE WHEN h.useful = 0 AND h.negative_reason = 'wrong_result_type' THEN 1 END) AS wrong_result_type,
                   COUNT(CASE WHEN h.useful = 0 AND h.negative_reason = 'missing_detail' THEN 1 END) AS missing_detail,
                   COUNT(CASE WHEN h.useful = 0 AND (h.negative_reason IS NULL OR
                         h.negative_reason NOT IN ('scope_mismatch','wrong_result_type','missing_detail')) THEN 1 END) AS unspecified
            FROM cohort c LEFT JOIN hits h ON h.space_id = c.space_id AND h.memory_id = c.id AND h.created_at >= c.created_at
            "#
        ))
        .one(&self.db)
        .await?
        .ok_or_else(Error::unavailable)?;
        Ok(json!({
            "window_start": crate::types::timestamp(start)?, "window_end": crate::types::timestamp(end)?,
            "generated": usage.generated_count, "recalled": usage.recalled, "evaluated": usage.evaluated,
            "not_recalled": usage.generated_count - usage.recalled,
            "negative_reasons": {"scope_mismatch": usage.scope_mismatch, "wrong_result_type": usage.wrong_result_type,
                "missing_detail": usage.missing_detail, "unspecified": usage.unspecified}
        }))
    }
}
