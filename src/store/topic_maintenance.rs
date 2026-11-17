//! 合并主题只收敛入口，不遗忘事实；旧主题以别名修订保留，不参与普通列表和召回。
use super::*;
use crate::knowledge::{Link, MemoryType, Relation};
use sea_orm::{
    ExprTrait,
    sea_query::{Expr, Query, SimpleExpr},
};
use std::collections::HashSet;

impl Store {
    /// 校验模型原始依据必须先于可信旧分支并集，不能用旧关系“修好”模型的错误修订。
    pub(super) fn validate_model_topic_links(
        snapshot: &super::knowledge_jobs::Snapshot,
        draft: &crate::text_model::Draft,
    ) -> Result<()> {
        for link in &draft.links {
            if let Some(target) = snapshot.memories.iter().find(|m| m.id == link.memory_id) {
                if link.relation == Relation::DerivedFrom && link.revision != Some(target.revision)
                {
                    return Err(crate::text_model::error(
                        "model_evidence_invalid",
                        "模型原始依据与已读修订不匹配",
                    ));
                }
            } else if !snapshot.memories.iter().any(|m| {
                (Some(m.id) == draft.memory_id || draft.merge_from.contains(&m.id))
                    && m.links.contains(link)
            }) {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "未读关系只能保留原主题的完全相同入口",
                ));
            }
        }
        Ok(())
    }

    /// 容量只用于当前模型输入，不改变持久快照或推测可删除的历史分支。
    pub(super) fn topic_model_memories(
        memories: &[super::knowledge_jobs::MemorySnapshot],
    ) -> Result<Vec<Value>> {
        memories.iter().map(|memory| {
            let mut value = serde_json::to_value(memory).map_err(|_| Error::unavailable())?;
            if memory.memory_type == MemoryType::Topic {
                value["link_capacity"] = json!({"used":memory.links.len(),"remaining":crate::knowledge::MAX_LINKS.saturating_sub(memory.links.len()),"limit":crate::knowledge::MAX_LINKS});
            }
            Ok(value)
        }).collect()
    }

    /// 只折叠完全相同的依据；同对象的冲突修订不能靠取最大值隐藏。
    pub(super) fn normalize_topic_links(links: &mut Vec<Link>) -> Result<()> {
        let mut revisions = std::collections::HashMap::new();
        let mut normalized = Vec::new();
        for link in links.iter() {
            link.validate().map_err(|_| {
                crate::text_model::error("model_evidence_invalid", "主题关系格式无效")
            })?;
            if let Some(previous) = revisions.insert((link.memory_id, link.relation), link.revision)
            {
                if previous != link.revision {
                    return Err(crate::text_model::error(
                        "model_evidence_invalid",
                        "同一主题关系给出了冲突修订",
                    ));
                }
            } else {
                normalized.push(link.clone());
            }
        }
        if normalized.len() > crate::knowledge::MAX_LINKS {
            return Err(crate::text_model::error(
                "model_evidence_invalid",
                "主题关系并集超过32条，须保留分层入口，不能丢弃旧引用",
            ));
        }
        *links = normalized;
        Ok(())
    }
    /// 保留入口还被别名使用时，不能把它改成普通记忆或迁到另一适用范围。
    pub(super) async fn check_alias_dependents<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        kind: MemoryType,
        project: Option<&str>,
    ) -> Result<()> {
        let incoming = crate::entity::memory_links::Entity::find()
            .filter(crate::entity::memory_links::Column::SpaceId.eq(space.bytes()))
            .filter(crate::entity::memory_links::Column::TargetId.eq(id.as_bytes().to_vec()))
            .filter(crate::entity::memory_links::Column::Relation.eq("reference"))
            .all(db)
            .await?;
        for link in incoming {
            let head = Self::head(db, space, uuid_from_bytes(&link.memory_id)?).await?;
            if head.forgotten_at.is_some() || head.current_revision != link.revision {
                continue;
            }
            let revision = Self::revision(db, &head).await?;
            if revision.memory_type == "topic_alias"
                && (kind == MemoryType::Note || revision.project_key.as_deref() != project)
            {
                return Err(Error::invalid(
                    "此主题仍有合并前的入口，不能改为普通记忆或改变适用范围",
                ));
            }
        }
        Ok(())
    }
    /// 合并只折叠已读主题，引用取并集；不让模型省略某分支而把它静默移除。
    pub(super) fn prepare_topic_merge(
        snapshot: &super::knowledge_jobs::Snapshot,
        draft: &mut crate::text_model::Draft,
        edited: &mut HashSet<Uuid>,
    ) -> Result<Vec<(Uuid, i32, String)>> {
        if draft.merge_from.len() > 8 || (!draft.merge_from.is_empty() && draft.memory_id.is_none())
        {
            return Err(crate::text_model::error(
                "model_evidence_invalid",
                "主题合并须指定已读保留主题，每次最多合并8个入口",
            ));
        }
        let mut merged = Vec::new();
        for id in &draft.merge_from {
            let old = snapshot
                .memories
                .iter()
                .find(|m| m.id == *id)
                .ok_or_else(|| {
                    crate::text_model::error("model_evidence_invalid", "合并目标不在已读主题范围")
                })?;
            let keeper = snapshot
                .memories
                .iter()
                .find(|m| Some(m.id) == draft.memory_id)
                .ok_or_else(|| {
                    crate::text_model::error("model_evidence_invalid", "保留主题不在已读范围")
                })?;
            if old.memory_type != MemoryType::Topic
                || keeper.memory_type != MemoryType::Topic
                || old.project_key != keeper.project_key
                || Some(*id) == draft.memory_id
                || !edited.insert(*id)
            {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "主题合并范围、类型或重复目标无效",
                ));
            }
            merged.push((*id, old.revision, crate::knowledge::title(&old.content)));
        }
        if !merged.is_empty() {
            // 同一事实的不同派生修订只保留更近版本，随后仍由 check_links 核对它是否为当前修订。
            let sources = snapshot
                .memories
                .iter()
                .filter(|m| Some(m.id) == draft.memory_id || draft.merge_from.contains(&m.id));
            for link in sources.flat_map(|m| &m.links) {
                if let Some(existing) = draft
                    .links
                    .iter_mut()
                    .find(|l| l.memory_id == link.memory_id && l.relation == link.relation)
                {
                    existing.revision = std::cmp::max(existing.revision, link.revision);
                } else {
                    draft.links.push(link.clone());
                }
            }
            draft.links.retain(|l| {
                Some(l.memory_id) != draft.memory_id && !draft.merge_from.contains(&l.memory_id)
            });
            if draft.links.len() > crate::knowledge::MAX_LINKS {
                return Err(crate::text_model::error(
                    "model_evidence_invalid",
                    "合并后超过32条关系，须保留分层主题，不能丢弃入口",
                ));
            }
        }
        Ok(merged)
    }
    /// 与 memories 头表相关联，可复用于只读列表、数量统计，不扫描或解释正文标记。
    pub(crate) fn visible_memory_head() -> SimpleExpr {
        Expr::exists(
            Query::select()
                .expr(Expr::val(1))
                .from(memory_revisions::Entity)
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::SpaceId))
                        .equals((memories::Entity, memories::Column::SpaceId)),
                )
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::MemoryId))
                        .equals((memories::Entity, memories::Column::Id)),
                )
                .and_where(
                    Expr::col((memory_revisions::Entity, memory_revisions::Column::Revision))
                        .equals((memories::Entity, memories::Column::CurrentRevision)),
                )
                .and_where(
                    Expr::col((
                        memory_revisions::Entity,
                        memory_revisions::Column::MemoryType,
                    ))
                    .eq(MemoryType::TopicAlias.as_str()),
                )
                .to_owned(),
        )
        .not()
    }

    pub(super) async fn check_topic_alias<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        kind: MemoryType,
        project: Option<&str>,
        links: &[Link],
    ) -> Result<()> {
        if kind != MemoryType::TopicAlias {
            return Ok(());
        }
        if links.len() != 1 || links[0].relation != Relation::Reference {
            return Err(Error::invalid("主题别名须且只能引用一个主题入口"));
        }
        let mut target = links[0].memory_id;
        let mut visited = HashSet::from([id]);
        loop {
            if !visited.insert(target) || visited.len() > 64 {
                return Err(Error::invalid("主题别名不能形成循环或过长链路"));
            }
            let head = Self::head(db, space, target).await?;
            if head.forgotten_at.is_some() {
                return Err(Error::missing());
            }
            let revision = Self::revision(db, &head).await?;
            if revision.project_key.as_deref() != project {
                return Err(Error::invalid("主题别名不能跨适用范围"));
            }
            match MemoryType::parse(&revision.memory_type)? {
                MemoryType::Topic => return Ok(()),
                MemoryType::TopicAlias => {
                    let links = Self::links(db, space, target, head.current_revision).await?;
                    if links.len() != 1 || links[0].relation != Relation::Reference {
                        return Err(Error::unavailable());
                    }
                    target = links[0].memory_id;
                }
                MemoryType::Note => return Err(Error::invalid("主题别名只能指向主题")),
            }
        }
    }

    pub(super) async fn alias_redirect<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<Option<Uuid>> {
        let mut links = Self::links(db, space, id, revision).await?;
        let mut visited = HashSet::from([id]);
        loop {
            let Some(link) = links.first() else {
                return Ok(None);
            };
            let target = link.memory_id;
            if links.len() != 1
                || link.relation != Relation::Reference
                || !visited.insert(target)
                || visited.len() > 64
            {
                return Ok(None);
            }
            let head = Self::head(db, space, target).await?;
            if head.forgotten_at.is_some() {
                return Ok(None);
            }
            match Self::revision(db, &head).await?.memory_type.as_str() {
                "topic" => return Ok(Some(target)),
                "topic_alias" => {
                    links = Self::links(db, space, target, head.current_revision).await?
                }
                _ => return Ok(None),
            }
        }
    }
}
