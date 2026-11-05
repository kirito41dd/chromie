//! 包回执、当前修订、Secret 密文及两种索引任务在同一空间事务内追加。
use super::*;
use crate::{
    entity::{memory_imports, secrets, transfer_receipts},
    transfer::Bundle,
};
impl Store {
    pub(crate) async fn import_transfer(
        &self,
        space: SpaceId,
        version: &str,
        bundle: Bundle,
        digest: &str,
    ) -> Result<Value> {
        let tx = self.begin(space).await?;
        let owner = Self::transfer_space(&tx, space, version).await?;
        let package = bundle.package_id;
        let source = bundle.source_space.clone();
        let name = bundle.source_name.clone();
        let preview = self
            .classify_transfer(&tx, space, &owner.slug, version, bundle)
            .await?;
        if let Some(result) = preview.receipt {
            tx.commit().await?;
            return Ok(result);
        }
        if preview.public.get("preview_digest").and_then(Value::as_str) != Some(digest) {
            return Err(Error::conflict("transfer_preview_changed"));
        }
        let current = now();
        let mut result = preview.public;
        result["imported"] = result["new"].clone();
        result
            .as_object_mut()
            .ok_or_else(Error::unavailable)?
            .remove("items");
        result
            .as_object_mut()
            .ok_or_else(Error::unavailable)?
            .remove("preview_digest");
        transfer_receipts::ActiveModel {
            space_id: Set(space.bytes()),
            package_id: Set(package.as_bytes().to_vec()),
            content_hash: Set(preview.hash),
            result: Set(result.clone()),
            created_at: Set(current),
        }
        .insert(&tx)
        .await?;
        for item in preview.items {
            if item.status != "new" {
                continue;
            }
            let row = item.record;
            let id = row.memory_id;
            memories::ActiveModel {
                space_id: Set(space.bytes()),
                id: Set(id.as_bytes().to_vec()),
                current_revision: Set(1),
                created_at: Set(current),
                updated_at: Set(current),
                forgotten_at: Set(None),
                agent_id: Set(None),
            }
            .insert(&tx)
            .await?;
            memory_revisions::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                revision: Set(1),
                content: Set(Some(row.content)),
                project_key: Set(row.project_key),
                change_reason: Set(Some(format!(
                    "夺舍迁入：{}，源修订 {}",
                    source, row.source_revision
                ))),
                created_at: Set(current),
                agent_id: Set(None),
            }
            .insert(&tx)
            .await?;
            if let Some(secret) = row.secret {
                let cipher = self.cipher()?;
                let envelope = cipher.seal(space, id, 1, &secret.value.0)?;
                secrets::ActiveModel {
                    space_id: Set(space.bytes()),
                    memory_id: Set(id.as_bytes().to_vec()),
                    format_version: Set(crate::secret_crypto::FORMAT_VERSION),
                    payload_revision: Set(1),
                    key_id: Set(cipher.key_id().to_owned()),
                    nonce: Set(envelope.nonce),
                    ciphertext: Set(Some(envelope.ciphertext)),
                    created_at: Set(current),
                    updated_at: Set(current),
                    contract: Set(Some(secret.contract.value()?)),
                }
                .insert(&tx)
                .await?;
            }
            memory_imports::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                package_id: Set(package.as_bytes().to_vec()),
                source_space: Set(source.clone()),
                source_name: Set(name.clone()),
                source_revision: Set(row.source_revision),
                imported_at: Set(current),
            }
            .insert(&tx)
            .await?;
            Self::schedule(&tx, space, id, 1).await?;
        }
        tx.commit().await?;
        Ok(result)
    }
}
