use crate::{
    agent::{AgentAccess, KeyKind},
    auth::Auth,
    error::{Error, Result},
    mcp::MemoryMcp,
    store::Store,
    types::{Event, Recall, SpaceId, uuid_from_bytes},
    web_session::WebSessions,
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use sea_orm::EntityTrait;
use std::sync::Arc;

#[derive(Clone)]
struct Guard {
    auth: Auth,
    space: SpaceId,
    origin: Option<String>,
    store: Store,
}

async fn authenticate(
    State(guard): State<Guard>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    crate::web_auth::check_origin(request.headers(), guard.origin.as_deref())?;
    let path = request.uri().path();
    let secret_request = path.split('/').nth(3) == Some("secrets");
    let kind = if path.ends_with("/mcp") {
        Some(KeyKind::Mcp)
    } else if path.ends_with("/events") {
        Some(KeyKind::Hook)
    } else if secret_request && request.headers().contains_key(header::AUTHORIZATION) {
        Some(KeyKind::Mcp)
    } else {
        None
    };
    let actor = if let Some(kind) = kind {
        let token = crate::agent::bearer(request.headers())?;
        let access = guard
            .store
            .authenticate_agent(guard.space, kind, token.as_str())
            .await?;
        if secret_request {
            Store::validate_agent(&guard.store.db, guard.space, &access, true).await?;
        }
        if let Some(expected) = request.headers().get("x-chromie-agent-id") {
            let id = expected
                .to_str()
                .ok()
                .and_then(|value| uuid::Uuid::parse_str(value).ok());
            if id != Some(access.agent_id()) {
                return Err(Error::unauthorized());
            }
        }
        request.extensions_mut().insert(access.clone());
        Some(access)
    } else {
        let password = crate::web_auth::password(request.headers())?;
        guard.auth.verify(guard.space.as_uuid(), password).await?;
        None
    };
    request.extensions_mut().insert(guard.space);
    request.extensions_mut().insert(RequestActor(actor.clone()));
    let response = next.run(request).await;
    if response.status().is_success()
        && let Some(actor) = &actor
    {
        let _ = guard.store.touch_agent(actor).await;
    }
    Ok(response)
}

#[derive(Clone)]
struct RequestActor(Option<AgentAccess>);

impl RequestActor {
    fn store(self, store: Store) -> Store {
        match self.0 {
            Some(actor) => store.for_agent(actor),
            None => store,
        }
    }
}

async fn capture(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    payload: std::result::Result<Json<Event>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>> {
    let Json(mut event) = payload.map_err(|_| Error::invalid("采集请求的 JSON 格式或大小无效"))?;
    if let Some(access) = &actor.0 {
        event.client_type = access.client_type.clone();
    }
    Ok(Json(actor.store(store).capture(space, event).await?))
}

pub(crate) async fn dashboard(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    query: std::result::Result<
        Query<crate::dashboard::DashboardQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Result<Json<crate::dashboard::Snapshot>> {
    let Query(query) = query.map_err(|_| Error::invalid("看板排序参数无效"))?;
    Ok(Json(store.dashboard_ordered(space, query.order).await?))
}

pub(crate) async fn recall(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<Recall>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("召回请求的 JSON 格式或大小无效"))?;
    Ok(Json(store.recall_debug(space, input).await?))
}

fn secret_bytes(
    headers: &HeaderMap,
    bytes: std::result::Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<p256::elliptic_curve::zeroize::Zeroizing<Vec<u8>>> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(Error::invalid(
            "Secret 请求须使用 application/octet-stream 原始文件正文",
        ));
    }
    let bytes = bytes.map_err(|_| Error::invalid("Secret 正文读取失败或超过上限"))?;
    Ok(p256::elliptic_curve::zeroize::Zeroizing::new(
        bytes.to_vec(),
    ))
}

async fn secret_create(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    query: std::result::Result<
        Query<crate::types::SecretCreate>,
        axum::extract::rejection::QueryRejection,
    >,
    headers: HeaderMap,
    bytes: std::result::Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Json<serde_json::Value>> {
    let Query(input) = query.map_err(|_| Error::invalid("Secret 公开元数据或请求 ID 无效"))?;
    let plaintext = secret_bytes(&headers, bytes)?;
    Ok(Json(
        actor
            .store(store)
            .create_secret(space, input, &plaintext)
            .await?,
    ))
}

async fn secret_update(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    Path(id): Path<uuid::Uuid>,
    query: std::result::Result<
        Query<crate::types::SecretUpdate>,
        axum::extract::rejection::QueryRejection,
    >,
    headers: HeaderMap,
    bytes: std::result::Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Json<serde_json::Value>> {
    let Query(input) = query.map_err(|_| Error::invalid("Secret 请求 ID 或当前修订无效"))?;
    let plaintext = secret_bytes(&headers, bytes)?;
    Ok(Json(
        actor
            .store(store)
            .update_secret(space, id, input, &plaintext)
            .await?,
    ))
}

async fn secret_read(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    Path(id): Path<uuid::Uuid>,
    query: std::result::Result<
        Query<crate::types::SecretReadQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Result<Response> {
    let Query(input) = query.map_err(|_| Error::invalid("Secret 预期修订无效"))?;
    let read = actor
        .store(store)
        .read_secret_for(space, id, input.expected_revision, input.purpose)
        .await?;
    Ok((
        [
            ("content-type", "application/octet-stream".to_owned()),
            (
                "content-disposition",
                "attachment; filename=chromie-secret".to_owned(),
            ),
            ("x-content-type-options", "nosniff".to_owned()),
            ("x-chromie-revision", read.revision.to_string()),
            ("x-chromie-audit-id", read.audit_id.to_string()),
        ],
        read.bytes.to_vec(),
    )
        .into_response())
}

async fn secret_execution_report(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    Path(audit): Path<uuid::Uuid>,
    payload: std::result::Result<
        Json<crate::secret_audit::ExecutionReport>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Json<serde_json::Value>> {
    let Json(report) = payload.map_err(|_| Error::invalid("执行状态格式无效"))?;
    actor
        .store(store)
        .report_secret_execution(space, audit, report)
        .await?;
    Ok(Json(
        serde_json::json!({"recorded":true,"result_source":"client_reported"}),
    ))
}

async fn secret_contract_update(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(actor): Extension<RequestActor>,
    Path(id): Path<uuid::Uuid>,
    payload: std::result::Result<
        Json<crate::types::SecretContractUpdate>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Json<serde_json::Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("Secret 契约请求 JSON 无效或过大"))?;
    Ok(Json(
        actor
            .store(store)
            .set_secret_contract(space, id, input)
            .await?,
    ))
}

pub async fn router(store: Store, origin: Option<String>) -> Result<Router> {
    build_router(store, origin, None).await
}

pub async fn router_with_sessions(
    store: Store,
    origin: Option<String>,
    sessions: WebSessions,
) -> Result<Router> {
    build_router(store, origin, Some(sessions)).await
}

async fn build_router(
    store: Store,
    origin: Option<String>,
    sessions: Option<WebSessions>,
) -> Result<Router> {
    if let Some(origin) = &origin {
        crate::web_auth::validate_public_origin(origin)?;
    }
    let auth = Auth::new(store.db.clone());
    let readiness = store.clone();
    let mut router = Router::new().route(
        "/health",
        get(|| async { Json(serde_json::json!({"status":"ok"})) }),
    );
    router = router
        .route(
            "/ready",
            get(move || {
                let state = readiness.clone();
                async move {
                    if state.db.ping().await.is_ok() && state.search.available().await {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        .route("/", get(crate::web::page))
        .route("/spaces/{space}/dashboard", get(crate::web::page))
        .route("/spaces/{space}/recall", get(crate::web::page))
        .route("/spaces/{space}/access", get(crate::web::page))
        .route("/spaces/{space}/messages", get(crate::web::page))
        .route("/spaces/{space}/transfer", get(crate::web::page))
        .route("/install.sh", get(crate::web::installer))
        .route(
            "/integrations/cli/{target}/{file}",
            get(crate::web::cli_asset),
        )
        .route("/web/bootstrap.js", get(crate::web::bootstrap))
        .route(
            "/integrations/codex/chromie-codex.zip",
            get(crate::web::codex_plugin),
        )
        .route(
            "/integrations/chromie-client.zip",
            get(crate::web::capture_client),
        )
        .route("/web/chromie_web.js", get(crate::web::javascript))
        .route("/web/chromie_web_bg.wasm", get(crate::web::wasm));
    let spaces = crate::entity::spaces::Entity::find().all(&store.db).await?;
    for row in spaces {
        let space = SpaceId(uuid_from_bytes(&row.id)?);
        let service_store = store.clone();
        let mut config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(1024 * 1024)
            .enforce_origin_validation();
        if let Some(origin) = &origin {
            let parsed =
                reqwest::Url::parse(origin).map_err(|_| Error::invalid("公开 Origin 无效"))?;
            let host = parsed
                .host_str()
                .ok_or_else(|| Error::invalid("公开 Origin 缺少主机名"))?;
            config.allowed_hosts.push(host.to_owned());
            config.allowed_hosts.push(format!(
                "{host}:{}",
                parsed.port_or_known_default().unwrap_or(443)
            ));
            config.allowed_origins.push(origin.clone());
        }
        let service = StreamableHttpService::new(
            move || Ok(MemoryMcp::new(service_store.clone(), space)),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let prefix = format!("/spaces/{}", row.slug);
        // 与同路径的鉴权 POST 合并方法；GET 只提供不含空间数据的登录壳。
        router = router.route(&format!("{prefix}/recall"), get(crate::web::page));
        router = router.route(&format!("{prefix}/settings"), get(crate::web::page));
        let mut routes = Router::new()
            .route(
                &format!("{prefix}/secrets/access-audit/{{audit}}/result"),
                post(secret_execution_report).layer(DefaultBodyLimit::max(1024)),
            )
            .route(
                &format!("{prefix}/secrets/{{id}}/contract"),
                axum::routing::put(secret_contract_update).layer(DefaultBodyLimit::max(8192)),
            )
            .route_service(&format!("{prefix}/mcp"), service)
            .route(&format!("{prefix}/events"), post(capture))
            .route(
                &format!("{prefix}/secrets"),
                post(secret_create).layer(DefaultBodyLimit::max(
                    crate::secret_crypto::MAX_SECRET_BYTES,
                )),
            )
            .route(
                &format!("{prefix}/secrets/{{id}}"),
                get(secret_read)
                    .put(secret_update)
                    .layer(DefaultBodyLimit::max(
                        crate::secret_crypto::MAX_SECRET_BYTES,
                    )),
            );
        if sessions.is_none() {
            routes = routes
                .route(&format!("{prefix}/dashboard-data"), get(dashboard))
                .route(
                    &format!("{prefix}/recall"),
                    post(recall).layer(DefaultBodyLimit::max(4096)),
                );
        }
        let routes = routes
            .route_layer(middleware::from_fn_with_state(
                Guard {
                    auth: auth.clone(),
                    space,
                    origin: origin.clone(),
                    store: store.clone(),
                },
                authenticate,
            ))
            .layer(middleware::from_fn(crate::web::no_store))
            .layer(DefaultBodyLimit::max(1024 * 1024))
            .with_state(store.clone());
        router = router.merge(routes);
        if let Some(sessions) = &sessions {
            router = router.merge(crate::web_auth::routes(
                store.clone(),
                auth.clone(),
                space,
                &row.slug,
                origin.clone(),
                sessions.clone(),
            ));
        }
    }
    Ok(router)
}
