//! 临时 CLI 配对只保存随机凭据的摘要与批准结果，不缓存 Agent 密钥。
use crate::{
    agent::KeyKind,
    auth::Auth,
    error::{Error, Result},
    store::Store,
    types::SpaceId,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::HeaderMap,
    routing::{get, post},
};
use p256::{
    SecretKey,
    elliptic_curve::{Generate, subtle::ConstantTimeEq},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

const TTL: Duration = Duration::from_secs(300);

struct Pending {
    hash: Vec<u8>,
    code: String,
    client: String,
    expires: Instant,
    approved: Option<(Uuid, String)>,
}

#[derive(Clone)]
struct Pairings {
    space: SpaceId,
    auth: Auth,
    store: Store,
    origin: Option<String>,
    entries: Arc<Mutex<HashMap<Uuid, Pending>>>,
    rate: Arc<Mutex<(Instant, u32)>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    client: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    agent_id: Uuid,
    code: String,
}

async fn start(
    State(state): State<Pairings>,
    headers: HeaderMap,
    Json(input): Json<Start>,
) -> Result<Json<Value>> {
    crate::web_auth::check_origin(&headers, state.origin.as_deref())?;
    if !matches!(input.client.as_str(), "codex" | "generic") {
        return Err(Error::invalid("接入客户端须为 codex 或 generic"));
    }
    let mut rate = state.rate.lock().await;
    if rate.0.elapsed() >= Duration::from_secs(60) {
        *rate = (Instant::now(), 0);
    }
    if rate.1 >= 12 {
        return Err(Error::limited());
    }
    rate.1 += 1;
    drop(rate);
    let mut entries = state.entries.lock().await;
    entries.retain(|_, item| item.expires > Instant::now());
    if entries.len() >= 16 {
        return Err(Error::limited());
    }
    let secret = SecretKey::try_generate().map_err(|_| Error::unavailable())?;
    let token: String = secret
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let id = Uuid::now_v7();
    // 确认码不是领取凭据；用户须核对终端显示的码后再批准。
    let code: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(12)
        .collect();
    entries.insert(
        id,
        Pending {
            hash: Sha256::digest(token.as_bytes()).to_vec(),
            code: code.clone(),
            client: input.client,
            expires: Instant::now() + TTL,
            approved: None,
        },
    );
    Ok(Json(
        json!({"pairing_id":id,"device_key":token,"code":code,"expires_in":TTL.as_secs()}),
    ))
}

async fn info(Extension(state): Extension<Pairings>, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    let entries = state.entries.lock().await;
    let item = entries
        .get(&id)
        .filter(|v| v.expires > Instant::now())
        .ok_or_else(Error::missing)?;
    Ok(Json(
        json!({"pairing_id":id,"code":item.code,"client":item.client,"approved":item.approved.is_some(),
            "expires_in":item.expires.saturating_duration_since(Instant::now()).as_secs().saturating_add(1).min(TTL.as_secs()),
            "agent_id":item.approved.as_ref().map(|grant|grant.0)}),
    ))
}

async fn approve(
    Extension(state): Extension<Pairings>,
    Extension(version): Extension<crate::web_auth::WebCredentialVersion>,
    Path(id): Path<Uuid>,
    Json(input): Json<Approval>,
) -> Result<Json<Value>> {
    // 查询限定本空间，禁用接入不能获得新凭据。
    let enabled = state
        .store
        .list_agents(state.space)
        .await?
        .into_iter()
        .any(|v| v.agent_id == input.agent_id && v.enabled);
    if !enabled {
        return Err(Error::missing());
    }
    let mut entries = state.entries.lock().await;
    let item = entries
        .get_mut(&id)
        .filter(|v| v.expires > Instant::now())
        .ok_or_else(Error::missing)?;
    if item.code != input.code {
        return Err(Error::invalid("确认码与当前请求不符，请重新读取接入请求"));
    }
    if item.approved.is_some() {
        return Err(Error::conflict("already_approved"));
    }
    item.approved = Some((input.agent_id, version.0));
    Ok(Json(json!({"approved":true})))
}

async fn poll(
    State(state): State<Pairings>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    crate::web_auth::check_origin(&headers, state.origin.as_deref())?;
    let token = crate::agent::bearer(&headers)?;
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    let approved = {
        let mut entries = state.entries.lock().await;
        let item = entries
            .get(&id)
            .filter(|v| v.expires > Instant::now())
            .ok_or_else(Error::missing)?;
        if !bool::from(item.hash.ct_eq(&hash)) {
            return Err(Error::unauthorized());
        }
        let Some(approval) = item.approved.clone() else {
            return Ok(Json(json!({"status":"pending"})));
        };
        // 先消费再取密钥：并行轮询最多成功一次，网络失败须重新配对。
        entries.remove(&id);
        approval
    };
    if state.auth.current_version(state.space).await? != approved.1 {
        return Err(Error::unauthorized());
    }
    if !state
        .store
        .list_agents(state.space)
        .await?
        .into_iter()
        .any(|v| v.agent_id == approved.0 && v.enabled)
    {
        return Err(Error::unauthorized());
    }
    let mcp = state
        .store
        .reveal_agent_key(state.space, approved.0, KeyKind::Mcp)
        .await?;
    let hook = state
        .store
        .reveal_agent_key(state.space, approved.0, KeyKind::Hook)
        .await?;
    Ok(Json(
        json!({"status":"ready","agent_id":approved.0,"mcp_key":mcp.as_str(),"hook_key":hook.as_str()}),
    ))
}

pub(crate) fn routes(
    prefix: &str,
    store: Store,
    auth: Auth,
    space: SpaceId,
    origin: Option<String>,
) -> (Router<Store>, Router) {
    let state = Pairings {
        space,
        auth,
        store,
        origin,
        entries: Arc::default(),
        rate: Arc::new(Mutex::new((Instant::now(), 0))),
    };
    let protected = Router::new()
        .route(
            &format!("{prefix}/agents/pairings/{{id}}"),
            get(info).post(approve),
        )
        .layer(Extension(state.clone()))
        .layer(DefaultBodyLimit::max(1024));
    let public = Router::new()
        .route(&format!("{prefix}/cli/pairings"), post(start))
        .route(&format!("{prefix}/cli/pairings/{{id}}"), post(poll))
        .layer(DefaultBodyLimit::max(1024))
        .with_state(state);
    (protected, public)
}
