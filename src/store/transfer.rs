//! 炼化读取一致快照；夺舍预览与提交共用同一分类规则。
use super::*;
use crate::{
    entity::{memory_imports, secrets, transfer_receipts},
    secret_contract::SecretContract,
    secret_crypto::EncryptedPayload,
    transfer::{self, Bundle, Record, Secret, SecretBytes},
};
use sea_orm::{AccessMode, IsolationLevel};
use std::collections::{HashMap, HashSet};

pub(super) struct Classified {
    pub record: Record,
    pub status: &'static str,
    pub reason: &'static str,
    pub dependencies: Vec<Uuid>,
}
impl Classified {
    // 显式列出公开字段，不能把持有 Secret 原文字节的 Record 整体序列化。
    fn public_view(&self) -> Value {
        json!({"memory_id":self.record.memory_id,"kind":if self.record.secret.is_some(){"secret"}else{"memory"},
            "excerpt":self.record.content.chars().take(240).collect::<String>(),"project_key":self.record.project_key,
            "status":self.status,"reason":self.reason,"contract":self.record.secret.as_ref().map(|s|&s.contract)})
    }
}
pub(super) struct Preview {
    pub items: Vec<Classified>,
    pub public: Value,
    pub hash: Vec<u8>,
    pub receipt: Option<Value>,
}
impl Store {
    // 绑定验密时的真实版本；改密与提交共用空间锁，旧授权不能跨过改密提交。
    pub(super) async fn transfer_space<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        version: &str,
    ) -> Result<spaces::Model> {
        let row = spaces::Entity::find_by_id(space.bytes())
            .one(db)
            .await?
            .ok_or_else(Error::unauthorized)?;
        if crate::auth::password_version(&row.password_hash) != version {
            return Err(Error::unauthorized());
        }
        Ok(row)
    }
    pub(crate) async fn export_bundle(&self, space: SpaceId, version: &str) -> Result<Bundle> {
        let tx = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        let owner = Self::transfer_space(&tx, space, version).await?;
        let heads = memories::Entity::find()
            .filter(memories::Column::SpaceId.eq(space.bytes()))
            .filter(memories::Column::ForgottenAt.is_null())
            .limit((transfer::MAX_RECORDS + 1) as u64)
            .all(&tx)
            .await?;
        if heads.len() > transfer::MAX_RECORDS {
            return Err(Error::invalid("空间超过首版炼化的 10,000 条上限"));
        }
        let mut records = Vec::new();
        let mut size = 0usize;
        for head in heads {
            let id = uuid_from_bytes(&head.id)?;
            let revision = Self::revision(&tx, &head).await?;
            let content = revision.content.ok_or_else(Error::unavailable)?;
            let secret = secrets::Entity::find_by_id((space.bytes(), head.id))
                .one(&tx)
                .await?;
            let secret = secret
                .map(|row| self.open_transfer_secret(space, id, &row))
                .transpose()?;
            size = size
                .saturating_add(content.len())
                .saturating_add(secret.as_ref().map_or(0, |s| s.value.0.len()));
            if size > transfer::MAX_PLAINTEXT {
                return Err(Error::invalid("空间内容超过首版炼化上限"));
            }
            records.push(Record {
                memory_id: id,
                source_revision: head.current_revision,
                content,
                project_key: revision.project_key,
                secret,
            });
        }
        tx.commit().await?;
        Ok(Bundle {
            format: "chromie-soul".into(),
            version: 1,
            package_id: Uuid::now_v7(),
            source_space: owner.slug,
            source_name: owner.name,
            exported_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| Error::unavailable())?,
            records,
        })
    }
    fn open_transfer_secret(
        &self,
        space: SpaceId,
        id: Uuid,
        row: &secrets::Model,
    ) -> Result<Secret> {
        let value = self.cipher()?.open(
            space,
            id,
            EncryptedPayload {
                revision: row.payload_revision,
                format: row.format_version,
                key_id: &row.key_id,
                nonce: &row.nonce,
                ciphertext: row.ciphertext.as_deref().ok_or_else(Error::unavailable)?,
            },
        )?;
        Ok(Secret {
            contract: SecretContract::from_storage(row.contract.as_ref())?,
            value: SecretBytes(value),
        })
    }
    pub(crate) async fn preview_transfer(
        &self,
        space: SpaceId,
        version: &str,
        bundle: Bundle,
        page: transfer::PreviewPage,
        expected_digest: Option<&str>,
    ) -> Result<Value> {
        let tx = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        let owner = Self::transfer_space(&tx, space, version).await?;
        let mut result = self
            .classify_transfer(&tx, space, &owner.slug, version, bundle)
            .await?;
        if result.receipt.is_none() {
            if let Some(expected) = expected_digest {
                if result.public["preview_digest"].as_str() != Some(expected) {
                    return Err(Error::conflict("transfer_preview_changed"));
                }
            } else if page.index() != 0 {
                return Err(Error::invalid("翻页前请先预览迁移包"));
            }
            let range = page.range(result.items.len())?;
            let has_more = range.end < result.items.len();
            result.public["items"] = json!(
                result.items[range]
                    .iter()
                    .map(Classified::public_view)
                    .collect::<Vec<_>>()
            );
            result.public["page"] = json!(page.index());
            result.public["page_size"] = json!(transfer::PREVIEW_PAGE_SIZE);
            result.public["has_more"] = json!(has_more);
            result.public["truncated"] = json!(result.items.len() > transfer::PREVIEW_PAGE_SIZE);
        }
        tx.commit().await?;
        Ok(result.public)
    }
    pub(super) async fn classify_transfer<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        target: &str,
        version: &str,
        bundle: Bundle,
    ) -> Result<Preview> {
        bundle.validate()?;
        // 回执与公开预览都使用根密钥派生的 MAC，不暴露弱密码的离线猜测摘要。
        let hash = self
            .cipher()?
            .fingerprint(space, b"soul-package-v1", &bundle.digest()?)?;
        if let Some(row) = transfer_receipts::Entity::find_by_id((
            space.bytes(),
            bundle.package_id.as_bytes().to_vec(),
        ))
        .one(db)
        .await?
        {
            if row.content_hash != hash {
                return Err(Error::conflict("transfer_package_conflict"));
            }
            let mut public = row.result;
            public["replayed"] = json!(true);
            return Ok(Preview {
                items: Vec::new(),
                public: public.clone(),
                hash,
                receipt: Some(public),
            });
        }
        let secret_ids: HashSet<_> = bundle
            .records
            .iter()
            .filter(|r| r.secret.is_some())
            .map(|r| r.memory_id)
            .collect();
        let mut items = Vec::new();
        let mut states = Vec::new();
        for mut record in bundle.records {
            let dependencies = transfer::references(&record.content, &bundle.source_space);
            record.content = transfer::relocate(&record.content, &bundle.source_space, target);
            let head =
                memories::Entity::find_by_id((space.bytes(), record.memory_id.as_bytes().to_vec()))
                    .one(db)
                    .await?;
            states.push(json!([
                record.memory_id,
                head.as_ref().map(|h| h.current_revision),
                head.as_ref()
                    .and_then(|h| h.forgotten_at)
                    .map(|t| t.to_string())
            ]));
            let mut status = "new";
            let mut reason = "新记忆";
            if crate::types::bounded(
                &record.content,
                if record.secret.is_some() { 256 } else { 16000 },
            )
            .is_err()
            {
                status = "conflict";
                reason = "迁移引用后正文超过长度上限";
            } else if let Some(head) = head {
                status = "conflict";
                reason = "目标空间已有不同内容或已遗忘";
                if head.forgotten_at.is_none() {
                    let revision = Self::revision(db, &head).await?;
                    let stored = secrets::Entity::find_by_id((space.bytes(), head.id))
                        .one(db)
                        .await?;
                    let same_secret = match (&record.secret, stored) {
                        (None, None) => true,
                        (Some(a), Some(b)) if b.ciphertext.is_some() => {
                            let b = self.open_transfer_secret(space, record.memory_id, &b)?;
                            a.contract == b.contract && a.value.0.as_slice() == b.value.0.as_slice()
                        }
                        _ => false,
                    };
                    if same_secret
                        && revision.content.as_ref() == Some(&record.content)
                        && revision.project_key == record.project_key
                    {
                        status = "existing";
                        reason = "相同记忆已存在";
                    }
                }
            }
            items.push(Classified {
                record,
                status,
                reason,
                dependencies,
            });
        }
        // 任一 Secret 依赖冲突向引用它的记忆传播，避免引用目标空间的另一份凭据。
        loop {
            let unavailable: HashSet<_> = items
                .iter()
                .filter(|i| i.status == "conflict")
                .map(|i| i.record.memory_id)
                .collect();
            let mut changed = false;
            for item in &mut items {
                if item.status == "conflict" {
                    continue;
                }
                if item
                    .dependencies
                    .iter()
                    .any(|id| !secret_ids.contains(id) || unavailable.contains(id))
                {
                    item.status = "conflict";
                    item.reason = "引用的 Secret 缺失或存在冲突";
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut counts = HashMap::new();
        for item in &items {
            *counts.entry(item.status).or_insert(0usize) += 1;
        }
        let decision: Vec<_> = items
            .iter()
            .map(|i| (i.record.memory_id, i.status))
            .collect();
        let binding =
            serde_json::to_vec(&(version, &states, &decision)).map_err(|_| Error::unavailable())?;
        let digest = self
            .cipher()?
            .fingerprint(space, &binding, &hash)?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let rows: Vec<_> = items
            .iter()
            .take(100)
            .map(Classified::public_view)
            .collect();
        let public = json!({"package_id":bundle.package_id,"source_space":bundle.source_space,"source_name":bundle.source_name,
            "exported_at":bundle.exported_at,"new":counts.get("new").copied().unwrap_or(0),"skipped":counts.get("existing").copied().unwrap_or(0),
            "conflicts":counts.get("conflict").copied().unwrap_or(0),"total":items.len(),"items":rows,"truncated":items.len()>100,
            "preview_digest":digest,"replayed":false});
        Ok(Preview {
            items,
            public,
            hash,
            receipt: None,
        })
    }
    pub(crate) async fn memory_origin<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        agent: Option<&[u8]>,
    ) -> Result<Value> {
        let mut label = Self::source_label(db, space, agent).await?;
        // 可选来源故障或旧库尚未升级时，保留读取能力并明确标记信息不可用。
        let imported = memory_imports::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(db)
            .await;
        if imported.is_err() {
            label["import_provenance_available"] = json!(false);
        }
        if let Ok(Some(row)) = imported {
            if agent.is_none() {
                label["agent_name"] = json!(format!("迁入 · {}", row.source_space));
            }
            label["imported_from"] = json!({"source_space":row.source_space,"source_name":row.source_name,"source_revision":row.source_revision,
                "package_id":uuid_from_bytes(&row.package_id)?,"imported_at":row.imported_at.to_string()});
        }
        Ok(label)
    }
}
