use crate::{
    error::{Error, Result},
    knowledge::{Navigation, Neighbors},
    store::Store,
    types::{GetMemory, SpaceId},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::{get, post},
};
use serde_json::Value;

pub(crate) fn routes(prefix: &str) -> Router<Store> {
    Router::new()
        .route(&format!("{prefix}/knowledge/navigation"), post(navigation))
        .route(&format!("{prefix}/knowledge/links"), post(links))
        .route(&format!("{prefix}/knowledge/memory"), post(memory))
        .route(&format!("{prefix}/knowledge/jobs"), get(jobs).post(enqueue))
        .route(&format!("{prefix}/knowledge/review"), post(review))
        .layer(DefaultBodyLimit::max(4096))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    job_id: uuid::Uuid,
}
async fn review(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<Review>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("提炼结果参数无效"))?;
    Ok(Json(store.knowledge_review(space, input.job_id).await?))
}

async fn jobs(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
) -> Result<Json<Value>> {
    Ok(Json(store.knowledge_jobs(space).await?))
}
async fn enqueue(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<
        Json<crate::text_model::Queue>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Result<Json<Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("整理任务参数无效"))?;
    Ok(Json(store.enqueue_knowledge(space, input).await?))
}

async fn navigation(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<Navigation>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("主题导航参数无效"))?;
    Ok(Json(store.navigation(space, input).await?))
}

async fn links(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<Neighbors>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("引用查询参数无效"))?;
    Ok(Json(store.memory_neighbors(space, input).await?))
}

async fn memory(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    payload: std::result::Result<Json<GetMemory>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = payload.map_err(|_| Error::invalid("记忆阅读参数无效"))?;
    if input.event_id.is_some() || !matches!(input.view, crate::types::View::Current) {
        return Err(Error::invalid("记忆阅读仅支持当前公开正文"));
    }
    Ok(Json(store.get(space, input).await?))
}
