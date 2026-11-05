//! 系统设置只挂到 Web Cookie 鉴权路由，Agent 密钥不能取得管理能力。
use crate::{
    embedding::{Change, Save},
    error::{Error, Result},
    store::Store,
    types::SpaceId,
    web_auth::WebCredentialVersion,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::{get, post},
};
use serde_json::Value;

async fn config_response(store: &Store, space: SpaceId, mut value: Value) -> Result<Json<Value>> {
    value["usage"] = store
        .model_usage(space)
        .await
        .unwrap_or_else(|_| serde_json::json!({"available":false}));
    Ok(Json(value))
}
async fn read(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(_cookie): Extension<WebCredentialVersion>,
) -> Result<Json<Value>> {
    config_response(&store, space, store.model_settings(space).await?).await
}
async fn save(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(_cookie): Extension<WebCredentialVersion>,
    input: std::result::Result<Json<Save>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) =
        input.map_err(|_| Error::invalid("模型配置 JSON 格式或大小无效，未返回提交内容"))?;
    config_response(
        &store,
        space,
        store.save_model_settings(space, input).await?,
    )
    .await
}
async fn test_connection(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(_cookie): Extension<WebCredentialVersion>,
    input: std::result::Result<Json<Save>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) =
        input.map_err(|_| Error::invalid("模型配置 JSON 格式或大小无效，未返回提交内容"))?;
    Ok(Json(store.test_model_connection(space, input).await?))
}
async fn disable(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(_cookie): Extension<WebCredentialVersion>,
    Json(input): Json<Change>,
) -> Result<Json<Value>> {
    config_response(&store, space, store.disable_embedding(space, input).await?).await
}
async fn retry(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(_cookie): Extension<WebCredentialVersion>,
    Json(input): Json<Change>,
) -> Result<Json<Value>> {
    config_response(&store, space, store.retry_embedding(space, input).await?).await
}
pub(crate) fn routes(prefix: &str) -> Router<Store> {
    Router::new()
        .route(&format!("{prefix}/system/model"), get(read).post(save))
        .route(
            &format!("{prefix}/system/model/test"),
            post(test_connection),
        )
        .route(&format!("{prefix}/system/model/disable"), post(disable))
        .route(&format!("{prefix}/system/model/retry"), post(retry))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
