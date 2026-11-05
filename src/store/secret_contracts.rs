use super::*;
use crate::{
    entity::secrets, secret_contract::SecretContract, secret_crypto::EncryptedPayload,
    types::SecretContractUpdate,
};
use sea_orm::IntoActiveModel;

impl Store {
    pub async fn set_secret_contract(
        &self,
        space: SpaceId,
        id: Uuid,
        input: SecretContractUpdate,
    ) -> Result<Value> {
        input.contract.validate()?;
        let hash = Self::hash(&(id, &input))?;
        let tx = self.begin(space).await?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, true).await?;
        }
        if let Some(value) = self
            .replay(&tx, space, "secret_contract", input.request_id, &hash)
            .await?
        {
            return Self::secret_reply(&tx, space, id, value).await;
        }
        let head = Self::head(&tx, space, id).await?;
        if head.forgotten_at.is_some() {
            return Err(Error::missing());
        }
        if head.current_revision != input.expected_revision {
            return Err(Error::conflict("revision_conflict"));
        }
        let row = secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let plaintext = self.cipher()?.open(
            space,
            id,
            EncryptedPayload {
                revision: row.payload_revision,
                format: row.format_version,
                key_id: &row.key_id,
                nonce: &row.nonce,
                ciphertext: row.ciphertext.as_deref().ok_or_else(Error::missing)?,
            },
        )?;
        input.contract.validate_payload(&plaintext)?;
        let unchanged = SecretContract::from_storage(row.contract.as_ref())? == input.contract;
        let revision = if unchanged {
            head.current_revision
        } else {
            head.current_revision
                .checked_add(1)
                .ok_or_else(Error::unavailable)?
        };
        if !unchanged {
            let previous = Self::revision(&tx, &head).await?;
            let mut active = row.into_active_model();
            active.contract = Set(Some(input.contract.value()?));
            active.updated_at = Set(now());
            active.update(&tx).await?;
            memory_revisions::ActiveModel {
                space_id: Set(space.bytes()),
                memory_id: Set(id.as_bytes().to_vec()),
                revision: Set(revision),
                content: Set(previous.content),
                project_key: Set(previous.project_key),
                change_reason: Set(Some("更新 Secret 格式契约；原文未改变".into())),
                created_at: Set(now()),
                agent_id: Set(self.actor_bytes()),
            }
            .insert(&tx)
            .await?;
            let mut active = head.into_active_model();
            active.current_revision = Set(revision);
            active.updated_at = Set(now());
            active.update(&tx).await?;
            Self::schedule(&tx, space, id, revision).await?;
        }
        self.receipt(
            &tx,
            space,
            "secret_contract",
            input.request_id,
            hash,
            (id, Some(revision)),
        )
        .await?;
        let reply = Self::secret_reply(
            &tx,
            space,
            id,
            json!({"memory_id":id,"revision":revision,"readable":true,"unchanged":unchanged}),
        )
        .await?;
        tx.commit().await?;
        Ok(reply)
    }
}
