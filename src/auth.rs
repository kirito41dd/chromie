use crate::{
    entity::spaces,
    error::{Error, Result},
    types::SpaceId,
};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use sea_orm::{DatabaseConnection, EntityTrait};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

#[derive(Clone)]
pub struct Auth {
    db: DatabaseConnection,
    slots: Arc<Semaphore>,
    rate: Arc<Mutex<(std::time::Instant, f64)>>,
}

pub(crate) struct VerifiedPassword {
    pub space: SpaceId,
    pub version: String,
}

pub(crate) fn password_version(hash: &str) -> String {
    Sha256::digest(hash.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Auth {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db,
            slots: Arc::new(Semaphore::new(2)),
            rate: Arc::new(Mutex::new((std::time::Instant::now(), 32.0))),
        }
    }
    pub async fn verify(&self, space: Uuid, password: String) -> Result<SpaceId> {
        Ok(self.verify_login(space, password).await?.space)
    }

    pub(crate) async fn current_version(&self, space: SpaceId) -> Result<String> {
        let row = spaces::Entity::find_by_id(space.bytes())
            .one(&self.db)
            .await?
            .ok_or_else(Error::unauthorized)?;
        Ok(password_version(&row.password_hash))
    }

    pub(crate) async fn verify_login(
        &self,
        space: Uuid,
        password: String,
    ) -> Result<VerifiedPassword> {
        if password.len() > 1024 {
            return Err(Error::unauthorized());
        }
        {
            let mut rate = self.rate.lock().await;
            rate.1 = (rate.1 + rate.0.elapsed().as_secs_f64() * 8.0).min(32.0);
            rate.0 = std::time::Instant::now();
            if rate.1 < 1.0 {
                return Err(Error::limited());
            }
            rate.1 -= 1.0;
        }
        // Agent 常同时发起数次检索；限时排队，仍只允许两个慢哈希并行。
        // 入队前的令牌桶与两秒超时共同限制等待请求，不无限堆积验密任务。
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.slots.clone().acquire_owned(),
        )
        .await
        .map_err(|_| Error::unavailable())?
        .map_err(|_| Error::unavailable())?;
        let hash = spaces::Entity::find_by_id(space.as_bytes().to_vec())
            .one(&self.db)
            .await?
            .ok_or_else(Error::unauthorized)?
            .password_hash;
        // 签发绑定本次真正验证的哈希版本，不能在验密后再取一次新版本。
        let version = password_version(&hash);
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            PasswordHash::new(&hash).ok().is_some_and(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            })
        })
        .await
        .map_err(|_| Error::unavailable())?;
        if !valid {
            return Err(Error::unauthorized());
        }
        Ok(VerifiedPassword {
            space: SpaceId(space),
            version,
        })
    }
}

pub async fn hash_password(password: String) -> Result<String> {
    if password.chars().any(char::is_control) {
        return Err(Error::invalid("空间密码不能包含控制字符"));
    }
    if password.len() < 12 || password.len() > 1024 {
        return Err(Error::invalid("空间密码长度须为 12 至 1024 字节"));
    }
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|_| Error::unavailable())
    })
    .await
    .map_err(|_| Error::unavailable())?
}
