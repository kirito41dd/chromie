//! 客户端文件交换与发布接口。接入鉴权和 Origin 检查由空间路由统一处理。
use crate::{
    agent::AgentAccess,
    error::{Error, Result},
    skill_package::{MAX_BYTES, PublishSkill, RollbackSkill},
    store::Store,
    types::SpaceId,
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderName, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub(crate) fn routes(prefix: &str) -> Router<Store> {
    let base = format!("{prefix}/skills");
    Router::new()
        .route(
            &format!("{base}/objects"),
            post(upload).layer(DefaultBodyLimit::max(MAX_BYTES)),
        )
        .route(&format!("{base}/publish"), post(publish))
        .route(&format!("{base}/rollback"), post(rollback))
        .route(&format!("{base}/receipts/{{id}}"), get(receipt))
        .route(&format!("{base}/lookup"), get(lookup))
        .route(&format!("{base}/{{id}}"), get(manifest))
        .route(&format!("{base}/{{id}}/history"), get(history))
        .route(&format!("{base}/{{id}}/file"), get(file))
}

async fn upload(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    body: Bytes,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_upload(space, &body).await?))
}
async fn publish(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Json(input): Json<PublishSkill>,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_publish(space, input).await?))
}
async fn rollback(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Json(input): Json<RollbackSkill>,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_rollback(space, input).await?))
}
async fn receipt(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_receipt(space, id).await?))
}
#[derive(Deserialize)]
struct Lookup {
    name: String,
}
async fn lookup(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Query(q): Query<Lookup>,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_lookup(space, &q.name).await?))
}
async fn manifest(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    Ok(Json(s.for_agent(a).skill_manifest(space, id).await?))
}
#[derive(Deserialize)]
struct FileQuery {
    path: String,
    revision: i32,
}
async fn file(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Path(id): Path<Uuid>,
    Query(q): Query<FileQuery>,
) -> Result<FileResponse> {
    file_response(&s.for_agent(a), space, id, q).await
}

type FileResponse = ([(HeaderName, &'static str); 4], Vec<u8>);

async fn file_response(
    store: &Store,
    space: SpaceId,
    id: Uuid,
    q: FileQuery,
) -> Result<FileResponse> {
    if q.revision < 1 {
        return Err(Error::invalid("修订号无效"));
    }
    let body = store
        .skill_file(space, id, &q.path, Some(q.revision))
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    ))
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<i32>,
}
async fn history(
    State(s): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(a): Extension<AgentAccess>,
    Path(id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        s.for_agent(a).skill_history(space, id, q.before).await?,
    ))
}

// 浏览器入口挂在空间登录 Cookie 的路由组，只提供人类阅读所需的信息。
pub(crate) fn web_routes(prefix: &str) -> Router<Store> {
    Router::new()
        .route(&format!("{prefix}/skill-library"), get(library))
        .route(
            &format!("{prefix}/skill-library/{{id}}"),
            get(library_entry),
        )
        .route(
            &format!("{prefix}/skill-library/{{id}}/file"),
            get(library_file),
        )
        .route(
            &format!("{prefix}/skill-library/{{id}}/archive"),
            get(library_archive),
        )
}

#[derive(Deserialize)]
struct LibraryQuery {
    after: Option<Uuid>,
}

async fn library(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<Value>> {
    Ok(Json(store.skill_library(space, query.after).await?))
}

async fn library_entry(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let manifest = store.skill_manifest(space, id).await?;
    Ok(Json(serde_json::json!({
        "revision": manifest["revision"],
        "files": manifest["files"],
    })))
}

async fn library_file(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path(id): Path<Uuid>,
    Query(query): Query<FileQuery>,
) -> Result<FileResponse> {
    file_response(&store, space, id, query).await
}

#[derive(Deserialize)]
struct ArchiveQuery {
    revision: i32,
}

async fn library_archive(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Path(id): Path<Uuid>,
    Query(query): Query<ArchiveQuery>,
) -> Result<Response> {
    if query.revision < 1 {
        return Err(Error::invalid("修订号无效"));
    }
    let (name, archive) = store.skill_archive(space, id, query.revision).await?;
    let disposition = format!(r#"attachment; filename="{name}.zip""#);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_DISPOSITION, disposition.as_str()),
        ],
        archive,
    )
        .into_response())
}
