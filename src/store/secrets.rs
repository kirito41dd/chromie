use super::*;
use crate::secret_contract::SecretContract;
use crate::{
    entity::secrets,
    secret_crypto::{FORMAT_VERSION, MAX_SECRET_BYTES, SecretCipher},
    types::{SecretCreate, SecretUpdate, bounded},
};
use p256::elliptic_curve::zeroize::Zeroizing;
use sea_orm::IntoActiveModel;

impl Store {
    pub async fn enable_secrets(mut self, cipher: SecretCipher) -> Result<Self> {
        let mismatch = secrets::Entity::find()
            .select_only()
            .column(secrets::Column::KeyId)
            .filter(secrets::Column::Ciphertext.is_not_null())
            .filter(secrets::Column::KeyId.ne(cipher.key_id()))
            .into_tuple::<String>()
            .one(&self.db)
            .await?;
        let credential_mismatch = crate::entity::agent_credentials::Entity::find()
            .filter(crate::entity::agent_credentials::Column::KeyId.ne(cipher.key_id()))
            .one(&self.db)
            .await?;
        if mismatch.is_some() || credential_mismatch.is_some() {
            return Err(Error::invalid(
                "数据库密文与当前根私钥不匹配，已拒绝启动；请保留数据库并恢复原私钥，禁止重新初始化或覆盖",
            ));
        }
        self.secret_cipher = Some(cipher);
        Ok(self)
    }

    pub(super) fn cipher(&self) -> Result<&SecretCipher> {
        self.secret_cipher.as_ref().ok_or_else(Error::unavailable)
    }

    pub(super) async fn secret_reference<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<Value> {
        let row = spaces::Entity::find_by_id(space.bytes())
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
        Ok(
            json!({"kind":"secret_ref","secret_ref":format!("secret://{}/{id}",row.slug),"fetch_path":format!("/spaces/{}/secrets/{id}",row.slug)}),
        )
    }

    // 只查询密文是否存在，不为看板或召回读取密文内容，更不解密。
    pub(crate) async fn secret_storage<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<Option<bool>> {
        Ok(
            secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
                .select_only()
                .column_as(secrets::Column::Ciphertext.count(), "stored")
                .group_by(secrets::Column::SpaceId)
                .group_by(secrets::Column::MemoryId)
                .into_tuple::<i64>()
                .one(db)
                .await?
                .map(|count| count > 0),
        )
    }

