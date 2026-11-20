//! 原生扩展和过渡工具读取同一份已鉴权清单。URI 必须匹配服务端生成的标准路径。
use crate::{
    error::{Error, Result},
    store::Store,
    types::SpaceId,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    ErrorData,
    model::{
        CacheScope, CustomRequest, CustomResult, ErrorCode, ReadResourceResponse,
        ReadResourceResult, ResourceContents,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) fn protocol_error(e: Error) -> ErrorData {
    match e.status.as_u16() {
        404 => ErrorData::new(ErrorCode(-32002), "当前空间中没有可访问的 Skill 资源", None),
        400 | 409 => ErrorData::invalid_params(e.message, None),
        _ => ErrorData::internal_error(e.message, None),
    }
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(Error::unavailable)
}

fn uri(root: &reqwest::Url, path: &str) -> Result<String> {
    let mut u = root.clone();
    u.path_segments_mut()
        .map_err(|_| Error::unavailable())?
        .extend(path.split('/'));
    Ok(u.into())
}

fn root(manifest: &Value) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse("skill://chromie").map_err(|_| Error::unavailable())?;
    url.path_segments_mut()
        .map_err(|_| Error::unavailable())?
        .extend([
            field(manifest, "skill_id")?,
            field(&manifest["frontmatter"], "name")?,
        ]);
    Ok(url)
}

fn entry(manifest: &Value) -> Result<Value> {
    let root = root(manifest)?;
    let mut files = Vec::new();
    for file in manifest["files"]
        .as_array()
        .ok_or_else(Error::unavailable)?
    {
        files.push(json!({"uri":uri(&root,field(file,"path")?)?,"digest":file["digest"],"size":file["size"]}));
    }
    Ok(
        json!({"uri":uri(&root,"SKILL.md")?,"frontmatter":manifest["frontmatter"],"resources":files,
        "_meta":{"com.github.kirito41dd.chromie/revision":manifest["revision"]}}),
    )
}

async fn lookup_uri(store: &Store, space: SpaceId, address: &str) -> Result<(Value, String)> {
    let encoded = address
        .strip_prefix("skill://chromie/")
        .ok_or_else(Error::missing)?;
    let id = encoded
        .split('/')
        .next()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(Error::missing)?;
    let manifest = store.skill_manifest(space, id).await?;
    let root = root(&manifest)?;
    for f in manifest["files"]
        .as_array()
        .ok_or_else(Error::unavailable)?
    {
        let path = field(f, "path")?;
        if uri(&root, path)? == address {
            return Ok((manifest.clone(), path.to_owned()));
        }
    }
    Err(Error::missing())
}

pub(crate) async fn custom(
    store: &Store,
    space: SpaceId,
    req: CustomRequest,
) -> std::result::Result<CustomResult, ErrorData> {
    let params = req.params.unwrap_or_default();
    let mut result = json!({"resultType":"complete","ttlMs":5000,"cacheScope":"private"});
    match req.method.as_str() {
        "skills/list" => {
            let after = params
                .get("cursor")
                .map(|v| {
                    v.as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .ok_or_else(|| ErrorData::invalid_params("分页游标无效", None))
                })
                .transpose()?;
            let (list, next) = store
                .skill_manifests_page(space, after)
                .await
                .map_err(protocol_error)?;
            if let Some(cursor) = next {
                result["nextCursor"] = json!(cursor);
            }
            result["skills"] = Value::Array(
                list.iter()
                    .map(entry)
                    .collect::<Result<_>>()
                    .map_err(protocol_error)?,
            );
        }
        "skills/get" => {
            let address = params["uri"]
                .as_str()
                .ok_or_else(|| ErrorData::invalid_params("缺少 Skill 入口 URI", None))?;
            let (m, path) = lookup_uri(store, space, address)
                .await
                .map_err(protocol_error)?;
            if path != "SKILL.md" {
                return Err(protocol_error(Error::missing()));
            }
            result["skill"] = entry(&m).map_err(protocol_error)?;
        }
        _ => {
            return Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "未知的 MCP 方法",
                None,
            ));
        }
    }
    Ok(CustomResult(result))
}

pub(crate) async fn resource(
    store: &Store,
    space: SpaceId,
    address: &str,
) -> Result<ReadResourceResponse> {
    let (m, path) = lookup_uri(store, space, address).await?;
    let id = Uuid::parse_str(field(&m, "skill_id")?).map_err(|_| Error::unavailable())?;
    let revision =
        serde_json::from_value(m["revision"].clone()).map_err(|_| Error::unavailable())?;
    let bytes = store.skill_file(space, id, &path, Some(revision)).await?;
    let media = crate::skill_package::media_type(&path);
    let contents = match std::str::from_utf8(&bytes) {
        Ok(s)
            if (media.starts_with("text/") || media == "application/json") && !s.contains('\0') =>
        {
            ResourceContents::text(s, address).with_mime_type(media)
        }
        _ => ResourceContents::blob(STANDARD.encode(&bytes), address).with_mime_type(media),
    };
    Ok(ReadResourceResult::new(vec![contents])
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
        .into())
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListSkills {
    /// 上一页的 next_cursor，第一页省略。
    pub cursor: Option<Uuid>,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct Read {
    pub skill_id: Uuid,
    pub path: Option<String>,
    pub expected_revision: Option<i32>,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct Report {
    pub skill_id: Uuid,
    pub revision: Option<i32>,
    pub request_id: Uuid,
    pub outcome: String,
    pub reason: Option<String>,
}

pub(crate) async fn read(store: &Store, space: SpaceId, q: Read) -> Result<Value> {
    let path = q.path.as_deref().unwrap_or("SKILL.md");
    let m = store.skill_manifest(space, q.skill_id).await?;
    let revision: i32 =
        serde_json::from_value(m["revision"].clone()).map_err(|_| Error::unavailable())?;
    let stale = |r: &Value| json!({"code":"revision_conflict","message":"Skill 已更新，请重读入口","skill_id":q.skill_id,"revision":r,"path":"SKILL.md"});
    if q.expected_revision.is_some_and(|v| v != revision) {
        return Ok(stale(&m["revision"]));
    }
    let data = match store
        .skill_file(space, q.skill_id, path, Some(revision))
        .await
    {
        Err(e) if e.code == "revision_conflict" => {
            return Ok(stale(
                &store.skill_manifest(space, q.skill_id).await?["revision"],
            ));
        }
        r => r?,
    };
    let mut result = json!({"skill_id":q.skill_id,"revision":revision,"path":path});
    match std::str::from_utf8(&data) {
        Ok(s) if !s.contains('\0') && s.len() <= 60000 => result["text"] = Value::from(s),
        _ => result["next"] = Value::from("请用 chromie skill pull 拉取本版文件，在本机处理"),
    }
    if path == "SKILL.md" {
        result["files"] = json!(
            m["files"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| &f["path"])
                .collect::<Vec<_>>()
        );
    }
    Ok(result)
}
