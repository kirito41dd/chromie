//! 手动与自动工作均保存固定输入；接收消息本身不直接调用模型。
use super::*;
use crate::{
    entity::{knowledge_jobs as jobs, source_events, text_model_settings},
    text_model::{JobKind, Queue},
};
use sea_orm::{QueryOrder, sea_query::Expr};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct MemorySnapshot {
    pub id: Uuid,
    pub revision: i32,
    pub content: String,
    pub memory_type: crate::knowledge::MemoryType,
    pub project_key: Option<String>,
    #[serde(default)]
    pub links: Vec<crate::knowledge::Link>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EventSnapshot {
    pub event_id: Uuid,
    pub role: String,
    pub body: String,
    pub project_key: Option<String>,
    pub occurred_at: String,
    pub session_id: String,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic: Option<AutomaticSnapshot>,
    #[serde(default)]
    pub review: Option<ReviewSnapshot>,
    pub kind: JobKind,
    pub memories: Vec<MemorySnapshot>,
    pub events: Vec<EventSnapshot>,
    #[serde(default)]
    pub usage: Vec<super::use_reports::UseEvidence>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct AutomaticSnapshot {
    pub settings_revision: i64,
    pub memory_ids: Vec<Uuid>,
    #[serde(default)]
    pub conversation: Option<ConversationSnapshot>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ConversationSnapshot {
    pub conversation: crate::messages::Conversation,
    pub title: Option<String>,
    pub title_revision: i64,
    pub title_locked: bool,
    pub work_event_ids: Vec<Uuid>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ReviewSnapshot {
    pub candidates: Vec<crate::extraction::Candidate>,
    pub conversation_title: Option<String>,
    pub retrieval_partial: bool,
    pub rejected: std::collections::BTreeMap<usize, String>,
}

impl Store {
    pub async fn enqueue_knowledge(&self, space: SpaceId, input: Queue) -> Result<Value> {
        if input.memory_ids.len() > 10
            || input.event_ids.len() > 10
            || (input.kind != JobKind::Extract && !input.event_ids.is_empty())
            || (input.kind == JobKind::Extract && input.event_ids.is_empty())
        {
            return Err(Error::invalid(
                "每次整理最多选择 10 条记忆，提炼须选择 1～10 条采集消息",
            ));
        }
        let tx = self.begin(space).await?;
        let result = self.enqueue_knowledge_tx(&tx, space, input, None).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn enqueue_knowledge_tx(
        &self,
        tx: &DatabaseTransaction,
        space: SpaceId,
        input: Queue,
        automatic: Option<AutomaticSnapshot>,
    ) -> Result<Value> {
        let hash = Self::hash(&input)?;
        let request_hash: String = hash.iter().map(|v| format!("{v:02x}")).collect();
        if let Some(row) =
            jobs::Entity::find_by_id((space.bytes(), input.request_id.as_bytes().to_vec()))
                .one(tx)
                .await?
        {
            if row.result.as_ref().and_then(|v| v["request_hash"].as_str())
                != Some(request_hash.as_str())
            {
                return Err(Error::conflict("idempotency_conflict"));
            }
            return Ok(Self::job_view(&row));
        }
        let config = text_model_settings::Entity::find_by_id(1u8)
            .one(tx)
            .await?
            .filter(|row| row.enabled != 0)
            .ok_or_else(|| Error::invalid("请先在系统设置中配置并启用文本模型"))?;
        let explicit = !input.memory_ids.is_empty()
            || automatic.as_ref().is_some_and(|a| a.conversation.is_some());
        let ids = if input.kind == JobKind::Extract && !explicit {
            vec![]
        } else if explicit {
            input.memory_ids.clone()
        } else {
            memories::Entity::find()
                .filter(memories::Column::SpaceId.eq(space.bytes()))
                .filter(memories::Column::ForgottenAt.is_null())
                .order_by_desc(memories::Column::UpdatedAt)
                .limit(40)
                .all(tx)
                .await?
                .into_iter()
                .map(|row| uuid_from_bytes(&row.id))
                .collect::<Result<Vec<_>>>()?
        };
        let mut snapshot = Snapshot {
            kind: input.kind,
            review: None,
            automatic,
            memories: vec![],
            events: vec![],
            usage: vec![],
        };
        for id in ids {
            if snapshot.memories.len() == if snapshot.automatic.is_some() { 64 } else { 10 } {
                break;
            }
            let head = Self::head(tx, space, id).await?;
            if head.forgotten_at.is_some() {
                return Err(Error::missing());
            }
            if crate::entity::secrets::Entity::find_by_id((space.bytes(), head.id.clone()))
                .one(tx)
                .await?
                .is_some()
            {
                if explicit {
                    return Err(Error::invalid("后台整理不接收 Secret，请只选择普通记忆"));
                }
                continue;
            }
            let row = Self::revision(tx, &head).await?;
            snapshot.memories.push(MemorySnapshot {
                id,
                revision: row.revision,
                content: row.content.ok_or_else(Error::missing)?,
                memory_type: crate::knowledge::MemoryType::parse(&row.memory_type)?,
                project_key: row.project_key,
                links: Self::links(tx, space, id, row.revision).await?,
            });
        }
        let mut seen_events = std::collections::HashSet::new();
        for id in &input.event_ids {
            if !seen_events.insert(*id) {
                continue;
            }
            let row = source_events::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .one(tx)
                .await?
                .filter(|row| row.forgotten_at.is_none())
                .ok_or_else(Error::missing)?;
            snapshot.events.push(EventSnapshot {
                event_id: *id,
                role: row.role,
                body: row.body.ok_or_else(Error::missing)?,
                project_key: row.project_key,
                occurred_at: crate::types::timestamp(row.occurred_at)?,
                session_id: row.session_id,
            });
        }
        snapshot.events.sort_by(|a, b| {
            a.occurred_at
                .cmp(&b.occurred_at)
                .then(a.event_id.cmp(&b.event_id))
        });
        if input.kind == JobKind::Maintain {
            snapshot.usage = Self::usage_evidence(
                tx,
                space,
                &snapshot
                    .memories
                    .iter()
                    .filter(|m| m.memory_type == crate::knowledge::MemoryType::Note)
                    .map(|m| (m.id, m.revision))
                    .collect::<Vec<_>>(),
            )
            .await?;
        }
        if input.kind != JobKind::Extract && snapshot.memories.is_empty() {
            return Err(Error::invalid("当前没有可整理的普通记忆"));
        }
        let payload = serde_json::to_value(&snapshot).map_err(|_| Error::unavailable())?;
        if payload.to_string().len() > 128 * 1024 {
            return Err(Error {
                code: "knowledge_input_too_large",
                message: "整理范围超过 128 KiB，请减少选取内容",
                status: reqwest::StatusCode::BAD_REQUEST,
            });
        }
        let current = now();
        let row = jobs::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(input.request_id.as_bytes().to_vec()),
            kind: Set(input.kind.as_str().into()),
            automatic: Set(i8::from(snapshot.automatic.is_some())),
            config_revision: Set(config.revision),
            state: Set("pending".into()),
            payload: Set(Some(payload)),
            result: Set(Some(json!({"request_hash":request_hash}))),
            lease_owner: Set(None),
            lease_expires_at: Set(None),
            attempts: Set(0),
            run_after: Set(current),
            created_at: Set(current),
            updated_at: Set(current),
            last_error: Set(None),
        }
        .insert(tx)
        .await?;
        Ok(Self::job_view(&row))
    }

    pub(super) fn job_view(row: &jobs::Model) -> Value {
        let mut result = row.result.clone().unwrap_or_else(|| json!({}));
        if let Some(decisions) = result.get("decisions").and_then(Value::as_array) {
            let mut counts = serde_json::Map::new();
            for action in ["create", "propose_revision", "skip", "defer"] {
                counts.insert(
                    action.into(),
                    json!(
                        decisions
                            .iter()
                            .filter(|d| d["action"] == action && d["resolution"] != "superseded")
                            .count()
                    ),
                );
            }
            counts.insert(
                "applied".into(),
                json!(decisions.iter().filter(|d| d["applied"] == true).count()),
            );
            result["counts"] = Value::Object(counts);
            result["has_review"] = json!(true);
        }
        if let Some(fields) = result.as_object_mut() {
            fields.remove("request_hash");
            fields.remove("decisions");
        }
        json!({"automatic":row.automatic!=0,"job_id":uuid_from_bytes(&row.id).ok(),"kind":row.kind,"state":row.state,"attempts":row.attempts,"last_error":row.last_error,
            "result":result,"created_at":row.created_at.to_string(),"updated_at":row.updated_at.to_string()})
    }

    pub async fn knowledge_jobs(&self, space: SpaceId) -> Result<Value> {
        let mut rows = jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .order_by_desc(jobs::Column::CreatedAt)
            .limit(20)
            .all(&self.db)
            .await?;
        // 待确认方案单独补读，避免普通新任务把旧方案挤出最近记录。
        let proposals = jobs::Entity::find()
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .filter(jobs::Column::State.eq("done"))
            .filter(Expr::cust(
                "JSON_EXTRACT(result, '$.pending_revisions') > 0",
            ))
            .order_by_asc(jobs::Column::CreatedAt)
            .limit(20)
            .all(&self.db)
            .await?;
        for row in proposals {
            if !rows.iter().any(|existing| existing.id == row.id) {
                rows.push(row);
            }
        }
        let enabled = text_model_settings::Entity::find_by_id(1u8)
            .one(&self.db)
            .await?
            .is_some_and(|row| row.enabled != 0);
        Ok(
            json!({"organization":self.organization_status(space).await?,"extraction":self.extraction_status(space).await?,"maintenance":self.maintenance_status(space).await?,"enabled":enabled,"jobs":rows.iter().map(Self::job_view).collect::<Vec<_>>()}),
        )
    }

    /// 快照和冲突说明可能含被遗忘的信息，空间遗忘时清空后台副本并阻断在途发布。
    pub(super) async fn clear_knowledge_snapshots<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
    ) -> Result<()> {
        Self::release_extraction(db, Some(space)).await?;
        Self::release_maintenance(db, Some(space)).await?;
        crate::entity::recall_items::Entity::update_many()
            .col_expr(
                crate::entity::recall_items::Column::UseReason,
                Expr::value(None::<String>),
            )
            .filter(crate::entity::recall_items::Column::SpaceId.eq(space.bytes()))
            .exec(db)
            .await?;
        Self::clear_conversation_titles(db, space).await?;
        use crate::entity::organization_items as items;
        items::Entity::update_many()
            .col_expr(items::Column::State, Expr::value("pending"))
            .col_expr(items::Column::RunAfter, Expr::value(now()))
            .filter(items::Column::SpaceId.eq(space.bytes()))
            .filter(items::Column::State.eq("queued"))
            .exec(db)
            .await?;
        jobs::Entity::update_many()
            .col_expr(jobs::Column::Payload, Expr::value(Option::<Value>::None))
            .col_expr(jobs::Column::Result, Expr::value(Option::<Value>::None))
            .col_expr(jobs::Column::State, Expr::value("cancelled"))
            .col_expr(
                jobs::Column::LeaseOwner,
                Expr::value(Option::<Vec<u8>>::None),
            )
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Option::<time::PrimitiveDateTime>::None),
            )
            .col_expr(jobs::Column::UpdatedAt, Expr::value(now()))
            .filter(jobs::Column::SpaceId.eq(space.bytes()))
            .exec(db)
            .await?;
        Ok(())
    }
}
