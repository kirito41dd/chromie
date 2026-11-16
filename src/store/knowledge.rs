//! 关系随源修订保存。所有关系写操作由调用方持有空间事务锁，读取不越过空间和遗忘边界。
use super::*;
use crate::{
    entity::memory_links,
    knowledge::{Link, MemoryType, Navigation, Neighbors, Relation},
};
use sea_orm::{Condition, ExprTrait, JoinType, QueryOrder, RelationTrait, sea_query::Expr};
use std::collections::{HashSet, VecDeque};

impl Store {
    pub(super) async fn links<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<Vec<Link>> {
        memory_links::Entity::find()
            .filter(memory_links::Column::SpaceId.eq(space.bytes()))
            .filter(memory_links::Column::MemoryId.eq(id.as_bytes().to_vec()))
            .filter(memory_links::Column::Revision.eq(revision))
            .order_by_asc(memory_links::Column::TargetId)
            .order_by_asc(memory_links::Column::Relation)
            .all(db)
            .await?
            .into_iter()
            .map(|row| {
                Ok(Link {
                    memory_id: uuid_from_bytes(&row.target_id)?,
                    relation: Relation::parse(&row.relation)?,
                    revision: row.target_revision,
                })
            })
            .collect()
    }

    pub(super) async fn check_links<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        source: Uuid,
        links: &[Link],
        preserve: bool,
    ) -> Result<()> {
        if links.len() > 32 {
            return Err(Error::invalid("每条记忆最多关联 32 条关系"));
        }
        let mut unique = HashSet::new();
        for link in links {
            link.validate()?;
            if link.memory_id == source || !unique.insert((link.memory_id, link.relation)) {
                return Err(Error::invalid("关系不能指向自身或重复同一目标与类型"));
            }
            let target = Self::head(db, space, link.memory_id).await?;
            if target.forgotten_at.is_some() {
                return Err(Error::missing());
            }
            if link.relation == Relation::DerivedFrom {
                // 公开 Secret 标签可以导航引用，不进入派生摘要的依据。
                if crate::entity::secrets::Entity::find_by_id((space.bytes(), target.id.clone()))
                    .one(db)
                    .await?
                    .is_some()
                {
                    return Err(Error::invalid(
                        "Secret 只能建立普通引用，不能作为后台摘要的派生依据",
                    ));
                }
                if !preserve && link.revision != Some(target.current_revision) {
                    return Err(Error::conflict("dependency_revision_conflict"));
                }
                if memory_revisions::Entity::find_by_id((
                    space.bytes(),
                    target.id,
                    link.revision.ok_or_else(Error::unavailable)?,
                ))
                .one(db)
                .await?
                .is_none()
                {
                    return Err(Error::missing());
                }
            }
        }
        // 修订旧内容同样不能引入派生环；普通导航引用允许互链。
        let mut queue: VecDeque<Uuid> = links
            .iter()
            .filter(|v| v.relation == Relation::DerivedFrom)
            .map(|v| v.memory_id)
            .collect();
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if id == source {
                return Err(Error::invalid("派生依据不能形成循环"));
            }
            if !seen.insert(id) {
                continue;
            }
            if seen.len() > 512 {
                return Err(Error::invalid("派生链过大，请拆分整理范围"));
            }
            let head = Self::head(db, space, id).await?;
            for link in Self::links(db, space, id, head.current_revision).await? {
                if link.relation == Relation::DerivedFrom {
                    queue.push_back(link.memory_id);
                }
            }
        }
        Ok(())
    }

    pub(super) async fn add_links<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
        links: &[Link],
    ) -> Result<()> {
        for link in links {
            memory_links::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                revision: Set(revision),
                target_id: Set(link.memory_id.as_bytes().to_vec()),
                relation: Set(link.relation.as_str().into()),
                target_revision: Set(link.revision),
            }
            .insert(db)
            .await?;
        }
        Ok(())
    }

    /// 派生正文也可能保留被遗忘的信息，沿所有修订的依据关系清理，普通引用不扩大遗忘范围。
    pub(super) async fn include_derived_forget<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        affected: &mut HashSet<Uuid>,
    ) -> Result<()> {
        let mut queue: VecDeque<Uuid> = affected.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            let rows = memory_links::Entity::find()
                .filter(memory_links::Column::SpaceId.eq(space.bytes()))
                .filter(memory_links::Column::TargetId.eq(id.as_bytes().to_vec()))
                .filter(memory_links::Column::Relation.eq(Relation::DerivedFrom.as_str()))
                .all(db)
                .await?;
            for row in rows {
                let dependent = uuid_from_bytes(&row.memory_id)?;
                if affected.insert(dependent) {
                    queue.push_back(dependent);
                }
            }
        }
        Ok(())
    }

    pub(super) async fn knowledge_state<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<Value> {
        let links = Self::links(db, space, id, revision).await?;
        let count = links.len();
        let mut queue: VecDeque<Link> = links
            .into_iter()
            .filter(|v| v.relation == Relation::DerivedFrom)
            .collect();
        let mut seen = HashSet::new();
        let mut changed = false;
        while let Some(link) = queue.pop_front() {
            if !seen.insert((link.memory_id, link.revision)) {
                continue;
            }
            if seen.len() > 512 {
                changed = true;
                break;
            }
            let target = Self::head(db, space, link.memory_id).await?;
            if target.forgotten_at.is_some() || link.revision != Some(target.current_revision) {
                changed = true;
                break;
            }
            queue.extend(
                Self::links(db, space, link.memory_id, target.current_revision)
                    .await?
                    .into_iter()
                    .filter(|v| v.relation == Relation::DerivedFrom),
            );
        }
        Ok(json!({"outgoing_count":count,"needs_review":changed}))
    }

    pub async fn navigation(&self, space: SpaceId, input: Navigation) -> Result<Value> {
        let maximum = crate::types::output_budget(input.max_chars)?;
        if let Some(project) = &input.project_key {
            crate::types::bounded(project, 128)?;
        }
        let mut query = memory_revisions::Entity::find()
            .join(
                JoinType::InnerJoin,
                memory_revisions::Relation::Memories.def(),
            )
            .filter(memory_revisions::Column::SpaceId.eq(space.bytes()))
            .filter(memory_revisions::Column::MemoryType.eq(MemoryType::Topic.as_str()))
            .filter(memories::Column::ForgottenAt.is_null())
            .filter(
                Expr::col((memory_revisions::Entity, memory_revisions::Column::Revision))
                    .equals((memories::Entity, memories::Column::CurrentRevision)),
            );
        if let Some(project) = input.project_key {
            query = query.filter(
                Condition::any()
                    .add(memory_revisions::Column::ProjectKey.is_null())
                    .add(memory_revisions::Column::ProjectKey.eq(project)),
            );
        }
        if let Some(cursor) = input.cursor {
            query = query.filter(memory_revisions::Column::MemoryId.gt(cursor.as_bytes().to_vec()));
        }
        let rows = query
            .order_by_asc(memory_revisions::Column::MemoryId)
            .limit(21)
            .all(&self.db)
            .await?;
        let more = rows.len() > 20;
        let mut items = Vec::new();
        for row in rows.into_iter().take(20) {
            let id = uuid_from_bytes(&row.memory_id)?;
            items.push(json!({"memory_id":id,"revision":row.revision,"memory_type":row.memory_type,
                "title":crate::knowledge::title(row.content.as_deref().unwrap_or_default()),"project_key":row.project_key,
                "knowledge":Self::knowledge_state(&self.db, space, id, row.revision).await?}));
        }
        fit_page(json!({"kind":"navigation"}), items, more, maximum)
    }

    pub async fn memory_neighbors(&self, space: SpaceId, input: Neighbors) -> Result<Value> {
        let maximum = crate::types::output_budget(input.max_chars)?;
        if let Some(project) = &input.project_key {
            crate::types::bounded(project, 128)?;
        }
        let head = Self::head(&self.db, space, input.memory_id).await?;
        if head.forgotten_at.is_some() {
            return Err(Error::missing());
        }
        // 从源修订连接到当前记忆头，历史修订的旧引用不冒充当前反向引用。
        let mut query = memory_links::Entity::find()
            .join(
                JoinType::InnerJoin,
                memory_links::Relation::MemoryRevisions2.def(),
            )
            .join(
                JoinType::InnerJoin,
                memory_revisions::Relation::Memories.def(),
            )
            .filter(memory_links::Column::SpaceId.eq(space.bytes()))
            .filter(memories::Column::ForgottenAt.is_null())
            .filter(
                Expr::col((memory_links::Entity, memory_links::Column::Revision))
                    .equals((memories::Entity, memories::Column::CurrentRevision)),
            );
        let peer = if input.incoming {
            memory_links::Column::MemoryId
        } else {
            memory_links::Column::TargetId
        };
        query = if input.incoming {
            query.filter(memory_links::Column::TargetId.eq(input.memory_id.as_bytes().to_vec()))
        } else {
            query.filter(memory_links::Column::MemoryId.eq(head.id))
        };
        if let Some(cursor) = input.cursor {
            query = query.filter(peer.gt(cursor.as_bytes().to_vec()));
        }
        let ids = query
            .clone()
            .select_only()
            .column(peer)
            .distinct()
            .order_by_asc(peer)
            .limit(21)
            .into_tuple::<Vec<u8>>()
            .all(&self.db)
            .await?;
        let more = ids.len() > 20;
        let mut items = Vec::new();
        let mut last_scanned = None;
        for bytes in ids.into_iter().take(20) {
            let id = uuid_from_bytes(&bytes)?;
            last_scanned = Some(id);
            let target = Self::head(&self.db, space, id).await?;
            if target.forgotten_at.is_some() {
                continue;
            }
            let revision = Self::revision(&self.db, &target).await?;
            if let Some(project) = &input.project_key
                && revision
                    .project_key
                    .as_ref()
                    .is_some_and(|key| key != project)
            {
                continue;
            }
            let edges = query.clone().filter(peer.eq(bytes)).all(&self.db).await?;
            let relations: Vec<Value> = edges.into_iter().map(|edge| json!({"relation":edge.relation,"revision":edge.target_revision,
                "needs_review": edge.target_revision.is_some_and(|v| v != if input.incoming { head.current_revision } else { target.current_revision })})).collect();
            items.push(json!({"memory_id":id,"revision":revision.revision,"memory_type":revision.memory_type,
                "title":crate::knowledge::title(revision.content.as_deref().unwrap_or_default()),"project_key":revision.project_key,
                "relations":relations,"match_source":"linked"}));
        }
        let mut result = fit_page(
            json!({"kind":"memory_links","memory_id":input.memory_id,"incoming":input.incoming}),
            items,
            more,
            maximum,
        )?;
        // 范围过滤可能整页没有可见项，仍用扫描游标向前推进。
        if result["memories"].as_array().is_some_and(Vec::is_empty) && more {
            result["next_cursor"] = json!(last_scanned);
        }
        Ok(result)
    }
}

fn fit_page(
    mut value: Value,
    mut items: Vec<Value>,
    mut more: bool,
    maximum: usize,
) -> Result<Value> {
    loop {
        value["next_cursor"] = if more {
            items
                .last()
                .map(|item| item["memory_id"].clone())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        value["truncated"] = json!(more);
        value["memories"] = json!(items);
        if value.to_string().chars().count() <= maximum {
            return Ok(value);
        }
        if items.len() <= 1 {
            return Err(Error::invalid("关系元数据超出预算，请增大 max_chars"));
        }
        items.pop();
        more = true;
    }
}
