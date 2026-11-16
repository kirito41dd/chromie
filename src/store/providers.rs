//! Provider 写入与模型配置共用管理锁，连接切换会撤销旧文本结果和向量批次。
use super::*;
use crate::{
    entity::{embedding_generations, model_providers as providers, text_model_settings},
    provider::Save,
};
use p256::elliptic_curve::zeroize::Zeroizing;
use sea_orm::{ExprTrait, IntoActiveModel, QueryOrder, sea_query::Expr};

impl Store {
    pub(super) fn provider_key(&self, row: &providers::Model) -> Result<Option<Zeroizing<String>>> {
        match (&row.key_cipher, &row.key_nonce, &row.key_id) {
            (Some(ciphertext), Some(nonce), Some(key_id)) => {
                let plain = self.cipher()?.open_model(
                    uuid_from_bytes(&row.credential_id)?,
                    crate::secret_crypto::EncryptedPayload {
                        revision: 1,
                        format: 1,
                        key_id,
                        nonce,
                        ciphertext,
                    },
                )?;
                Ok(Some(Zeroizing::new(
                    String::from_utf8(plain.to_vec()).map_err(|_| Error::unavailable())?,
                )))
            }
            (None, None, None) => Ok(None),
            _ => Err(Error::unavailable()),
        }
    }
    pub(super) async fn provider<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<providers::Model> {
        providers::Entity::find_by_id(id.as_bytes().to_vec())
            .one(db)
            .await?
            .ok_or_else(Error::missing)
    }
    pub(super) async fn validate_provider<C: ConnectionTrait>(
        db: &C,
        expected: &providers::Model,
    ) -> Result<()> {
        let current = Self::provider(db, uuid_from_bytes(&expected.id)?).await?;
        if current.revision != expected.revision {
            return Err(Error::conflict("provider_changed"));
        }
        Ok(())
    }
    pub async fn model_providers(&self, space: SpaceId) -> Result<Value> {
        self.check_model_admin(space).await?;
        let rows = providers::Entity::find()
            .order_by_asc(providers::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(json!({"providers":rows.iter().map(Self::provider_view).collect::<Vec<_>>()}))
    }
    fn provider_view(row: &providers::Model) -> Value {
        json!({"provider_id":uuid_from_bytes(&row.id).ok(),"revision":row.revision,"name":row.name,"base_url":row.base_url,"has_api_key":row.key_cipher.is_some()})
    }
    pub async fn save_provider(&self, space: SpaceId, input: Save) -> Result<Value> {
        self.check_model_admin(space).await?;
        crate::types::bounded(input.name.trim(), 128)?;
        if input.name.chars().any(char::is_control)
            || input
                .api_key
                .as_ref()
                .is_some_and(|key| key.len() > 8192 || key.chars().any(char::is_control))
            || (input.clear_api_key && input.api_key.as_ref().is_some_and(|key| !key.is_empty()))
        {
            return Err(Error::invalid("服务商名称或 API Key 格式无效"));
        }
        let url = crate::embedding::validate_url(&input.base_url)?;
        let tx = self.db.begin().await?;
        let owner = Self::model_settings_locked(&tx).await?;
        if owner.admin_space_id != Some(space.bytes()) {
            return Err(Error::unauthorized());
        }
        let old = match input.provider_id {
            Some(id) => Some(Self::provider(&tx, id).await?),
            None => None,
        };
        if old.as_ref().map_or(0, |row| row.revision) != input.expected_revision {
            return Err(Error::conflict("provider_changed"));
        }
        let key = if input.clear_api_key {
            None
        } else if let Some(key) = input.api_key.as_ref().filter(|key| !key.is_empty()) {
            Some(Zeroizing::new(key.clone()))
        } else if let Some(row) = &old {
            if row.base_url != url && row.key_cipher.is_some() {
                return Err(Error::invalid(
                    "更换服务地址须重新提供 API Key，或明确选择接口无需 Key",
                ));
            }
            self.provider_key(row)?
        } else {
            None
        };
        let connection_changed = old.as_ref().is_none_or(|row| row.base_url != url)
            || old
                .as_ref()
                .map(|row| self.provider_key(row))
                .transpose()?
                .flatten()
                .as_deref()
                != key.as_deref();
        let id = input.provider_id.unwrap_or_else(Uuid::now_v7);
        let credential_id = Uuid::now_v7();
        let envelope = key
            .as_ref()
            .map(|key| self.cipher()?.seal_model(credential_id, 1, key.as_bytes()))
            .transpose()?;
        let existed = old.is_some();
        let mut active = old.map(|row| row.into_active_model()).unwrap_or_default();
        active.id = Set(id.as_bytes().to_vec());
        active.name = Set(input.name.trim().to_owned());
        active.base_url = Set(url.clone());
        active.credential_id = Set(credential_id.as_bytes().to_vec());
        active.revision = Set(input
            .expected_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?);
        active.key_cipher = Set(envelope.as_ref().map(|e| e.ciphertext.clone()));
        active.key_nonce = Set(envelope.as_ref().map(|e| e.nonce.clone()));
        active.key_id = Set(if envelope.is_some() {
            Some(self.cipher()?.key_id().to_owned())
        } else {
            None
        });
        active.updated_at = Set(now());
        if existed {
            active.update(&tx).await?;
        } else {
            active.created_at = Set(now());
            active.insert(&tx).await?;
        }
        let mut embedding_changed = false;
        if connection_changed {
            embedding_changed = embedding_generations::Entity::update_many()
                .col_expr(
                    embedding_generations::Column::ConnectionRevision,
                    Expr::col(embedding_generations::Column::ConnectionRevision).add(1),
                )
                .col_expr(
                    embedding_generations::Column::BaseUrl,
                    Expr::value(url.clone()),
                )
                .filter(embedding_generations::Column::ProviderId.eq(id.as_bytes().to_vec()))
                .exec(&tx)
                .await?
                .rows_affected
                > 0;
            text_model_settings::Entity::update_many()
                .col_expr(
                    text_model_settings::Column::Revision,
                    Expr::col(text_model_settings::Column::Revision).add(1),
                )
                .col_expr(text_model_settings::Column::BaseUrl, Expr::value(url))
                .filter(text_model_settings::Column::ProviderId.eq(id.as_bytes().to_vec()))
                .exec(&tx)
                .await?;
        }
        if embedding_changed {
            let mut owner = owner.into_active_model();
            owner.revision = Set(owner
                .revision
                .as_ref()
                .checked_add(1)
                .ok_or_else(Error::unavailable)?);
            owner.updated_at = Set(now());
            owner.update(&tx).await?;
        }
        tx.commit().await?;
        self.model_providers(space).await
    }

    /// 小规模实例启动回填：逐个连接绑定后即可重入；只合并地址与原 Key 完全相同的连接。
    pub async fn migrate_model_providers(&self) -> Result<()> {
        let tx = self.db.begin().await?;
        if crate::entity::instance_model_settings::Entity::find_by_id(1u8)
            .one(&tx)
            .await?
            .is_none()
        {
            tx.commit().await?;
            return Ok(());
        }
        Self::model_settings_locked(&tx).await?;
        let rows = embedding_generations::Entity::find()
            .filter(embedding_generations::Column::ProviderId.is_null())
            .filter(embedding_generations::Column::State.is_in(["active", "building"]))
            .all(&tx)
            .await?;
        for row in rows {
            let connection = self.legacy_model_connection(&row)?;
            let id = self
                .find_or_create_provider(
                    &tx,
                    "模型服务",
                    &connection.base_url,
                    connection.key.as_ref(),
                )
                .await?;
            let mut active = row.into_active_model();
            active.provider_id = Set(Some(id));
            active.key_cipher = Set(None);
            active.key_nonce = Set(None);
            active.key_id = Set(None);
            active.update(&tx).await?;
        }
        if let Some(row) = text_model_settings::Entity::find_by_id(1u8)
            .filter(text_model_settings::Column::ProviderId.is_null())
            .one(&tx)
            .await?
        {
            let connection = self.legacy_text_connection(&row)?;
            let id = self
                .find_or_create_provider(
                    &tx,
                    "模型服务",
                    &connection.base_url,
                    connection.key.as_ref(),
                )
                .await?;
            let mut active = row.into_active_model();
            active.provider_id = Set(Some(id));
            active.key_cipher = Set(None);
            active.key_nonce = Set(None);
            active.key_id = Set(None);
            active.update(&tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    async fn find_or_create_provider<C: ConnectionTrait>(
        &self,
        db: &C,
        name: &str,
        url: &str,
        key: Option<&Zeroizing<String>>,
    ) -> Result<Vec<u8>> {
        let rows = providers::Entity::find().all(db).await?;
        let count = rows
            .iter()
            .filter(|row| row.name == name || row.name.starts_with(&format!("{name} ")))
            .count();
        let display_name = if count == 0 {
            name.to_owned()
        } else {
            format!("{name} {}", count + 1)
        };
        for row in rows {
            if row.base_url == url
                && self.provider_key(&row)?.as_ref().map(|v| v.as_str()) == key.map(|v| v.as_str())
            {
                return Ok(row.id);
            }
        }
        let id = Uuid::now_v7();
        let credential = Uuid::now_v7();
        let encrypted = key
            .map(|key| self.cipher()?.seal_model(credential, 1, key.as_bytes()))
            .transpose()?;
        providers::ActiveModel {
            id: Set(id.as_bytes().to_vec()),
            name: Set(display_name),
            base_url: Set(url.to_owned()),
            credential_id: Set(credential.as_bytes().to_vec()),
            revision: Set(1),
            key_cipher: Set(encrypted.as_ref().map(|e| e.ciphertext.clone())),
            key_nonce: Set(encrypted.as_ref().map(|e| e.nonce.clone())),
            key_id: Set(if encrypted.is_some() {
                Some(self.cipher()?.key_id().to_owned())
            } else {
                None
            }),
            created_at: Set(now()),
            updated_at: Set(now()),
        }
        .insert(db)
        .await?;
        Ok(id.as_bytes().to_vec())
    }
}
