use super::*;
use crate::{
    entity::{memory_sources, source_events},
    types::{GetMemory, View, output_budget, timestamp},
};
use sea_orm::QueryOrder;

// 只裁剪正文及结果条数，不能裁剪对象 ID、版本或空间标识。
pub fn budget(mut value: Value, max_chars: usize) -> Value {
    fn shorten(value: &mut Value) -> bool {
        let mut changed = false;
        match value {
            Value::Object(fields) => {
                for (name, field) in fields {
                    if name == "contract" {
                        continue;
                    }
                    if matches!(name.as_str(), "content" | "body" | "change_reason") {
                        if let Value::String(text) = field {
                            let length = text.chars().count();
                            if length > 32 {
                                *text = text.chars().take(length / 2).collect();
                                changed = true;
                            }
                        }
                    } else {
                        changed |= shorten(field);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    changed |= shorten(item);
                }
            }
            _ => {}
        }
        changed
    }
    let limit = max_chars.saturating_sub(128);
    while value.to_string().chars().count() > limit {
        value["truncated"] = json!(true);
        if shorten(&mut value) {
            continue;
        }
        let mut removed = false;
        for name in ["memories", "history", "sources"] {
            if let Some(items) = value.get_mut(name).and_then(Value::as_array_mut)
                && !items.is_empty()
            {
                items.pop();
                removed = true;
                let next = items.last().and_then(|v| {
                    v.get(if name == "history" {
                        "revision"
                    } else {
                        "event_id"
                    })
                });
                let next = next.map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                });
                if name != "memories" {
                    value["next_cursor"] = json!(next);
                }
                break;
            }
        }
        if !removed {
            return json!({"truncated":true,"code":"budget_too_small","message":"元数据超出预算，请增大 max_chars"});
        }
    }
    value
}

