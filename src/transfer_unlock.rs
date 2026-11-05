//! 迁移验密只提升当前 Web 登录会话，固定十分钟；不保留密码或原始 Cookie。
use crate::{
    auth::Auth,
    error::{Error, Result},
    store::Store,
    types::SpaceId,
    web_auth::{WebCredentialVersion, WebSessionBinding},
};
use axum::{
    Extension, Json, Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const UNLOCK_SECONDS: u64 = 600;

struct Grant {
    credential: String,
    expires: Instant,
}
struct Slot {
    ticket: Option<uuid::Uuid>,
    grant: Option<Grant>,
    retain_until: Instant,
}

/// 每个空间的路由持有一份，键只来自已验证的登录 Cookie 指纹。
/// 查询不续期，锁定与退出真正移除服务端授权，进程重启自动回到锁定态。
#[derive(Clone, Default)]
pub(crate) struct TransferUnlocks(Arc<Mutex<HashMap<WebSessionBinding, Slot>>>);

impl TransferUnlocks {
    // 先登记一次性验密票据再等待哈希。撤销移除票据，后来重新解锁也不复用旧票据，
    // 防止锁定／退出完成后，在途验密结果重新授予旧会话权限。
    fn begin_at(&self, session: WebSessionBinding, now: Instant) -> Result<uuid::Uuid> {
        let mut grants = self.0.lock().map_err(|_| Error::unavailable())?;
        grants.retain(|_, slot| slot.retain_until > now);
        let ticket = uuid::Uuid::now_v7();
        let slot = grants.entry(session).or_insert(Slot {
            ticket: None,
            grant: None,
            retain_until: now,
        });
        slot.ticket = Some(ticket);
        slot.retain_until = now + Duration::from_secs(UNLOCK_SECONDS);
        Ok(ticket)
    }
    fn issue_at(
        &self,
        session: WebSessionBinding,
        ticket: uuid::Uuid,
        credential: String,
        now: Instant,
    ) -> Result<()> {
        let mut grants = self.0.lock().map_err(|_| Error::unavailable())?;
        grants.retain(|_, slot| slot.retain_until > now);
        let slot = grants
            .get_mut(&session)
            .filter(|s| s.ticket == Some(ticket))
            .ok_or(Error {
                code: "transfer_unlock_cancelled",
                message: "解锁请求已取消，请重新操作",
                status: reqwest::StatusCode::CONFLICT,
            })?;
        slot.ticket = None;
        slot.grant = Some(Grant {
            credential,
            expires: now + Duration::from_secs(UNLOCK_SECONDS),
        });
        slot.retain_until = now + Duration::from_secs(UNLOCK_SECONDS);
        Ok(())
    }
    fn remaining_at(
        &self,
        session: WebSessionBinding,
        credential: &str,
        now: Instant,
    ) -> Result<u64> {
        let mut grants = self.0.lock().map_err(|_| Error::unavailable())?;
        grants.retain(|_, slot| slot.retain_until > now);
        Ok(grants
            .get(&session)
            .and_then(|s| s.grant.as_ref())
            .filter(|g| g.credential == credential && g.expires > now)
            .map_or(0, |g| {
                let remaining = g.expires.duration_since(now);
                remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
            }))
    }
    pub(crate) fn require(&self, session: WebSessionBinding, credential: &str) -> Result<()> {
        if self.remaining_at(session, credential, Instant::now())? == 0 {
            return Err(Error {
                code: "transfer_locked",
                message: "请先解锁当前空间的迁移操作",
                status: reqwest::StatusCode::FORBIDDEN,
            });
        }
        Ok(())
    }
    pub(crate) fn revoke(&self, session: WebSessionBinding) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| Error::unavailable())?
            .remove(&session);
        Ok(())
    }
    fn status(&self, session: WebSessionBinding, credential: &str) -> Result<Status> {
        let expires_in = self.remaining_at(session, credential, Instant::now())?;
        Ok(Status {
            unlocked: expires_in > 0,
            expires_in,
        })
    }
}

