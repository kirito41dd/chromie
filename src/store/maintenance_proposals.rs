//! 维护只对 Agent 报告形成待确认草案；发布和确认都复核实际使用修订，不凭报告直接改事实。
use super::knowledge_jobs::Snapshot;
use super::*;
use crate::{knowledge::MemoryType, recall_usage::UseOutcome, text_model::MaintenanceRevision};
use std::collections::HashSet;

impl Store {
    pub(super) fn maintenance_proposals(
        snapshot: &Snapshot,
        drafts: Vec<MaintenanceRevision>,
    ) -> Result<Vec<Value>> {
        if drafts.len() > 5
            || (snapshot.kind != crate::text_model::JobKind::Maintain && !drafts.is_empty())
        {
            return Err(crate::text_model::error(
                "model_evidence_invalid",
                "只有独立维护可提出使用反馈修订，每批最多5项",
            ));
        }
        let mut seen = HashSet::new();
        let mut records = vec![];
        for draft in drafts {
            crate::types::bounded(&draft.content, 16000)?;
            crate::types::bounded(&draft.reason, 1000)?;
            if !seen.insert(draft.memory_id)
                || draft.feedback_refs.is_empty()
                || draft.feedback_refs.len() > 10
                || crate::extraction::incomplete_method(&draft.content)
            {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "维护草案目标重复、依据缺失或正文不完整",
                ));
            }
            let old = snapshot
                .memories
                .iter()
                .find(|m| {
                    m.id == draft.memory_id
                        && m.revision == draft.expected_revision
                        && m.memory_type == MemoryType::Note
                })
                .ok_or_else(|| {
                    crate::text_model::error(
                        "model_evidence_invalid",
                        "维护只能修订已完整读取的普通记忆",
                    )
                })?;
            let mut evidence = vec![];
            for id in &draft.feedback_refs {
                let proof = snapshot
                    .usage
                    .iter()
                    .find(|e| {
                        e.recall_id == *id
                            && e.memory_id == old.id
                            && e.revision == old.revision
                            && matches!(
                                e.outcome,
                                UseOutcome::ContentOutdated | UseOutcome::EnvironmentBlocked
                            )
                            && e.reason.is_some()
                    })
                    .ok_or_else(|| {
                        crate::text_model::error(
                            "model_evidence_invalid",
                            "维护修订没有同一目标修订的实际问题报告",
                        )
                    })?;
                if !evidence
                    .iter()
                    .any(|e: &super::use_reports::UseEvidence| e.recall_id == proof.recall_id)
                {
                    evidence.push(proof.clone());
                }
            }
            let details=evidence.iter().map(|e|json!({"role":"agent_reported","quote":e.reason,"occurred_at":e.reported_at,"reference":format!("recall:{} / agent:{}",e.recall_id,e.agent_id)})).collect::<Vec<_>>();
            records.push(json!({"candidate_index":records.len(),"action":"propose_revision","category":"method","reason":draft.reason,"content":draft.content,"replacement":{"memory_id":old.id,"expected_revision":old.revision,"content":draft.content},"previous_content":old.content,"source_refs":[],"evidence":[],"evidence_details":details,"usage_evidence":evidence,"applied":false}));
        }
        Ok(records)
    }
}