    pub(super) async fn decorate_secret<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        value: &mut Value,
    ) -> Result<()> {
        if let Some(stored) = Self::secret_storage(db, space, id).await? {
            value["secret_stored"] = json!(stored);
            let reference = Self::secret_reference(db, space, id).await?;
            for (key, field) in reference.as_object().ok_or_else(Error::unavailable)? {
                value[key] = field.clone();
            }
            if let Some(content) = value.get("content").cloned() {
                value["label"] = content;
            }
            if value.get("is_current").and_then(Value::as_bool) == Some(true) {
                value["secret_format"] = json!(Self::contract(db, space, id).await?.format());
            }
        }
        Ok(())
    }

    pub(super) async fn secret_reply<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
        mut value: Value,
    ) -> Result<Value> {
        if value.get("hidden").and_then(Value::as_bool) != Some(true) {
            let reference = Self::secret_reference(db, space, id).await?;
            for (key, field) in reference.as_object().ok_or_else(Error::unavailable)? {
                value[key] = field.clone();
            }
        }
        Ok(value)
    }

    fn check_secret_body(body: &[u8]) -> Result<()> {
        if body.is_empty() || body.len() > MAX_SECRET_BYTES {
            return Err(Error::invalid("Secret 正文须为 1～60,000 字节"));
        }
        Ok(())
    }

    pub async fn create_secret(
        &self,
        space: SpaceId,
        input: SecretCreate,
        plaintext: &[u8],
    ) -> Result<Value> {
        bounded(&input.label, 256)?;
        if let Some(project) = &input.project_key {
            bounded(project, 128)?;
        }
        Self::check_secret_body(plaintext)?;
        if let Some(contract) = &input.contract {
            contract.validate_payload(plaintext)?;
        }
        let cipher = self.cipher()?;
        let metadata = serde_json::to_vec(&input).map_err(|_| Error::unavailable())?;
        let hash = cipher.fingerprint(space, &metadata, plaintext)?;
        let tx = self.begin(space).await?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, true).await?;
        }
        if let Some(value) = self
            .replay(&tx, space, "secret_create", input.request_id, &hash)
            .await?
        {
            let id = value
                .get("memory_id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(Error::unavailable)?;
            return Self::secret_reply(&tx, space, id, value).await;
        }
        let id = Uuid::now_v7();
        let envelope = cipher.seal(space, id, 1, plaintext)?;
        let current = now();
        memories::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            current_revision: Set(1),
            created_at: Set(current),
            updated_at: Set(current),
            forgotten_at: Set(None),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        memory_revisions::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            revision: Set(1),
            content: Set(Some(input.label)),
            project_key: Set(input.project_key),
            change_reason: Set(None),
            created_at: Set(current),
            agent_id: Set(self.actor_bytes()),
        }
        .insert(&tx)
        .await?;
        secrets::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            format_version: Set(FORMAT_VERSION),
            payload_revision: Set(1),
            key_id: Set(cipher.key_id().to_owned()),
            nonce: Set(envelope.nonce),
            ciphertext: Set(Some(envelope.ciphertext)),
            created_at: Set(current),
            updated_at: Set(current),
            contract: Set(input
                .contract
                .as_ref()
                .map(SecretContract::value)
                .transpose()?),
        }
        .insert(&tx)
        .await?;
        Self::schedule(&tx, space, id, 1).await?;
        self.receipt(
            &tx,
            space,
            "secret_create",
            input.request_id,
            hash,
            (id, Some(1)),
        )
        .await?;
        let result = Self::secret_reply(
            &tx,
            space,
            id,
            json!({"memory_id":id,"revision":1,"readable":true,"processing_state":"index_pending"}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn update_secret(
        &self,
        space: SpaceId,
        id: Uuid,
        input: SecretUpdate,
        plaintext: &[u8],
    ) -> Result<Value> {
        Self::check_secret_body(plaintext)?;
        let cipher = self.cipher()?;
        let metadata = serde_json::to_vec(&(id, &input)).map_err(|_| Error::unavailable())?;
        let hash = cipher.fingerprint(space, &metadata, plaintext)?;
        let tx = self.begin(space).await?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, true).await?;
        }
        if let Some(value) = self
            .replay(&tx, space, "secret_update", input.request_id, &hash)
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
            .filter(|row| row.ciphertext.is_some())
            .ok_or_else(Error::missing)?;
        let contract = match &input.contract {
            Some(contract) => contract.clone(),
            None => SecretContract::from_storage(row.contract.as_ref())?,
        };
        contract.validate_payload(plaintext)?;
        let payload_revision = row
            .payload_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        let revision = head
            .current_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        let envelope = cipher.seal(space, id, payload_revision, plaintext)?;
        let previous = Self::revision(&tx, &head).await?;
        let mut active = row.into_active_model();
        active.format_version = Set(FORMAT_VERSION);
        active.payload_revision = Set(payload_revision);
        active.key_id = Set(cipher.key_id().to_owned());
        active.nonce = Set(envelope.nonce);
        active.ciphertext = Set(Some(envelope.ciphertext));
        active.contract = Set(Some(contract.value()?));
        active.updated_at = Set(now());
        active.update(&tx).await?;
        memory_revisions::ActiveModel {
            space_id: Set(space.bytes()),
            memory_id: Set(id.as_bytes().to_vec()),
            revision: Set(revision),
            content: Set(previous.content),
            project_key: Set(previous.project_key),
            change_reason: Set(Some("更新 Secret 载荷；原文仅保存密文".into())),
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
        self.receipt(
            &tx,
            space,
            "secret_update",
            input.request_id,
            hash,
            (id, Some(revision)),
        )
        .await?;
        let result = Self::secret_reply(&tx, space, id, json!({"memory_id":id,"revision":revision,"readable":true,"processing_state":"index_pending"})).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn read_secret(&self, space: SpaceId, id: Uuid) -> Result<Zeroizing<Vec<u8>>> {
        Ok(self.read_secret_at(space, id, None).await?.bytes)
    }

    pub async fn read_secret_at(
        &self,
        space: SpaceId,
        id: Uuid,
        expected_revision: Option<i32>,
    ) -> Result<SecretRead> {
        self.read_secret_for(space, id, expected_revision, None)
            .await
    }

    pub async fn read_secret_for(
        &self,
        space: SpaceId,
        id: Uuid,
        expected_revision: Option<i32>,
        purpose: Option<String>,
    ) -> Result<SecretRead> {
        crate::secret_audit::validate_purpose(purpose.as_deref())?;
        let cipher = self.cipher()?;
        let tx = self.begin(space).await?;
        if let Some(actor) = &self.actor {
            Self::validate_agent(&tx, space, actor, true).await?;
        }
        let head = Self::head(&tx, space, id).await?;
        if head.forgotten_at.is_some() {
            return Err(Error::missing());
        }
        if expected_revision.is_some_and(|version| version < 1 || version != head.current_revision)
        {
            return Err(Error::conflict("revision_conflict"));
        }
        let row = secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        let ciphertext = row.ciphertext.as_deref().ok_or_else(Error::missing)?;
        let plaintext = cipher.open(
            space,
            id,
            crate::secret_crypto::EncryptedPayload {
                revision: row.payload_revision,
                format: row.format_version,
                key_id: &row.key_id,
                nonce: &row.nonce,
                ciphertext,
            },
        )?;
        SecretContract::from_storage(row.contract.as_ref())?.validate_payload(&plaintext)?;
        let audit_id = self
            .audit_delivery(&tx, space, id, head.current_revision, purpose)
            .await?;
        tx.commit().await?;
        Ok(SecretRead {
            bytes: plaintext,
            revision: head.current_revision,
            audit_id,
        })
    }

    pub(super) async fn contract<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<SecretContract> {
        let value = secrets::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .select_only()
            .column(secrets::Column::Contract)
            .into_tuple::<Option<Value>>()
            .one(db)
            .await?
            .ok_or_else(Error::missing)?;
        SecretContract::from_storage(value.as_ref())
    }
}

pub struct SecretRead {
    pub bytes: Zeroizing<Vec<u8>>,
    pub revision: i32,
    pub audit_id: Uuid,
}
