use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::{fmt::Write, path::PathBuf};

pub async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn resource(content_type: &'static str, body: impl IntoResponse) -> Response {
    let mut response = body.into_response();
    for (name, value) in [
        ("content-type", content_type),
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

pub async fn page() -> Response {
    resource(
        "text/html; charset=utf-8",
        include_str!("../web/index.html"),
    )
}

// 只对不含用户数据的公开前端资源启用缓存；可变 URL 每次重验证，升级后不会复用旧内容。
fn cached_resource(headers: &HeaderMap, content_type: &'static str, bytes: Vec<u8>) -> Response {
    let mut tag = String::with_capacity(66);
    tag.push('"');
    for byte in Sha256::digest(&bytes) {
        let _ = write!(tag, "{byte:02x}");
    }
    tag.push('"');
    let matches = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            let value = value.trim();
            value == "*" || value.strip_prefix("W/").unwrap_or(value) == tag
        });
    let mut response = if matches {
        resource(content_type, StatusCode::NOT_MODIFIED)
    } else {
        resource(content_type, bytes)
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    if let Ok(value) = HeaderValue::from_str(&tag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

pub async fn bootstrap(headers: HeaderMap) -> Response {
    cached_resource(
        &headers,
        "text/javascript; charset=utf-8",
        include_bytes!("../web/bootstrap.js").to_vec(),
    )
}

// 路由只映射构建产物的固定文件名，不接受请求路径作为文件系统路径。
async fn asset_bytes(filename: &str) -> std::io::Result<Vec<u8>> {
    let root = std::env::var_os("CHROMIE_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/pkg"));
    tokio::fs::read(root.join(filename)).await
}

fn missing_asset() -> Response {
    resource(
        "text/plain; charset=utf-8",
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "看板资源尚未构建，请按开发运行文档构建 Web",
        ),
    )
}

async fn asset(filename: &str, content_type: &'static str) -> Response {
    match asset_bytes(filename).await {
        Ok(bytes) => resource(content_type, bytes),
        Err(_) => missing_asset(),
    }
}
async fn cached_asset(headers: HeaderMap, filename: &str, content_type: &'static str) -> Response {
    match asset_bytes(filename).await {
        Ok(bytes) => cached_resource(&headers, content_type, bytes),
        Err(_) => missing_asset(),
    }
}

pub async fn javascript(headers: HeaderMap) -> Response {
    cached_asset(headers, "chromie_web.js", "text/javascript; charset=utf-8").await
}

pub async fn wasm(headers: HeaderMap) -> Response {
    cached_asset(headers, "chromie_web_bg.wasm", "application/wasm").await
}

pub async fn codex_plugin() -> Response {
    let mut response = asset("chromie-codex.zip", "application/zip").await;
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"chromie-codex.zip\""),
    );
    response
}

pub async fn capture_client() -> Response {
    let mut response = asset("chromie-client.zip", "application/zip").await;
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"chromie-client.zip\""),
    );
    response
}

pub async fn installer() -> Response {
    resource(
        "text/plain; charset=utf-8",
        include_str!("../integrations/install.sh"),
    )
}

pub async fn cli_asset(
    axum::extract::Path((target, file)): axum::extract::Path<(String, String)>,
) -> Response {
    if !matches!(
        target.as_str(),
        "aarch64-apple-darwin"
            | "x86_64-apple-darwin"
            | "aarch64-unknown-linux-gnu"
            | "x86_64-unknown-linux-gnu"
    ) || !matches!(file.as_str(), "chromie" | "sha256")
    {
        return resource(
            "text/plain; charset=utf-8",
            (StatusCode::NOT_FOUND, "没有此平台的 CLI 构建产物"),
        );
    }
    let response = asset(
        &format!("cli/{target}/{file}"),
        if file == "sha256" {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        },
    )
    .await;
    if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        resource(
            "text/plain; charset=utf-8",
            (StatusCode::NOT_FOUND, "部署者尚未构建此平台的 CLI 程序"),
        )
    } else {
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, routing::get};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn public_resources_revalidate_without_downloading_and_change_with_content() {
        let state = Arc::new(Mutex::new(b"fictional-wasm-version-one".to_vec()));
        let router = Router::new()
            .route(
                "/asset",
                get(
                    |State(bytes): State<Arc<Mutex<Vec<u8>>>>, headers: HeaderMap| async move {
                        cached_resource(&headers, "application/wasm", bytes.lock().unwrap().clone())
                    },
                ),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/asset");
        let first = client.get(&url).send().await.unwrap();
        assert_eq!(first.status(), 200);
        assert_eq!(
            first.headers()[header::CACHE_CONTROL],
            "public, max-age=0, must-revalidate"
        );
        let tag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        assert_eq!(
            first.bytes().await.unwrap().as_ref(),
            b"fictional-wasm-version-one"
        );
        for candidate in [
            tag.clone(),
            format!("W/{tag}"),
            format!("\"unrelated\", W/{tag}"),
            "*".into(),
        ] {
            for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
                let cached = client
                    .request(method, &url)
                    .header(header::IF_NONE_MATCH, &candidate)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(cached.status(), 304);
                assert_eq!(cached.headers()[header::ETAG], tag);
                assert!(cached.bytes().await.unwrap().is_empty());
            }
        }
        *state.lock().unwrap() = b"fictional-wasm-version-two".to_vec();
        let changed = client
            .get(&url)
            .header(header::IF_NONE_MATCH, &tag)
            .send()
            .await
            .unwrap();
        assert_eq!(changed.status(), 200);
        assert_ne!(changed.headers()[header::ETAG], tag);
        assert_eq!(
            changed.bytes().await.unwrap().as_ref(),
            b"fictional-wasm-version-two"
        );
        task.abort();
    }

    #[tokio::test]
    async fn page_and_unavailable_assets_remain_uncacheable() {
        let page = page().await;
        assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!page.headers().contains_key(header::ETAG));
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        let missing = cached_asset(
            headers,
            &format!("missing-{}.wasm", uuid::Uuid::now_v7()),
            "application/wasm",
        )
        .await;
        assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(missing.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!missing.headers().contains_key(header::ETAG));
    }
}
