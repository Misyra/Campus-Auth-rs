//! rust-embed 静态文件服务（frontend/dist 经编译嵌入）

use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;

#[cfg(not(feature = "no-embed"))]
use axum::http::header;
#[cfg(not(feature = "no-embed"))]
use axum::response::Response;

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

#[cfg(not(feature = "no-embed"))]
pub async fn handler(uri: Uri) -> impl IntoResponse {
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
        "no-cache"
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
pub async fn handler(_uri: Uri) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "前端未嵌入（no-embed 构建）").into_response()
}
