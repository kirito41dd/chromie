use crate::{
    auth::Auth,
    error::{Error, Result},
    store::Store,
    types::SpaceId,
    web_session::{SESSION_SECONDS, WebSessions},
};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cookie::{Cookie, SameSite};
use sha2::{Digest, Sha256};

const COOKIE_NAME: &str = "chromie_session";

#[derive(Clone)]
pub(crate) struct WebCredentialVersion(pub String);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WebSessionBinding(pub(crate) [u8; 32]);

#[derive(Clone)]
struct Guard {
    auth: Auth,
    space: SpaceId,
    origin: Option<String>,
    path: String,
    sessions: WebSessions,
    transfer_unlocks: crate::transfer_unlock::TransferUnlocks,
}

pub(crate) fn check_origin(headers: &HeaderMap, configured: Option<&str>) -> Result<()> {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site")
    {
        return Err(Error::unauthorized());
    }
    let expected = if let Some(origin) = configured {
        origin.to_owned()
    } else {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(Error::unauthorized)?;
        let url =
            reqwest::Url::parse(&format!("http://{host}")).map_err(|_| Error::unauthorized())?;
        if !matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        ) || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::unauthorized());
        }
        url.origin().ascii_serialization()
    };
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.to_str().map_err(|_| Error::unauthorized())? != expected
    {
        return Err(Error::unauthorized());
    }
    Ok(())
}

pub(crate) fn validate_public_origin(origin: &str) -> Result<()> {
    let url = reqwest::Url::parse(origin).map_err(|_| Error::invalid("公开 Origin 格式无效"))?;
    if !crate::transport::allows(&url)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.origin().ascii_serialization() != origin
    {
        return Err(Error::invalid(
            "Origin 须使用 HTTP 或 HTTPS，且不含路径或凭据",
        ));
    }
    Ok(())
}

pub(crate) fn password(headers: &HeaderMap) -> Result<String> {
    if headers.get_all("x-chromie-password").iter().count() != 1 {
        return Err(Error::unauthorized());
    }
    headers
        .get("x-chromie-password")
        .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
        .map(str::to_owned)
        .ok_or_else(Error::unauthorized)
}

fn token(headers: &HeaderMap) -> Result<String> {
    let mut token = None;
    for value in headers.get_all(header::COOKIE) {
        for entry in Cookie::split_parse(value.to_str().map_err(|_| Error::unauthorized())?) {
            let entry = entry.map_err(|_| Error::unauthorized())?;
            if entry.name() == COOKIE_NAME {
                if token.is_some() {
                    return Err(Error::unauthorized());
                }
                token = Some(entry.value().to_owned());
            }
        }
    }
    token.ok_or_else(Error::unauthorized)
}

fn cookie_response(guard: &Guard, token: String, remove: bool) -> Result<Response> {
    let lifetime = if remove {
        cookie::time::Duration::ZERO
    } else {
        cookie::time::Duration::seconds(SESSION_SECONDS as i64)
    };
    let expires = if remove {
        cookie::time::OffsetDateTime::UNIX_EPOCH
    } else {
        cookie::time::OffsetDateTime::now_utc() + lifetime
    };
    let cookie = Cookie::build((COOKIE_NAME, token))
        .path(guard.path.clone())
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(
            guard
                .origin
                .as_deref()
                .is_some_and(|origin| origin.starts_with("https://")),
        )
        .max_age(lifetime)
        .expires(expires)
        .build();
    let header = HeaderValue::from_str(&cookie.to_string()).map_err(|_| Error::unavailable())?;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, header)]).into_response())
}

async fn login(State(guard): State<Guard>, headers: HeaderMap) -> Result<Response> {
    check_origin(&headers, guard.origin.as_deref())?;
    let verified = guard
        .auth
        .verify_login(guard.space.as_uuid(), password(&headers)?)
        .await?;
    cookie_response(&guard, guard.sessions.issue(&verified)?, false)
}

async fn logout(State(guard): State<Guard>, headers: HeaderMap) -> Result<Response> {
    check_origin(&headers, guard.origin.as_deref())?;
    if let Ok(token) = token(&headers) {
        guard
            .transfer_unlocks
            .revoke(WebSessionBinding(Sha256::digest(token.as_bytes()).into()))?;
    }
    cookie_response(&guard, String::new(), true)
}

async fn authenticate(
    State(guard): State<Guard>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    check_origin(request.headers(), guard.origin.as_deref())?;
    let cookie_only = matches!(
        request.uri().path().split('/').nth(3),
        Some("agents" | "messages" | "system" | "transfer")
    );
    let space = if !cookie_only && request.headers().contains_key("x-chromie-password") {
        guard
            .auth
            .verify(guard.space.as_uuid(), password(request.headers())?)
            .await?
    } else {
        let token = token(request.headers())?;
        let version = guard.sessions.verify(&token, guard.space)?;
        if guard.auth.current_version(guard.space).await? != version {
            return Err(Error::unauthorized());
        }
        request
            .extensions_mut()
            .insert(WebCredentialVersion(version));
        request
            .extensions_mut()
            .insert(WebSessionBinding(Sha256::digest(token.as_bytes()).into()));
        guard.space
    };
    request.extensions_mut().insert(space);
    Ok(next.run(request).await)
}

pub(crate) fn routes(
    store: Store,
    auth: Auth,
    space: SpaceId,
    slug: &str,
    origin: Option<String>,
    sessions: WebSessions,
) -> Router {
    let guard = Guard {
        auth,
        space,
        origin,
        path: format!("/spaces/{slug}/"),
        sessions,
        transfer_unlocks: Default::default(),
    };
    let prefix = format!("/spaces/{slug}");
    let (pairing, pairing_public) = crate::pairing::routes(
        &prefix,
        store.clone(),
        guard.auth.clone(),
        space,
        guard.origin.clone(),
    );
    let data = Router::new()
        .merge(crate::system_http::routes(&prefix))
        .merge(crate::transfer_http::routes(
            &prefix,
            guard.auth.clone(),
            guard.transfer_unlocks.clone(),
        ))
        .merge(crate::transfer_unlock::routes(
            &prefix,
            guard.auth.clone(),
            guard.transfer_unlocks.clone(),
        ))
        .merge(pairing)
        .merge(crate::agent_http::routes(&prefix))
        .route(
            &format!("{prefix}/messages/sessions"),
            post(crate::messages::sessions).layer(DefaultBodyLimit::max(2048)),
        )
        .route(
            &format!("{prefix}/messages/conversation"),
            post(crate::messages::conversation).layer(DefaultBodyLimit::max(2048)),
        )
        .route(
            &format!("{prefix}/messages/events/{{id}}"),
            get(crate::messages::detail),
        )
        .route(
            &format!("{prefix}/dashboard-data"),
            get(crate::http::dashboard),
        )
        .route(
            &format!("{prefix}/recall"),
            post(crate::http::recall).layer(DefaultBodyLimit::max(4096)),
        )
        .route_layer(middleware::from_fn_with_state(guard.clone(), authenticate))
        .with_state(store);
    let session = Router::new()
        .route(&format!("{prefix}/login"), post(login))
        .route(&format!("{prefix}/logout"), post(logout))
        .with_state(guard);
    data.merge(session)
        .merge(pairing_public)
        .layer(middleware::from_fn(crate::web::no_store))
}