#[derive(Serialize)]
struct Status {
    unlocked: bool,
    expires_in: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unlock {
    space_password: String,
}

async fn status(
    Extension(unlocks): Extension<TransferUnlocks>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(version): Extension<WebCredentialVersion>,
) -> Result<Json<Status>> {
    Ok(Json(unlocks.status(session, &version.0)?))
}
async fn unlock(
    Extension(auth): Extension<Auth>,
    Extension(space): Extension<SpaceId>,
    Extension(unlocks): Extension<TransferUnlocks>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(version): Extension<WebCredentialVersion>,
    input: std::result::Result<Json<Unlock>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Status>> {
    let Json(input) = input.map_err(|_| Error::invalid("解锁参数格式无效"))?;
    let ticket = unlocks.begin_at(session, Instant::now())?;
    let verified = auth
        .verify_login(space.as_uuid(), input.space_password)
        .await
        .map_err(|error| {
            if error.code == "unauthorized" {
                Error {
                    code: "transfer_password_invalid",
                    message: "空间密码有误，请重新输入",
                    status: reqwest::StatusCode::UNAUTHORIZED,
                }
            } else {
                error
            }
        })?;
    if verified.version != version.0 {
        return Err(Error::unauthorized());
    }
    unlocks.issue_at(session, ticket, version.0.clone(), Instant::now())?;
    Ok(Json(unlocks.status(session, &version.0)?))
}
async fn lock(
    Extension(unlocks): Extension<TransferUnlocks>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(version): Extension<WebCredentialVersion>,
) -> Result<Json<Status>> {
    unlocks.revoke(session)?;
    Ok(Json(unlocks.status(session, &version.0)?))
}
pub(crate) fn routes(prefix: &str, auth: Auth, unlocks: TransferUnlocks) -> Router<Store> {
    Router::new()
        .route(
            &format!("{prefix}/transfer/unlock"),
            get(status).post(unlock),
        )
        .route(&format!("{prefix}/transfer/lock"), post(lock))
        .layer(DefaultBodyLimit::max(4096))
        .layer(Extension(auth))
        .layer(Extension(unlocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revocation_cannot_be_undone_by_an_inflight_password_verification() {
        let unlocks = TransferUnlocks::default();
        let session = WebSessionBinding([9; 32]);
        let now = Instant::now();
        // 验密尚在等待哈希时，另一个请求已经完成锁定／退出。
        let ticket = unlocks.begin_at(session, now).unwrap();
        unlocks.revoke(session).unwrap();
        assert!(
            unlocks
                .issue_at(session, ticket, "version-a".into(), now)
                .is_err()
        );
        assert_eq!(
            unlocks.remaining_at(session, "version-a", now).unwrap(),
            0,
            "撤销后迟到的验密不能重授权限"
        );
        let next = unlocks.begin_at(session, now).unwrap();
        assert!(
            unlocks
                .issue_at(session, ticket, "version-a".into(), now)
                .is_err(),
            "后来重新解锁也不能复活先前的票据"
        );
        unlocks
            .issue_at(session, next, "version-a".into(), now)
            .unwrap();
        assert_eq!(
            unlocks.remaining_at(session, "version-a", now).unwrap(),
            600
        );
        assert!(
            unlocks
                .issue_at(session, next, "version-a".into(), now)
                .is_err(),
            "同一验密结果不能重复颁发或续期"
        );
    }
    #[test]
    fn unlock_is_fixed_duration_session_bound_version_bound_and_revocable() {
        let unlocks = TransferUnlocks::default();
        let session = WebSessionBinding([1; 32]);
        let now = Instant::now();
        let ticket = unlocks.begin_at(session, now).unwrap();
        unlocks
            .issue_at(session, ticket, "version-a".into(), now)
            .unwrap();
        assert_eq!(
            unlocks.remaining_at(session, "version-a", now).unwrap(),
            600
        );
        assert_eq!(
            unlocks
                .remaining_at(session, "version-a", now + Duration::from_secs(599))
                .unwrap(),
            1
        );
        assert_eq!(
            unlocks
                .remaining_at(WebSessionBinding([2; 32]), "version-a", now)
                .unwrap(),
            0
        );
        assert_eq!(unlocks.remaining_at(session, "version-b", now).unwrap(), 0);
        assert_eq!(
            unlocks
                .remaining_at(session, "version-a", now + Duration::from_secs(600))
                .unwrap(),
            0
        );
        let ticket = unlocks.begin_at(session, now).unwrap();
        unlocks
            .issue_at(session, ticket, "version-a".into(), now)
            .unwrap();
        unlocks.revoke(session).unwrap();
        assert_eq!(unlocks.remaining_at(session, "version-a", now).unwrap(), 0);
    }
}
