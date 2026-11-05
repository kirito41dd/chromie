//! 迁移仅供 Web 登录后再次验密使用，Agent 凭据不能请求迁移包。
use crate::{
    auth::Auth,
    error::{Error, Result},
    store::Store,
    transfer,
    transfer_unlock::TransferUnlocks,
    types::SpaceId,
    web_auth::{WebCredentialVersion, WebSessionBinding},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default)]
    space_password: Option<String>,
    passphrase: String,
}
async fn verify(
    auth: &Auth,
    space: SpaceId,
    cookie: &WebCredentialVersion,
    session: WebSessionBinding,
    unlocks: &TransferUnlocks,
    password: Option<String>,
) -> Result<()> {
    let Some(password) = password else {
        return unlocks.require(session, &cookie.0);
    };
    let verified = auth.verify_login(space.as_uuid(), password).await?;
    if verified.version != cookie.0 {
        return Err(Error::unauthorized());
    }
    Ok(())
}
async fn export(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(cookie): Extension<WebCredentialVersion>,
    Extension(auth): Extension<Auth>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(unlocks): Extension<TransferUnlocks>,
    input: std::result::Result<Json<Export>, axum::extract::rejection::JsonRejection>,
) -> Result<Response> {
    let Json(input) = input.map_err(|_| Error::invalid("炼化参数格式无效"))?;
    transfer::passphrase(&input.passphrase)?;
    verify(
        &auth,
        space,
        &cookie,
        session,
        &unlocks,
        input.space_password,
    )
    .await?;
    let permit = transfer::acquire().await?;
    let bundle = store.export_bundle(space, &cookie.0).await?;
    let filename = format!(
        "chromie-{}-{}.chromie",
        bundle.source_space, bundle.package_id
    );
    let (bytes, _permit) = tokio::task::spawn_blocking(move || {
        transfer::encrypt(bundle, input.passphrase).map(|bytes| (bytes, permit))
    })
    .await
    .map_err(|_| Error::unavailable())??;
    if auth.current_version(space).await? != cookie.0 {
        return Err(Error::unauthorized());
    }
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    .map_err(|_| Error::unavailable())?,
            ),
        ],
        bytes,
    )
        .into_response())
}
struct Upload {
    password: Option<String>,
    phrase: String,
    bytes: Vec<u8>,
    digest: Option<String>,
    page: transfer::PreviewPage,
}
async fn upload(mut body: Multipart) -> Result<Upload> {
    let mut password = None;
    let mut phrase = None;
    let mut bytes = None;
    let mut digest = None;
    let mut page = None;
    while let Some(mut field) = body
        .next_field()
        .await
        .map_err(|_| Error::invalid("迁移文件上传不完整"))?
    {
        let name = field.name().unwrap_or("").to_owned();
        let limit = if name == "package" {
            transfer::MAX_PACKAGE
        } else {
            1024
        };
        let mut value = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| Error::invalid("迁移文件上传不完整"))?
        {
            if value.len().saturating_add(chunk.len()) > limit {
                return Err(Error::invalid("迁移文件或参数超过上限"));
            }
            value.extend_from_slice(&chunk);
        }
        if name == "package" {
            if bytes.replace(value).is_some() {
                return Err(Error::invalid("重复迁移文件"));
            }
            continue;
        }
        let value = String::from_utf8(value).map_err(|_| Error::invalid("迁移参数编码无效"))?;
        let target = match name.as_str() {
            "space_password" => &mut password,
            "passphrase" => &mut phrase,
            "preview_digest" => &mut digest,
            "page" => &mut page,
            _ => return Err(Error::invalid("未知迁移参数")),
        };
        if target.replace(value).is_some() {
            return Err(Error::invalid("重复迁移参数"));
        }
    }
    Ok(Upload {
        password,
        phrase: phrase.ok_or_else(|| Error::invalid("缺少迁移口令"))?,
        bytes: bytes.ok_or_else(|| Error::invalid("未选择迁移文件"))?,
        digest,
        page: page
            .map(|p| transfer::PreviewPage::parse(&p))
            .transpose()?
            .unwrap_or_default(),
    })
}
// 许可随阻塞解密任务移动，HTTP 取消不会提前释放 KDF 的资源额度。
async fn open(
    auth: &Auth,
    space: SpaceId,
    cookie: &WebCredentialVersion,
    session: WebSessionBinding,
    unlocks: &TransferUnlocks,
    body: Multipart,
) -> Result<(
    transfer::Bundle,
    OwnedSemaphorePermit,
    Option<String>,
    transfer::PreviewPage,
)> {
    let permit = transfer::acquire().await?;
    let input = tokio::time::timeout(std::time::Duration::from_secs(120), upload(body))
        .await
        .map_err(|_| Error::invalid("迁移文件上传超时，请重新选择文件"))??;
    verify(auth, space, cookie, session, unlocks, input.password).await?;
    let (bundle, permit) = tokio::task::spawn_blocking(move || {
        transfer::decrypt(input.bytes, input.phrase).map(|bundle| (bundle, permit))
    })
    .await
    .map_err(|_| Error::unavailable())??;
    Ok((bundle, permit, input.digest, input.page))
}
async fn preview(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(cookie): Extension<WebCredentialVersion>,
    Extension(auth): Extension<Auth>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(unlocks): Extension<TransferUnlocks>,
    body: Multipart,
) -> Result<Json<Value>> {
    let (bundle, _permit, digest, page) =
        open(&auth, space, &cookie, session, &unlocks, body).await?;
    Ok(Json(
        store
            .preview_transfer(space, &cookie.0, bundle, page, digest.as_deref())
            .await?,
    ))
}
async fn confirm(
    State(store): State<Store>,
    Extension(space): Extension<SpaceId>,
    Extension(cookie): Extension<WebCredentialVersion>,
    Extension(auth): Extension<Auth>,
    Extension(session): Extension<WebSessionBinding>,
    Extension(unlocks): Extension<TransferUnlocks>,
    body: Multipart,
) -> Result<Json<Value>> {
    let (bundle, _permit, digest, _) = open(&auth, space, &cookie, session, &unlocks, body).await?;
    Ok(Json(
        store
            .import_transfer(space, &cookie.0, bundle, digest.as_deref().unwrap_or(""))
            .await?,
    ))
}
pub(crate) fn routes(prefix: &str, auth: Auth, unlocks: TransferUnlocks) -> Router<Store> {
    Router::new()
        .route(
            &format!("{prefix}/transfer/export"),
            post(export).layer(DefaultBodyLimit::max(4096)),
        )
        .route(&format!("{prefix}/transfer/preview"), post(preview))
        .route(&format!("{prefix}/transfer/confirm"), post(confirm))
        .layer(DefaultBodyLimit::max(transfer::MAX_PACKAGE + 8192))
        .layer(Extension(auth))
        .layer(Extension(unlocks))
}
