use super::*;
use crate::{
    agent::{AgentAccess, AgentRecord, CreateAgent, KeyKind, UpdateAgent},
    entity::{agent_connections as agents, agent_credentials as credentials},
    secret_crypto::{EncryptedPayload, FORMAT_VERSION},
    types::{bounded, timestamp},
};
use p256::{
    SecretKey,
    elliptic_curve::{Generate, zeroize::Zeroizing},
};
use sea_orm::{IntoActiveModel, PaginatorTrait, QueryOrder};

fn validate_name(name: &str) -> Result<()> {
    bounded(name, 100)?;
    if name.trim() != name || name.chars().any(char::is_control) {
        return Err(Error::invalid("接入名称不能包含控制字符或首尾空白"));
    }
    Ok(())
}

fn record(row: agents::Model) -> Result<AgentRecord> {
    Ok(AgentRecord {
        agent_id: uuid_from_bytes(&row.id)?,
        agent_name: row.name,
        client_type: row.client_type,
        enabled: row.enabled != 0,
        allow_secrets: row.allow_secrets != 0,
        created_at: timestamp(row.created_at)?,
        last_mcp_at: row.last_mcp_at.map(timestamp).transpose()?,
        last_hook_at: row.last_hook_at.map(timestamp).transpose()?,
    })
}

impl Store {
    pub async fn list_agents(&self, space: SpaceId) -> Result<Vec<AgentRecord>> {
        agents::Entity::find()
            .filter(agents::Column::SpaceId.eq(space.bytes()))
            .order_by_desc(agents::Column::CreatedAt)
            .limit(200)
            .all(&self.db)
            .await?
            .into_iter()
            .map(record)
            .collect()
    }

