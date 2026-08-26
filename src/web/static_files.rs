//! rust-embed 静态文件服务（frontend/dist 经编译嵌入）

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

/// 未注册的 `/api/*` 请求返回的 404 JSON 响应（避免被 SPA 回退吞成 200 + index.html）。
fn api_not_found() -> Response {
    let body = r#"{"error":{"code":"NOT_FOUND","message":"接口不存在"}}"#;
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

#[cfg(not(feature = "no-embed"))]
/// 根据扩展名推断 MIME（避免引入额外依赖）
fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(not(feature = "no-embed"))]
#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// 根目录 `openapi.json` 的嵌入（供 `/openapi.json` 路由服务，生产环境可用）
#[cfg(not(feature = "no-embed"))]
#[derive(rust_embed::RustEmbed)]
#[folder = "."]
#[include = "openapi.json"]
struct OpenApiAsset;

#[cfg(not(feature = "no-embed"))]
/// 返回嵌入的 openapi.json（前端运行时兜底获取版本等契约信息）
pub async fn openapi_handler() -> impl IntoResponse {
    match OpenApiAsset::get("openapi.json") {
        Some(asset) => asset_response("openapi.json", asset),
        None => (StatusCode::NOT_FOUND, "openapi.json 未嵌入").into_response(),
    }
}

#[cfg(feature = "no-embed")]
pub async fn openapi_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        "openapi.json 未嵌入（no-embed 构建）",
    )
        .into_response()
}

#[cfg(all(test, not(feature = "no-embed")))]
mod tests {
    use super::*;

    /// 嵌入的 openapi.json 应存在且可解析为含 info.version 的对象
    #[test]
    fn test_openapi_asset_embedded() {
        let asset = OpenApiAsset::get("openapi.json").expect("openapi.json 应被嵌入");
        let text = std::str::from_utf8(&asset.data).expect("openapi.json 应为 UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(text).expect("openapi.json 应为合法 JSON");
        assert!(parsed["info"]["version"].is_string(), "应含 info.version");
    }
}

#[cfg(not(feature = "no-embed"))]
pub async fn handler(uri: Uri) -> impl IntoResponse {
    // 未注册的 /api/* 直接返回 404 JSON，避免被 SPA 回退吞成 200 + index.html
    if uri.path().starts_with("/api") {
        return api_not_found();
    }
    let path = uri.path().trim_start_matches('/');
    if let Some(asset) = Assets::get(path) {
        return asset_response(path, asset);
    }
    // SPA 路由：尝试 path + "/index.html"
    let with_index = format!("{path}/index.html");
    if let Some(asset) = Assets::get(&with_index) {
        return asset_response(&with_index, asset);
    }
    // 最终回退到 index.html
    if let Some(asset) = Assets::get("index.html") {
        return asset_response("index.html", asset);
    }
    (StatusCode::NOT_FOUND, "前端资源未嵌入").into_response()
}

#[cfg(not(feature = "no-embed"))]
fn asset_response(path: &str, asset: rust_embed::EmbeddedFile) -> Response {
    let mime = guess_mime(path);
    let cache = if path.contains('.') && !path.ends_with("index.html") {
        "max-age=31536000, immutable"
    } else {
        // index.html 必须用 no-store：no-cache 依赖 ETag/Last-Modified 验证器，
        // 但嵌入资源无这些头，浏览器会直接用缓存 → 引用旧 bundle 名 → 前端永远停在旧版本
        "no-store"
    };
    let body: Vec<u8> = asset.data.into_owned();
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::OK;
    if let Ok(v) = header::HeaderValue::from_str(mime) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = header::HeaderValue::from_str(cache) {
        resp.headers_mut().insert(header::CACHE_CONTROL, v);
    }
    resp
}

#[cfg(feature = "no-embed")]
pub async fn handler(uri: Uri) -> impl IntoResponse {
    // 未注册的 /api/* 返回 404 JSON（与嵌入版一致）
    if uri.path().starts_with("/api") {
        return api_not_found();
    }
    (StatusCode::NOT_FOUND, "前端未嵌入（no-embed 构建）").into_response()
}