impl Store {
    pub(super) async fn card<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        selected: Option<i32>,
    ) -> Result<Value> {
        let mut head = Self::head(db, space, id).await?;
        if head.forgotten_at.is_some() {
            return Err(Error::missing());
        }
        let is_current = selected.is_none_or(|version| version == head.current_revision);
        if let Some(version) = selected {
            head.current_revision = version;
        }
        let revision = Self::revision(db, &head).await?;
        let sources = memory_sources::Entity::find()
            .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
            .filter(memory_sources::Column::MemoryId.eq(id.as_bytes().to_vec()))
            .filter(memory_sources::Column::Revision.eq(head.current_revision))
            .all(db)
            .await?;
        let refs = sources
            .iter()
            .map(|v| uuid_from_bytes(&v.event_id))
            .collect::<Result<Vec<_>>>()?;
        let task = outbox_tasks::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(db)
            .await?;
        let processing = if !is_current {
            "historical"
        } else {
            task.map(|task| {
                if task.state == "done" {
                    "index_ready"
                } else {
                    "index_pending"
                }
            })
            .unwrap_or("index_unknown")
        };
        let mut value = json!({"kind":"memory","memory_id":id,"revision":head.current_revision,"is_current":is_current,"content":revision.content,"processing_state":processing,
            "project_key":revision.project_key,"updated_at":timestamp(revision.created_at)?,"source_refs":refs,
            "evidence_status":if refs.is_empty(){"unverified"}else{"source_linked"}});
        value["origin"] = Self::memory_origin(db, space, id, revision.agent_id.as_deref()).await?;
        value["created_by"] = Self::memory_origin(db, space, id, head.agent_id.as_deref()).await?;
        Self::decorate_secret(db, space, id, &mut value).await?;
        Ok(value)
    }

    pub async fn get(&self, space: SpaceId, input: GetMemory) -> Result<Value> {
        let maximum = output_budget(input.max_chars)?;
        if let (None, Some(id)) = (input.memory_id, input.event_id) {
            let event = source_events::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .one(&self.db)
                .await?
                .ok_or_else(Error::missing)?;
            if event.forgotten_at.is_some() {
                return Err(Error::missing());
            }
            let offset = input
                .cursor
                .as_deref()
                .unwrap_or("0")
                .parse::<usize>()
                .map_err(|_| Error::invalid("事件游标无效"))?;
            let body = event.body.unwrap_or_default();
            let total = body.chars().count();
            if offset > total {
                return Err(Error::invalid("事件游标超出范围"));
            }
            let mut count = total.saturating_sub(offset).min(maximum);
            loop {
                if count == 0 && offset < total {
                    return Err(Error::invalid("元数据超出预算，请增大 max_chars"));
                }
                let next = (offset + count < total).then(|| (offset + count).to_string());
                let value = json!({"kind":"source_event","event_id":id,"session_id":event.session_id,"turn_id":event.turn_id,"role":event.role,
                    "project_key":event.project_key,"occurred_at":timestamp(event.occurred_at)?,"body":body.chars().skip(offset).take(count).collect::<String>(),
                    "next_cursor":next,"truncated":next.is_some(),"evidence_status":"raw_statement","origin":Self::source_label(&self.db, space, event.agent_id.as_deref()).await?});
                if value.to_string().chars().count() + 128 <= maximum {
                    return Ok(value);
                }
                if count == 0 {
                    return Err(Error::invalid("元数据超出预算，请增大 max_chars"));
                }
                count /= 2;
            }
        }
        let Some(id) = input.memory_id.filter(|_| input.event_id.is_none()) else {
            return Err(Error::invalid("须且只能指定一个记忆或原始事件"));
        };
        let head = Self::head(&self.db, space, id).await?;
        if head.forgotten_at.is_some() {
            return Ok(json!({"memory_id":id,"hidden":true}));
        }
        let mut value = match input.view {
            View::Current => {
                let (version, offset) = match input.cursor.as_deref() {
                    Some(cursor) => {
                        let (version, offset) = cursor
                            .split_once(':')
                            .ok_or_else(|| Error::invalid("正文游标无效"))?;
                        (
                            version
                                .parse::<i32>()
                                .map_err(|_| Error::invalid("修订无效"))?,
                            offset
                                .parse::<usize>()
                                .map_err(|_| Error::invalid("正文偏移无效"))?,
                        )
                    }
                    None => (head.current_revision, 0),
                };
                let mut card = Self::card(&self.db, space, id, Some(version)).await?;
                if card.get("kind").and_then(Value::as_str) == Some("secret_ref")
                    && card.get("is_current").and_then(Value::as_bool) == Some(true)
                {
                    card["contract"] = Self::contract(&self.db, space, id).await?.value()?;
                }
                let body = card
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(Error::missing)?
                    .to_owned();
                let total = body.chars().count();
                if offset > total {
                    return Err(Error::invalid("正文游标超出范围"));
                }
                let mut count = total.saturating_sub(offset).min(maximum);
                loop {
                    if count == 0 && offset < total {
                        return Err(Error::invalid("元数据超出预算，请增大 max_chars"));
                    }
                    let next =
                        (offset + count < total).then(|| format!("{version}:{}", offset + count));
                    card["content"] =
                        json!(body.chars().skip(offset).take(count).collect::<String>());
                    card["next_cursor"] = json!(next);
                    card["truncated"] = json!(next.is_some());
                    if card.to_string().chars().count() + 128 <= maximum {
                        return Ok(card);
                    }
                    if count == 0 {
                        return Err(Error::invalid("元数据超出预算，请增大 max_chars"));
                    }
                    count /= 2;
                }
            }
            View::History => {
                let before = input
                    .cursor
                    .as_deref()
                    .map(|v| v.parse::<i32>().map_err(|_| Error::invalid("修订游标无效")))
                    .transpose()?;
                let mut query = memory_revisions::Entity::find()
                    .filter(memory_revisions::Column::SpaceId.eq(space.bytes()))
                    .filter(memory_revisions::Column::MemoryId.eq(head.id));
                if let Some(before) = before {
                    query = query.filter(memory_revisions::Column::Revision.lt(before));
                }
                let mut rows = query
                    .order_by_desc(memory_revisions::Column::Revision)
                    .limit(11)
                    .all(&self.db)
                    .await?;
                let more = rows.len() > 10;
                rows.truncate(10);
                let next = if more {
                    rows.last().map(|row| row.revision.to_string())
                } else {
                    None
                };
                let mut history = Vec::new();
                for row in rows {
                    history.push(json!({"revision":row.revision,"content":row.content,"content_cursor":format!("{}:0",row.revision),"project_key":row.project_key,"change_reason":row.change_reason,
                        "origin":Self::memory_origin(&self.db, space, id, row.agent_id.as_deref()).await?}));
                }
                json!({"memory_id":id,"history":history,"next_cursor":next,"truncated":more})
            }
            View::Evidence => {
                let mut query = memory_sources::Entity::find()
                    .filter(memory_sources::Column::SpaceId.eq(space.bytes()))
                    .filter(memory_sources::Column::MemoryId.eq(head.id))
                    .filter(memory_sources::Column::Revision.eq(head.current_revision));
                if let Some(cursor) = input.cursor {
                    let cursor =
                        Uuid::parse_str(&cursor).map_err(|_| Error::invalid("来源游标无效"))?;
                    query = query
                        .filter(memory_sources::Column::EventId.gt(cursor.as_bytes().to_vec()));
                }
                let mut refs = query
                    .order_by_asc(memory_sources::Column::EventId)
                    .limit(11)
                    .all(&self.db)
                    .await?;
                let more = refs.len() > 10;
                refs.truncate(10);
                let next = if more {
                    refs.last()
                        .map(|row| uuid_from_bytes(&row.event_id).map(|id| id.to_string()))
                        .transpose()?
                } else {
                    None
                };
                let mut sources = Vec::new();
                for source in refs {
                    let event = source_events::Entity::find_by_id((space.bytes(), source.event_id))
                        .one(&self.db)
                        .await?
                        .ok_or_else(Error::missing)?;
                    if event.forgotten_at.is_some() {
                        continue;
                    }
                    sources.push(json!({"origin":Self::source_label(&self.db, space, event.agent_id.as_deref()).await?,"event_id":uuid_from_bytes(&event.id)?,"session_id":event.session_id,"role":event.role,"occurred_at":timestamp(event.occurred_at)?,
                        "body":event.body.as_deref().unwrap_or_default().chars().take(256).collect::<String>(),"preview":true}));
                }
                json!({"memory_id":id,"sources":sources,"next_cursor":next,"truncated":more})
            }
        };
        if value.get("kind").and_then(Value::as_str) != Some("secret_ref") {
            Self::decorate_secret(&self.db, space, id, &mut value).await?;
        }
        Ok(budget(value, maximum))
    }
}
