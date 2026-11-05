use crate::{
    agent::{CreateAgent, KeyKind, UpdateAgent},
    error::{Error, Result},
    store::Store,
    types::SpaceId,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    routing::{get, patch, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

async fn list(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
) -> Result<Json<Value>> {
    Ok(Json(json!({"agents":store.list_agents(space).await?})))
}

async fn create(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    body: std::result::Result<Json<CreateAgent>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = body.map_err(|_| Error::invalid("接入配置格式无效"))?;
    Ok(Json(
        json!({"agent":store.create_agent(space, input).await?}),
    ))
}

async fn update(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path(id): Path<Uuid>,
    body: std::result::Result<Json<UpdateAgent>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = body.map_err(|_| Error::invalid("接入配置格式无效"))?;
    Ok(Json(
        json!({"agent":store.update_agent(space, id, input).await?}),
    ))
}

async fn reveal(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path((id, kind)): Path<(Uuid, String)>,
) -> Result<Json<Value>> {
    let key = store
        .reveal_agent_key(space, id, KeyKind::parse(&kind)?)
        .await?;
    Ok(Json(json!({"key":key.as_str()})))
}

async fn rotate(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path((id, kind)): Path<(Uuid, String)>,
) -> Result<Json<Value>> {
    store
        .rotate_agent_key(space, id, KeyKind::parse(&kind)?)
        .await?;
    Ok(Json(json!({"rotated":true})))
}

pub(crate) fn routes(prefix: &str) -> Router<Store> {
    Router::new()
        .route(&format!("{prefix}/agents"), get(list).post(create))
        .route(&format!("{prefix}/agents/{{id}}"), patch(update))
        .route(
            &format!("{prefix}/agents/{{id}}/keys/{{kind}}/reveal"),
            post(reveal),
        )
        .route(
            &format!("{prefix}/agents/{{id}}/keys/{{kind}}/rotate"),
            post(rotate),
        )
        .layer(DefaultBodyLimit::max(4096))
}
