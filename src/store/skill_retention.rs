use super::skills::object_key;
use super::*;
use crate::{
    entity::{
        skill_revision_files as files, skill_revisions as revisions, skills,
        stored_objects as objects,
    },
    skill_objects::BucketStore,
};
use sea_orm::{IntoActiveModel, QueryOrder, sea_query::Query};

impl Store {
    /// 桶内物理键按空间和不可复用的对象 ID 生成。只回收完整匹配该格式且宽限期已过的孤儿。
    async fn sweep_skill_bucket(&self, bucket: &BucketStore, space: SpaceId) -> Result<()> {
        let prefix = format!("{}/", space.as_uuid());
        let mut token = None;
        let cutoff = now() - time::Duration::days(1);
        loop {
            let (entries, next) = bucket.list(&prefix, token.as_deref()).await?;
            for (key, modified) in entries {
                if modified >= cutoff {
                    continue;
                }
                let Some(id) = key
                    .strip_prefix(&prefix)
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    continue;
                };
                if key != object_key(space, id) {
                    continue;
                }
                // 上传者先提交登记，再发 PUT。旧对象的物理 ID 不会分配给下一次上传。
                if objects::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                    .one(&self.db)
                    .await?
                    .is_none()
                {
                    bucket.delete(&key).await?;
                }
            }
            if next.is_none() {
                break;
            }
            if next == token {
                return Err(Error::unavailable());
            }
            token = next;
        }
        Ok(())
    }

    /// 删除只针对每次上传独有的物理键。先在空间事务中去引用，提交后才访问 RustFS。
    pub async fn compact_skills(&self) -> Result<()> {
        let bucket = self.skill_bucket()?;
        for s in spaces::Entity::find().all(&self.db).await? {
            let space = SpaceId(uuid_from_bytes(&s.id)?);
            self.sweep_skill_bucket(bucket, space).await?;
            let tx = self.begin(space).await?;
            self.trim_skill_reports(&tx, space).await?;
            let cutoff = now() - time::Duration::days(1);
            for head in skills::Entity::find()
                .filter(skills::Column::SpaceId.eq(space.bytes()))
                .filter(skills::Column::CurrentRevision.gt(5))
                .all(&tx)
                .await?
            {
                // 替换时间由下一修订的发布时间界定，避免清理刚被替换的旧长寿版本。
                let old = revisions::Entity::find()
                    .filter(revisions::Column::SpaceId.eq(space.bytes()))
                    .filter(revisions::Column::SkillId.eq(head.id.clone()))
                    .filter(revisions::Column::Retained.eq(1))
                    .filter(revisions::Column::Revision.lte(head.current_revision - 5))
                    .order_by_asc(revisions::Column::Revision)
                    .limit(50)
                    .all(&tx)
                    .await?;
                for row in old {
                    let next = revisions::Entity::find_by_id((
                        space.bytes(),
                        head.id.clone(),
                        row.revision + 1,
                    ))
                    .one(&tx)
                    .await?;
                    if next.is_none_or(|r| r.created_at > cutoff) {
                        break;
                    }
                    files::Entity::delete_many()
                        .filter(files::Column::SpaceId.eq(space.bytes()))
                        .filter(files::Column::SkillId.eq(head.id.clone()))
                        .filter(files::Column::Revision.eq(row.revision))
                        .exec(&tx)
                        .await?;
                    let mut update = row.into_active_model();
                    update.retained = Set(0);
                    update.update(&tx).await?;
                }
            }
            let references = Query::select()
                .column(files::Column::ObjectId)
                .from(files::Entity)
                .and_where(files::Column::SpaceId.eq(space.bytes()))
                .to_owned();
            let pending = objects::Entity::find()
                .filter(objects::Column::SpaceId.eq(space.bytes()))
                .filter(objects::Column::Id.not_in_subquery(references))
                .filter(
                    sea_orm::Condition::any()
                        .add(objects::Column::State.is_in(["deleting", "live"]))
                        .add(objects::Column::CreatedAt.lt(cutoff)),
                )
                .limit(100)
                .all(&tx)
                .await?;
            let mut to_delete = Vec::new();
            for row in pending {
                let id = uuid_from_bytes(&row.id)?;
                let mut changes = row.into_active_model();
                changes.state = Set("deleting".into());
                changes.canonical_digest = Set(None);
                changes.update(&tx).await?;
                to_delete.push(id);
            }
            tx.commit().await?;
            for id in to_delete {
                bucket.delete(&object_key(space, id)).await?;
                objects::Entity::delete_many()
                    .filter(objects::Column::SpaceId.eq(space.bytes()))
                    .filter(objects::Column::Id.eq(id.as_bytes().to_vec()))
                    .filter(objects::Column::State.eq("deleting"))
                    .exec(&self.db)
                    .await?;
            }
        }
        Ok(())
    }
}