    async fn agent_row<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Uuid,
    ) -> Result<agents::Model> {
        agents::Entity::find_by_id((space.bytes(), id.as_bytes().to_vec()))
            .one(db)
            .await?
            .ok_or_else(Error::missing)
    }

    pub async fn create_agent(&self, space: SpaceId, input: CreateAgent) -> Result<AgentRecord> {
        validate_name(&input.agent_name)?;
        self.cipher()?;
        let tx = self.begin(space).await?;
        if agents::Entity::find()
            .filter(agents::Column::SpaceId.eq(space.bytes()))
            .count(&tx)
            .await?
            >= 200
        {
            return Err(Error::invalid("每个空间最多创建 200 个接入"));
        }
        Self::unique_agent_name(&tx, space, &input.agent_name, None).await?;
        let id = Uuid::now_v7();
        let row = agents::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(id.as_bytes().to_vec()),
            name: Set(input.agent_name),
            // 保留旧列兼容历史记录；新接入不绑定客户端软件。
            client_type: Set("other".into()),
            enabled: Set(1),
            allow_secrets: Set(0),
            created_at: Set(now()),
            updated_at: Set(now()),
            last_mcp_at: Set(None),
            last_hook_at: Set(None),
        }
        .insert(&tx)
        .await?;
        for kind in [KeyKind::Mcp, KeyKind::Hook] {
            self.write_agent_key(&tx, space, id, kind, 1).await?;
        }
        tx.commit().await?;
        record(row)
    }

    async fn unique_agent_name<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        name: &str,
        except: Option<Uuid>,
    ) -> Result<()> {
        let mut query = agents::Entity::find()
            .filter(agents::Column::SpaceId.eq(space.bytes()))
            .filter(agents::Column::Name.eq(name));
        if let Some(id) = except {
            query = query.filter(agents::Column::Id.ne(id.as_bytes().to_vec()));
        }
        if query.one(db).await?.is_some() {
            return Err(Error {
                code: "agent_name_conflict",
                message: "接入名称已被使用",
                status: axum::http::StatusCode::CONFLICT,
            });
        }
        Ok(())
    }

    pub async fn update_agent(
        &self,
        space: SpaceId,
        id: Uuid,
        input: UpdateAgent,
    ) -> Result<AgentRecord> {
        let tx = self.begin(space).await?;
        let mut row = Self::agent_row(&tx, space, id).await?.into_active_model();
        if let Some(name) = input.agent_name {
            validate_name(&name)?;
            Self::unique_agent_name(&tx, space, &name, Some(id)).await?;
            row.name = Set(name);
        }
        if let Some(enabled) = input.enabled {
            row.enabled = Set(i8::from(enabled));
        }
        if let Some(allow) = input.allow_secrets {
            row.allow_secrets = Set(i8::from(allow));
        }
        row.updated_at = Set(now());
        let updated = row.update(&tx).await?;
        tx.commit().await?;
        record(updated)
    }

    async fn write_agent_key(
        &self,
        tx: &DatabaseTransaction,
        space: SpaceId,
        id: Uuid,
        kind: KeyKind,
        revision: i32,
    ) -> Result<()> {
        // P-256 的系统随机源提供约 256 位熵；仅使用随机字节作为不透明凭据，不执行签名。
        let random = SecretKey::try_generate().map_err(|_| Error::unavailable())?;
        let raw = Zeroizing::new(random.to_bytes());
        let mut token = Zeroizing::new(kind.prefix().to_owned());
        for byte in raw.iter() {
            use std::fmt::Write;
            write!(&mut *token, "{byte:02x}").map_err(|_| Error::unavailable())?;
        }
        let cipher = self.cipher()?;
        let envelope = cipher.seal_agent(space, id, kind.as_str(), revision, token.as_bytes())?;
        let row = credentials::ActiveModel {
            space_id: Set(space.bytes()),
            agent_id: Set(id.as_bytes().to_vec()),
            kind: Set(kind.as_str().into()),
            revision: Set(revision),
            key_hash: Set(Sha256::digest(token.as_bytes()).to_vec()),
            format_version: Set(FORMAT_VERSION),
            key_id: Set(cipher.key_id().into()),
            nonce: Set(envelope.nonce),
            ciphertext: Set(envelope.ciphertext),
            updated_at: Set(now()),
        };
        if revision == 1 {
            row.insert(tx).await?;
        } else {
            row.update(tx).await?;
        }
        Ok(())
    }

    pub async fn reveal_agent_key(
        &self,
        space: SpaceId,
        id: Uuid,
        kind: KeyKind,
    ) -> Result<Zeroizing<String>> {
        let row = credentials::Entity::find_by_id((
            space.bytes(),
            id.as_bytes().to_vec(),
            kind.as_str().to_owned(),
        ))
        .one(&self.db)
        .await?
        .ok_or_else(Error::missing)?;
        let plaintext = self.cipher()?.open_agent(
            space,
            id,
            kind.as_str(),
            EncryptedPayload {
                revision: row.revision,
                format: row.format_version,
                key_id: &row.key_id,
                nonce: &row.nonce,
                ciphertext: &row.ciphertext,
            },
        )?;
        if Sha256::digest(plaintext.as_slice()).as_slice() != row.key_hash {
            return Err(Error::unavailable());
        }
        String::from_utf8(plaintext.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| Error::unavailable())
    }

    pub async fn rotate_agent_key(&self, space: SpaceId, id: Uuid, kind: KeyKind) -> Result<()> {
        let tx = self.begin(space).await?;
        let row = credentials::Entity::find_by_id((
            space.bytes(),
            id.as_bytes().to_vec(),
            kind.as_str().to_owned(),
        ))
        .one(&tx)
        .await?
        .ok_or_else(Error::missing)?;
        self.write_agent_key(
            &tx,
            space,
            id,
            kind,
            row.revision.checked_add(1).ok_or_else(Error::unavailable)?,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn authenticate_agent(
        &self,
        space: SpaceId,
        kind: KeyKind,
        token: &str,
    ) -> Result<AgentAccess> {
        if !token.starts_with(kind.prefix())
            || token.len() != kind.prefix().len() + 64
            || !token
                .bytes()
                .skip(kind.prefix().len())
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::agent_unauthorized());
        }
        let hash = Sha256::digest(token.as_bytes()).to_vec();
        let row = credentials::Entity::find()
            .filter(credentials::Column::SpaceId.eq(space.bytes()))
            .filter(credentials::Column::Kind.eq(kind.as_str()))
            .filter(credentials::Column::KeyHash.eq(hash.clone()))
            .one(&self.db)
            .await?
            .ok_or_else(Error::agent_unauthorized)?;
        let agent = Self::agent_row(&self.db, space, uuid_from_bytes(&row.agent_id)?).await?;
        if agent.enabled == 0 {
            return Err(Error::agent_unauthorized());
        }
        Ok(AgentAccess {
            space,
            id: uuid_from_bytes(&agent.id)?,
            kind,
            hash,
            revision: row.revision,
            client_type: agent.client_type,
        })
    }

    pub(crate) async fn validate_agent<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        access: &AgentAccess,
        secret: bool,
    ) -> Result<()> {
        if access.space.as_uuid() != space.as_uuid() || (secret && access.kind != KeyKind::Mcp) {
            return Err(Error::agent_unauthorized());
        }
        let row = Self::agent_row(db, space, access.id).await?;
        let credential = credentials::Entity::find_by_id((
            space.bytes(),
            access.id.as_bytes().to_vec(),
            access.kind.as_str().to_owned(),
        ))
        .one(db)
        .await?
        .ok_or_else(Error::agent_unauthorized)?;
        if row.enabled == 0
            || credential.revision != access.revision
            || credential.key_hash != access.hash
        {
            return Err(Error::agent_unauthorized());
        }
        if secret && row.allow_secrets == 0 {
            return Err(Error {
                code: "secret_access_denied",
                message: "此接入未开启 Secret 存取权限",
                status: axum::http::StatusCode::FORBIDDEN,
            });
        }
        Ok(())
    }

    pub(crate) async fn touch_agent(&self, access: &AgentAccess) -> Result<()> {
        let column = match access.kind {
            KeyKind::Mcp => agents::Column::LastMcpAt,
            KeyKind::Hook => agents::Column::LastHookAt,
        };
        agents::Entity::update_many()
            .col_expr(column, sea_orm::sea_query::Expr::value(now()))
            .filter(agents::Column::SpaceId.eq(access.space.bytes()))
            .filter(agents::Column::Id.eq(access.id.as_bytes().to_vec()))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub(crate) async fn source_label<C: ConnectionTrait>(
        db: &C,
        space: SpaceId,
        id: Option<&[u8]>,
    ) -> Result<Value> {
        let Some(bytes) = id else {
            return Ok(json!({"agent_id":null,"agent_name":"历史记录"}));
        };
        let row = Self::agent_row(db, space, uuid_from_bytes(bytes)?).await?;
        Ok(
            json!({"agent_id":uuid_from_bytes(&row.id)?,"agent_name":row.name,"client_type":row.client_type}),
        )
    }
}
