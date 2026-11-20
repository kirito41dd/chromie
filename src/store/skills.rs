use super::*;
use crate::{
    entity::{
        skill_revision_files as files, skill_revisions as revisions, skills,
        stored_objects as objects,
    },
    skill_objects::BucketStore,
    skill_package::{self, PublishSkill, RollbackSkill, SkillUploadFile},
};
use sea_orm::{IntoActiveModel, QueryOrder, sea_query::Expr};
use std::collections::{BTreeMap, HashMap};

fn bytes(id: Uuid) -> Vec<u8> {
    id.as_bytes().to_vec()
}

pub(super) fn object_key(space: SpaceId, id: Uuid) -> String {
    format!("{}/{}", space.as_uuid(), id)
}

fn conflict() -> Error {
    Error::conflict("revision_conflict")
}

fn receipt_value(row: &write_receipts::Model) -> Result<Value> {
    Ok(json!({"skill_id":uuid_from_bytes(&row.target_id)?,"revision":row.revision}))
}

fn check_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > 2000 {
        return Err(Error::invalid("请简述修改原因，最多 2000 字节"));
    }
    Ok(())
}

/// 上传阶段只发放当前接入的暂存句柄。发布后可共享同空间的不可变对象。
impl Store {
    pub(super) fn skill_bucket(&self) -> Result<&BucketStore> {
        if let Some(bucket) = self.skill_objects.get() {
            return Ok(bucket);
        }
        let bucket = BucketStore::from_env()?;
        Ok(self.skill_objects.get_or_init(|| bucket))
    }

