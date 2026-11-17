//! 文本模型独立于 embedding 配置；复用管理空间、根密钥加密、共享并发与日用量。
use super::*;
use crate::{
    entity::text_model_settings as settings,
    text_model::{Connection, Save},
};
use p256::elliptic_curve::zeroize::Zeroizing;
use sea_orm::IntoActiveModel;
use std::time::Duration;

impl Store {
    pub(super) fn legacy_text_connection(&self, row: &settings::Model) -> Result<Connection> {
        let key = match (&row.key_cipher, &row.key_nonce, &row.key_id) {
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
                Some(Zeroizing::new(
                    String::from_utf8(plain.to_vec()).map_err(|_| Error::unavailable())?,
                ))
            }
            (None, None, None) => None,
            _ => return Err(Error::unavailable()),
        };
        Ok(Connection {
            max_output_tokens: row.max_output_tokens,
            base_url: row.base_url.clone(),
            model: row.model_name.clone(),
            key,
        })
    }

    pub(super) async fn text_connection(&self, row: &settings::Model) -> Result<Connection> {
        if let Some(id) = &row.provider_id {
            let p = Self::provider(&self.db, uuid_from_bytes(id)?).await?;
            return Ok(Connection {
                max_output_tokens: row.max_output_tokens,
                base_url: p.base_url.clone(),
                model: row.model_name.clone(),
                key: self.provider_key(&p)?,
            });
        }
        self.legacy_text_connection(row)
    }

    pub async fn text_model_settings(&self, space: SpaceId) -> Result<Value> {
        self.check_model_admin(space).await?;
        let Some(row) = settings::Entity::find_by_id(1u8).one(&self.db).await? else {
            return Ok(
                json!({"revision":0,"enabled":false,"base_url":"","model":"","has_api_key":false,"max_concurrency":4,"max_output_tokens":16384}),
            );
        };
        let provider = match &row.provider_id {
            Some(id) => Some(Self::provider(&self.db, uuid_from_bytes(id)?).await?),
            None => None,
        };
        Ok(
            json!({"revision":row.revision,"enabled":row.enabled!=0,"provider_id":row.provider_id.as_deref().map(uuid_from_bytes).transpose()?,"base_url":provider.as_ref().map_or(row.base_url.as_str(),|p|p.base_url.as_str()),"model":row.model_name,"max_output_tokens":row.max_output_tokens,
            "has_api_key":provider.as_ref().map_or(row.key_cipher.is_some(),|p|p.key_cipher.is_some()),"max_concurrency":self.model_limit(&row.model_name).await?.max}),
        )
    }

    async fn prepare_text_connection(
        &self,
        space: SpaceId,
        input: &Save,
    ) -> Result<(Connection, Option<crate::entity::model_providers::Model>)> {
        self.check_model_admin(space).await?;
        crate::types::bounded(&input.model, 256)?;
        if input.model.chars().any(char::is_control)
            || input
                .api_key
                .as_ref()
                .is_some_and(|key| key.len() > 8192 || key.chars().any(char::is_control))
            || (input.clear_api_key && input.api_key.as_ref().is_some_and(|key| !key.is_empty()))
        {
            return Err(Error::invalid("模型名称或 API Key 格式无效"));
        }
        crate::model_runtime::ModelLimit::new(input.max_concurrency, 0)?;
        let provider = match input.provider_id {
            Some(id) => Some(Self::provider(&self.db, id).await?),
            None => None,
        };
        if provider.is_some() && (input.api_key.is_some() || input.clear_api_key) {
            return Err(Error::invalid("已选择服务商，请在服务商配置中修改 API Key"));
        }
        let base_url = if let Some(p) = &provider {
            p.base_url.clone()
        } else {
            crate::embedding::validate_url(&input.base_url)?
        };
        let old = settings::Entity::find_by_id(1u8).one(&self.db).await?;
        if old.as_ref().map_or(0, |row| row.revision) != input.expected_revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        let max_output_tokens = input
            .max_output_tokens
            .or(old.as_ref().map(|row| row.max_output_tokens))
            .unwrap_or(16384);
        if !(1024..=65536).contains(&max_output_tokens) {
            return Err(Error::invalid("文本模型输出预算须为1024～65536 Token"));
        }
        let key = if let Some(p) = &provider {
            self.provider_key(p)?
        } else if input.clear_api_key {
            None
        } else if let Some(key) = input.api_key.as_ref().filter(|key| !key.is_empty()) {
            Some(Zeroizing::new(key.clone()))
        } else if let Some(row) = old {
            // 地址和凭据必须来自同一次Provider解析；旧行的URL镜像不能验证动态新Key。
            let previous = self.text_connection(&row).await?;
            if previous.key.is_some() && previous.base_url != base_url {
                return Err(Error::invalid(
                    "更换服务地址须重新提供 API Key，或明确清除旧 Key",
                ));
            }
            previous.key
        } else {
            None
        };
        Ok((
            Connection {
                max_output_tokens,
                base_url,
                model: input.model.clone(),
                key,
            },
            provider,
        ))
    }

    pub(super) async fn call_text_model(
        &self,
        connection: &Connection,
        input: &Value,
    ) -> Result<Value> {
        self.call_text_model_guarded(connection, input, None).await
    }

    pub(super) async fn call_text_model_guarded(
        &self,
        connection: &Connection,
        input: &Value,
        admission: Option<super::knowledge_worker::KnowledgeAdmission<'_>>,
    ) -> Result<Value> {
        let permit = tokio::time::timeout(
            Duration::from_secs(15),
            self.model_runtime.acquire(
                &connection.model,
                self.model_limit(&connection.model).await?,
            ),
        )
        .await
        .map_err(|_| {
            crate::text_model::error("model_busy", "模型并发已满，等待超时，将稍后重试")
        })?;
        // 等待额度可能跨越暂停、遗忘、Provider或模型配置切换；许可不等于旧任务仍获授权。
        if let Some(admission) = admission {
            self.validate_knowledge_admission(&admission).await?;
        }
        let mut usage = None;
        let result = connection.request(input, &mut usage).await;
        drop(permit);
        if let Some(mut usage) = usage {
            usage.failed = result.is_err();
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    self.record_model_usage(&connection.model, &usage)
                )
                .await,
                Ok(Ok(()))
            ) {
                tracing::warn!("文本模型用量未能确认累计，模型结果继续使用");
            }
        }
        result
    }

    pub async fn test_text_model(&self, space: SpaceId, input: Save) -> Result<Value> {
        let (connection, _) = self.prepare_text_connection(space, &input).await?;
        self.probe_text_model(&connection).await
    }

    async fn probe_text_model(&self, connection: &Connection) -> Result<Value> {
        let started = std::time::Instant::now();
        let value = self
            .call_text_model(
                connection,
                &json!({"kind":"probe","memories":[],"events":[]}),
            )
            .await?;
        let parsed: crate::text_model::Output = serde_json::from_value(value)
            .map_err(|_| crate::text_model::error("model_response", "模型未返回约定的整理 JSON"))?;
        if !parsed.notes.is_empty() || !parsed.conflicts.is_empty() {
            return Err(crate::text_model::error(
                "model_response",
                "模型未遵守连接测试约定",
            ));
        }
        Ok(
            json!({"connected":true,"model":connection.model,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
        )
    }

    pub async fn save_text_model(&self, space: SpaceId, input: Save) -> Result<Value> {
        let (connection, provider) = self.prepare_text_connection(space, &input).await?;
        self.probe_text_model(&connection).await?;
        let tx = self.db.begin().await?;
        // 与 embedding 共用管理配置锁和单调版本，避免同名模型的并发配置彼此覆盖。
        let mut owner = Self::model_settings_locked(&tx).await?.into_active_model();
        if owner.admin_space_id.as_ref() != &Some(space.bytes()) {
            return Err(Error::unauthorized());
        }
        let global_revision = owner
            .revision
            .as_ref()
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        let previous = settings::Entity::find_by_id(1u8)
            .lock_exclusive()
            .one(&tx)
            .await?;
        if previous.as_ref().map_or(0, |row| row.revision) != input.expected_revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        let revision = input
            .expected_revision
            .checked_add(1)
            .ok_or_else(Error::unavailable)?;
        let credential_id = Uuid::now_v7();
        if let Some(p) = &provider {
            Self::validate_provider(&tx, p).await?;
        }
        let envelope = connection
            .key
            .as_ref()
            .filter(|_| provider.is_none())
            .map(|key| self.cipher()?.seal_model(credential_id, 1, key.as_bytes()))
            .transpose()?;
        let existed = previous.is_some();
        // 连接配置不代表自动处理授权；任务选择只由 save_organization 修改。
        let mut active = previous
            .map(|row| row.into_active_model())
            .unwrap_or_default();
        active.id = Set(1);
        active.revision = Set(revision);
        active.credential_id = Set(credential_id.as_bytes().to_vec());
        active.enabled = Set(1);
        active.provider_id = Set(provider.as_ref().map(|p| p.id.clone()));
        active.base_url = Set(connection.base_url);
        active.model_name = Set(connection.model.clone());
        active.max_output_tokens = Set(connection.max_output_tokens);
        active.key_cipher = Set(envelope.as_ref().map(|value| value.ciphertext.clone()));
        active.key_nonce = Set(envelope.as_ref().map(|value| value.nonce.clone()));
        active.key_id = Set(envelope
            .as_ref()
            .map(|_| self.cipher().map(|cipher| cipher.key_id().to_owned()))
            .transpose()?);
        active.updated_at = Set(now());
        if existed {
            active.update(&tx).await?;
        } else {
            active.insert(&tx).await?;
        }
        let limit = crate::model_runtime::ModelLimit::new(input.max_concurrency, global_revision)?;
        Self::persist_model_limit(&tx, &connection.model, limit).await?;
        owner.revision = Set(global_revision);
        owner.updated_at = Set(now());
        owner.update(&tx).await?;
        tx.commit().await?;
        self.model_runtime.apply(&connection.model, limit);
        self.text_model_settings(space).await
    }

    pub async fn disable_text_model(&self, space: SpaceId, revision: i64) -> Result<Value> {
        self.check_model_admin(space).await?;
        let tx = self.db.begin().await?;
        Self::model_settings_locked(&tx).await?;
        let row = settings::Entity::find_by_id(1u8)
            .lock_exclusive()
            .one(&tx)
            .await?
            .ok_or_else(Error::missing)?;
        if row.revision != revision {
            return Err(Error::conflict("model_settings_changed"));
        }
        let mut active = row.into_active_model();
        active.enabled = Set(0);
        active.revision = Set(revision.checked_add(1).ok_or_else(Error::unavailable)?);
        active.updated_at = Set(now());
        active.update(&tx).await?;
        tx.commit().await?;
        self.text_model_settings(space).await
    }
}