    pub async fn skill_upload(&self, space: SpaceId, data: &[u8]) -> Result<Value> {
        if data.len() > skill_package::MAX_BYTES {
            return Err(Error::invalid("单文件超过包大小上限"));
        }
        let agent = self.actor_bytes().ok_or_else(Error::unauthorized)?;
        let bucket = self.skill_bucket()?;
        let id = Uuid::now_v7();
        let digest = skill_package::digest(data);
        let tx = self.begin(space).await?;
        objects::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(bytes(id)),
            agent_id: Set(agent),
            digest: Set(digest.clone()),
            size: Set(data.len() as i32),
            canonical_digest: Set(None),
            created_at: Set(now()),
            state: Set("uploading".into()),
        }
        .insert(&tx)
        .await?;
        tx.commit().await?;
        bucket.put(&object_key(space, id), data).await?;
        let ready = objects::Entity::update_many()
            .col_expr(objects::Column::State, Expr::value("staged"))
            .filter(objects::Column::SpaceId.eq(space.bytes()))
            .filter(objects::Column::Id.eq(bytes(id)))
            .filter(objects::Column::State.eq("uploading"))
            .exec(&self.db)
            .await?;
        if ready.rows_affected != 1 {
            bucket.delete(&object_key(space, id)).await?;
            return Err(Error::unavailable());
        }
        Ok(json!({"object_id":id,"digest":digest,"size":data.len()}))
    }

    async fn skill_head<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<skills::Model> {
        skills::Entity::find_by_id((space.bytes(), bytes(id)))
            .one(db)
            .await?
            .ok_or_else(Error::missing)
    }

    async fn skill_snapshot<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        head: &skills::Model,
    ) -> Result<Value> {
        let revision = head.current_revision;
        let rev = revisions::Entity::find_by_id((space.bytes(), head.id.clone(), revision))
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
        let entries = Self::skill_file_objects(db, space, head).await?.into_iter().map(|(file, object)| {
            json!({"path":file.path,"media_type":file.media_type,"digest":object.digest,"size":object.size})
        }).collect::<Vec<_>>();
        Ok(
            json!({"skill_id":uuid_from_bytes(&head.id)?,"revision":revision,"frontmatter":rev.frontmatter,"files":entries}),
        )
    }

    async fn skill_file_objects<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        head: &skills::Model,
    ) -> Result<Vec<(files::Model, objects::Model)>> {
        let rows = files::Entity::find()
            .filter(files::Column::SpaceId.eq(space.bytes()))
            .filter(files::Column::SkillId.eq(head.id.clone()))
            .filter(files::Column::Revision.eq(head.current_revision))
            .order_by_asc(files::Column::Path)
            .all(db)
            .await?;
        let info: HashMap<_, _> = objects::Entity::find()
            .filter(objects::Column::SpaceId.eq(space.bytes()))
            .filter(objects::Column::Id.is_in(rows.iter().map(|f| f.object_id.clone())))
            .all(db)
            .await?
            .into_iter()
            .map(|o| (o.id.clone(), o))
            .collect();
        rows.into_iter()
            .map(|file| {
                let object = info
                    .get(&file.object_id)
                    .cloned()
                    .ok_or_else(Error::unavailable)?;
                Ok((file, object))
            })
            .collect()
    }

    /// 一次固定请求修订和不可变对象。对象 I/O 在事务外进行，全部校验成功才交付 ZIP。
    pub(crate) async fn skill_archive(
        &self,
        space: SpaceId,
        id: Uuid,
        revision: i32,
    ) -> Result<(String, Vec<u8>)> {
        let tx = self.db.begin().await?;
        let head = Self::skill_head(&tx, space, id).await?;
        if revision != head.current_revision {
            return Err(conflict());
        }
        let files = Self::skill_file_objects(&tx, space, &head).await?;
        tx.commit().await?;
        skill_package::validate_name(&head.name)?;
        skill_package::validate_layout(
            files
                .iter()
                .map(|(f, o)| (f.path.as_str(), o.size as usize)),
        )?;
        let mut contents = Vec::with_capacity(files.len());
        for (file, object) in files {
            contents.push((file.path, self.verified_object(space, &object).await?));
        }
        tokio::task::spawn_blocking(move || {
            let archive = skill_package::zip_package(&head.name, contents)?;
            Ok((head.name, archive))
        })
        .await
        .map_err(|_| Error::unavailable())?
    }

    pub async fn skill_manifest(&self, space: SpaceId, id: Uuid) -> Result<Value> {
        let tx = self.db.begin().await?;
        let head = Self::skill_head(&tx, space, id).await?;
        let result = Self::skill_snapshot(&tx, space, &head).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// 游标取上一页最小修订号，计数保留，只有 retained 的修订仍可回退。
    pub async fn skill_history(
        &self,
        space: SpaceId,
        id: Uuid,
        before: Option<i32>,
    ) -> Result<Value> {
        Self::skill_head(&self.db, space, id).await?;
        let mut query = revisions::Entity::find()
            .filter(revisions::Column::SpaceId.eq(space.bytes()))
            .filter(revisions::Column::SkillId.eq(bytes(id)));
        if let Some(n) = before {
            query = query.filter(revisions::Column::Revision.lt(n));
        }
        let mut rows = query
            .order_by_desc(revisions::Column::Revision)
            .limit(21)
            .all(&self.db)
            .await?;
        let next = if rows.len() > 20 {
            rows.truncate(20);
            rows.last().map(|r| r.revision)
        } else {
            None
        };
        let entries: Vec<_> = rows
            .into_iter()
            .map(|r| {
                json!({"revision":r.revision,"base_revision":r.base_revision,
            "reason":r.reason,"retained":r.retained!=0,"usage_counts":r.usage_counts})
            })
            .collect();
        Ok(json!({"skill_id":id,"revisions":entries,"next_before":next}))
    }

    pub async fn skill_lookup(&self, space: SpaceId, name: &str) -> Result<Value> {
        skill_package::validate_name(name)?;
        let head = skills::Entity::find()
            .filter(skills::Column::SpaceId.eq(space.bytes()))
            .filter(skills::Column::Name.eq(name))
            .one(&self.db)
            .await?
            .ok_or_else(Error::missing)?;
        self.skill_manifest(space, uuid_from_bytes(&head.id)?).await
    }

    /// Web 列表只读取空间内的 Skill 概要，展开时才访问包清单和 RustFS。
    pub(crate) async fn skill_library(&self, space: SpaceId, after: Option<Uuid>) -> Result<Value> {
        let mut query = skills::Entity::find().filter(skills::Column::SpaceId.eq(space.bytes()));
        if let Some(after) = after {
            query = query.filter(skills::Column::Id.lt(bytes(after)));
        }
        let mut rows = query
            .order_by_desc(skills::Column::Id)
            .limit(21)
            .all(&self.db)
            .await?;
        let more = rows.len() > 20;
        rows.truncate(20);
        let mut items = Vec::with_capacity(rows.len());
        let mut cursor = None;
        for row in rows {
            let id = uuid_from_bytes(&row.id)?;
            cursor = Some(id);
            items.push(json!({"id":id,"name":row.name,"description":row.description,"revision":row.current_revision}));
        }
        Ok(json!({"items": items, "next": if more {cursor} else {None}}))
    }

    /// 两条 MCP 发现路径按同一空间、同一 ID 顺序翻页。多取一条仅用于判断下一页。
    async fn skill_catalog_page<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        after: Option<Uuid>,
    ) -> Result<(Vec<skills::Model>, Option<Uuid>)> {
        const PAGE_SIZE: usize = 10;
        let mut select = skills::Entity::find().filter(skills::Column::SpaceId.eq(space.bytes()));
        if let Some(id) = after {
            select = select.filter(skills::Column::Id.gt(bytes(id)));
        }
        let mut heads = select
            .order_by_asc(skills::Column::Id)
            .limit((PAGE_SIZE + 1) as u64)
            .all(db)
            .await?;
        let next = if heads.len() > PAGE_SIZE {
            heads.truncate(PAGE_SIZE);
            heads
                .last()
                .map(|head| uuid_from_bytes(&head.id))
                .transpose()?
        } else {
            None
        };
        Ok((heads, next))
    }

    pub(crate) async fn skill_manifests_page(
        &self,
        space: SpaceId,
        after: Option<Uuid>,
    ) -> Result<(Vec<Value>, Option<Uuid>)> {
        let tx = self.db.begin().await?;
        let (heads, next) = Self::skill_catalog_page(&tx, space, after).await?;
        let mut manifests = Vec::with_capacity(heads.len());
        for head in heads {
            manifests.push(Self::skill_snapshot(&tx, space, &head).await?);
        }
        tx.commit().await?;
        Ok((manifests, next))
    }

    /// 兼容宿主只需模型可见的目录。发布事务已将 YAML 中的名称和描述同步到当前版本。
    pub(crate) async fn skill_directory(
        &self,
        space: SpaceId,
        after: Option<Uuid>,
    ) -> Result<Value> {
        let (heads, next) = Self::skill_catalog_page(&self.db, space, after).await?;
        let mut entries = Vec::with_capacity(heads.len());
        for head in heads {
            entries.push(json!({
                "skill_id": uuid_from_bytes(&head.id)?,
                "name": head.name,
                "description": head.description,
                "revision": head.current_revision
            }));
        }
        Ok(json!({"skills": entries, "next_cursor": next}))
    }

    pub async fn skill_file(
        &self,
        space: SpaceId,
        id: Uuid,
        path: &str,
        revision: Option<i32>,
    ) -> Result<Vec<u8>> {
        skill_package::validate_path(path)?;
        let tx = self.db.begin().await?;
        let head = Self::skill_head(&tx, space, id).await?;
        if revision.is_some_and(|r| r != head.current_revision) {
            return Err(conflict());
        }
        let file = files::Entity::find_by_id((
            space.bytes(),
            bytes(id),
            head.current_revision,
            path.to_owned(),
        ))
        .one(&tx)
        .await?
        .filter(|f| f.path == path)
        .ok_or_else(Error::missing)?;
        let object = objects::Entity::find_by_id((space.bytes(), file.object_id))
            .one(&tx)
            .await?
            .ok_or_else(Error::unavailable)?;
        tx.commit().await?;
        self.verified_object(space, &object).await
    }

    async fn verified_object(&self, space: SpaceId, object: &objects::Model) -> Result<Vec<u8>> {
        let id = uuid_from_bytes(&object.id)?;
        let data = self
            .skill_bucket()?
            .get(
                &object_key(space, id),
                usize::try_from(object.size).map_err(|_| Error::unavailable())?,
            )
            .await?;
        if data.len() != object.size as usize || skill_package::digest(&data) != object.digest {
            return Err(Error::unavailable());
        }
        Ok(data)
    }

    async fn staged<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        file: &SkillUploadFile,
    ) -> Result<objects::Model> {
        let o = objects::Entity::find_by_id((space.bytes(), bytes(file.object_id)))
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
        if Some(o.agent_id.as_slice()) != self.actor_bytes().as_deref()
            || o.digest != file.digest
            || o.size as usize != file.size
            || (o.state == "staged" && o.created_at < now() - time::Duration::days(1))
            || !matches!(o.state.as_str(), "staged" | "live")
        {
            return Err(Error::invalid("上传句柄或文件摘要不匹配，请重新上传"));
        }
        Ok(o)
    }

    async fn skill_receipt_row<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<Option<write_receipts::Model>> {
        let receipt =
            write_receipts::Entity::find_by_id((space.bytes(), "skill_write".into(), bytes(id)))
                .one(db)
                .await?;
        Ok(receipt.filter(|r| r.agent_id == self.actor_bytes()))
    }

    pub async fn skill_receipt(&self, space: SpaceId, id: Uuid) -> Result<Value> {
        receipt_value(
            &self
                .skill_receipt_row(&self.db, space, id)
                .await?
                .ok_or_else(Error::missing)?,
        )
    }

    async fn skill_replay<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        id: Uuid,
        hash: &[u8],
    ) -> Result<Option<Value>> {
        let Some(r) = self.skill_receipt_row(db, space, id).await? else {
            return Ok(None);
        };
        if r.input_hash != hash {
            return Err(Error::conflict("idempotency_conflict"));
        }
        Ok(Some(receipt_value(&r)?))
    }

    pub async fn skill_publish(&self, space: SpaceId, mut input: PublishSkill) -> Result<Value> {
        check_reason(&input.reason)?;
        if input.skill_id.is_some() != input.expected_revision.is_some()
            || input.expected_revision.is_some_and(|n| n < 1)
        {
            return Err(Error::invalid("修订必须指定 Skill 和基础版本"));
        }
        input.files.sort_by(|a, b| a.path.cmp(&b.path));
        skill_package::validate_layout(input.files.iter().map(|f| (f.path.as_str(), f.size)))?;
        // 幂等内容按文件摘要识别。上传重试可能换了暂存对象 ID。
        let content: Vec<_> = input
            .files
            .iter()
            .map(|f| (&f.path, &f.digest, f.size))
            .collect();
        let hash = Self::hash(&(
            "publish",
            input.skill_id,
            input.expected_revision,
            &input.reason,
            content,
        ))?;
        if let Some(r) = self
            .skill_replay(&self.db, space, input.request_id, &hash)
            .await?
        {
            return Ok(r);
        }
        // 在事务外逐一验证候选字节，包括可能复用的已有对象。事务中再次核验句柄和可引用状态。
        let mut metadata = None;
        for file in &input.files {
            let staged = self.staged(&self.db, space, file).await?;
            let bytes = self.verified_object(space, &staged).await?;
            if file.path == "SKILL.md" {
                metadata = Some(skill_package::frontmatter(&bytes)?);
            }
            if let Some(existing) = objects::Entity::find()
                .filter(objects::Column::SpaceId.eq(space.bytes()))
                .filter(objects::Column::CanonicalDigest.eq(&staged.digest))
                .one(&self.db)
                .await?
                && existing.id != staged.id
            {
                self.verified_object(space, &existing).await?;
            }
        }
        let metadata = metadata.ok_or_else(Error::missing)?;
        let tx = self.begin(space).await?;
        if let Some(r) = self
            .skill_replay(&tx, space, input.request_id, &hash)
            .await?
        {
            tx.commit().await?;
            return Ok(r);
        }
        let id = input.skill_id.unwrap_or_else(Uuid::now_v7);
        let mut manifest = BTreeMap::new();
        for f in &input.files {
            let staged = self.staged(&tx, space, f).await?;
            let live = objects::Entity::find()
                .filter(objects::Column::SpaceId.eq(space.bytes()))
                .filter(objects::Column::CanonicalDigest.eq(&staged.digest))
                .one(&tx)
                .await?;
            let selected = if let Some(o) = live {
                o.id
            } else {
                let id = staged.id.clone();
                let digest = staged.digest.clone();
                let mut changes = staged.into_active_model();
                changes.canonical_digest = Set(Some(digest));
                changes.state = Set("live".into());
                changes.update(&tx).await?;
                id
            };
            manifest.insert(f.path.clone(), selected);
        }
        self.commit_skill(
            tx,
            space,
            Release {
                id,
                base: input.expected_revision,
                metadata,
                manifest,
                reason: input.reason,
                request: input.request_id,
                hash,
            },
        )
        .await
    }

    pub async fn skill_rollback(&self, space: SpaceId, input: RollbackSkill) -> Result<Value> {
        check_reason(&input.reason)?;
        let hash = Self::hash(&("rollback", &input))?;
        if let Some(receipt) = self
            .skill_replay(&self.db, space, input.request_id, &hash)
            .await?
        {
            return Ok(receipt);
        }
        // 回退也会发布一份可读取的完整包。先检查旧清单指向的对象，再进入写事务。
        let previous_files = files::Entity::find()
            .filter(files::Column::SpaceId.eq(space.bytes()))
            .filter(files::Column::SkillId.eq(bytes(input.skill_id)))
            .filter(files::Column::Revision.eq(input.target_revision))
            .all(&self.db)
            .await?;
        if previous_files.is_empty() {
            return Err(Error::missing());
        }
        for file in previous_files {
            let object = objects::Entity::find_by_id((space.bytes(), file.object_id))
                .one(&self.db)
                .await?
                .ok_or_else(Error::unavailable)?;
            self.verified_object(space, &object).await?;
        }
        let tx = self.begin(space).await?;
        if let Some(r) = self
            .skill_replay(&tx, space, input.request_id, &hash)
            .await?
        {
            tx.commit().await?;
            return Ok(r);
        }
        let old = revisions::Entity::find_by_id((
            space.bytes(),
            bytes(input.skill_id),
            input.target_revision,
        ))
        .one(&tx)
        .await?
        .filter(|r| r.retained != 0)
        .ok_or_else(Error::missing)?;
        let manifest = files::Entity::find()
            .filter(files::Column::SpaceId.eq(space.bytes()))
            .filter(files::Column::SkillId.eq(bytes(input.skill_id)))
            .filter(files::Column::Revision.eq(old.revision))
            .all(&tx)
            .await?
            .into_iter()
            .map(|f| (f.path, f.object_id))
            .collect();
        self.commit_skill(
            tx,
            space,
            Release {
                id: input.skill_id,
                base: Some(input.expected_revision),
                metadata: old.frontmatter,
                manifest,
                reason: input.reason,
                request: input.request_id,
                hash,
            },
        )
        .await
    }

    async fn commit_skill(
        &self,
        tx: DatabaseTransaction,
        space: SpaceId,
        release: Release,
    ) -> Result<Value> {
        let Release {
            id,
            base,
            metadata,
            manifest,
            reason,
            request,
            hash,
        } = release;
        let name = metadata["name"]
            .as_str()
            .ok_or_else(|| Error::invalid("缺少 Skill 名称"))?;
        let description = metadata["description"]
            .as_str()
            .ok_or_else(|| Error::invalid("缺少 Skill 说明"))?;
        let revision = base.unwrap_or(0).checked_add(1).ok_or_else(conflict)?;
        if let Some(expected) = base {
            let change = skills::Entity::update_many()
                .col_expr(skills::Column::CurrentRevision, Expr::value(revision))
                .col_expr(skills::Column::Description, Expr::value(description))
                .filter(skills::Column::SpaceId.eq(space.bytes()))
                .filter(skills::Column::Id.eq(bytes(id)))
                .filter(skills::Column::Name.eq(name))
                .filter(skills::Column::CurrentRevision.eq(expected))
                .exec(&tx)
                .await?;
            if change.rows_affected != 1 {
                return Err(conflict());
            }
        } else {
            skills::ActiveModel {
                space_id: Set(space.bytes()),
                id: Set(bytes(id)),
                name: Set(name.to_owned()),
                description: Set(description.to_owned()),
                current_revision: Set(1),
                usage_counts: Set(json!({})),
            }
            .insert(&tx)
            .await
            .map_err(|e| {
                if matches!(
                    e.sql_err(),
                    Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
                ) {
                    Error::conflict("skill_name_conflict")
                } else {
                    e.into()
                }
            })?;
        }
        revisions::ActiveModel {
            space_id: Set(space.bytes()),
            skill_id: Set(bytes(id)),
            revision: Set(revision),
            base_revision: Set(base),
            agent_id: Set(self.actor_bytes().ok_or_else(Error::unauthorized)?),
            frontmatter: Set(metadata),
            reason: Set(reason),
            retained: Set(1),
            usage_counts: Set(json!({})),
            created_at: Set(now()),
        }
        .insert(&tx)
        .await?;
        files::Entity::insert_many(manifest.into_iter().map(|(path, object_id)| {
            files::ActiveModel {
                space_id: Set(space.bytes()),
                skill_id: Set(bytes(id)),
                revision: Set(revision),
                media_type: Set(skill_package::media_type(&path).to_owned()),
                path: Set(path),
                object_id: Set(object_id),
            }
        }))
        .exec(&tx)
        .await?;
        self.receipt(
            &tx,
            space,
            "skill_write",
            request,
            hash,
            (id, Some(revision)),
        )
        .await?;
        tx.commit().await?;
        Ok(json!({"skill_id":id,"revision":revision}))
    }
}

struct Release {
    id: Uuid,
    base: Option<i32>,
    metadata: Value,
    manifest: BTreeMap<String, Vec<u8>>,
    reason: String,
    request: Uuid,
    hash: Vec<u8>,
}
