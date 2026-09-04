//! 背景图路由：获取 / 上传 / URL 拉取 / 删除（A-5 自 system.rs 拆出）
//!
//! M1 细粒度 state：经 `State<Arc<dyn ConfigApi>>` 提取，不触达 `state.container`。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::IntoResponse;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;

use std::sync::Arc;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};

/// 背景图文件最大字节数（上传与远程下载统一）。
pub(crate) const MAX_BACKGROUND_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// multipart 请求体需要为边界与字段头预留少量空间。
pub(crate) const BACKGROUND_UPLOAD_BODY_LIMIT: usize = MAX_BACKGROUND_IMAGE_BYTES + 64 * 1024;

// ---- 背景图管理 ----

#[derive(Deserialize)]
pub struct BackgroundFetchBody {
    /// 图片 URL
    pub url: String,
}

/// 背景图存储目录
fn background_dir(config: &Arc<dyn ConfigApi>) -> std::path::PathBuf {
    config.base_path().join("config").join("background")
}

/// 从文件名中提取安全文件名（防路径穿越），失败则用 UUID 生成
fn safe_filename(original: Option<String>) -> String {
    let candidate = original.unwrap_or_else(|| format!("bg-{}", uuid::Uuid::new_v4()));
    std::path::Path::new(&candidate)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("bg-{}", uuid::Uuid::new_v4()))
}

/// 根据 Content-Type 返回图片扩展名（不含 `.`）
fn ext_from_content_type(ct: &str) -> Option<&'static str> {
    match ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/x-icon" => Some("ico"),
        _ => None,
    }
}

/// 根据 magic bytes 识别图片格式（Content-Type 缺失或不可信时兜底）
fn ext_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        Some("png")
    } else if bytes.len() >= 3 && &bytes[0..3] == b"\xFF\xD8\xFF" {
        Some("jpg")
    } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
        Some("bmp")
    } else if bytes.len() >= 4 && (&bytes[0..4] == b"\x00\x00\x01\x00") {
        Some("ico")
    } else {
        None
    }
}

/// 按真实文件签名验证背景图，返回规范化扩展名。
fn validate_background_image(bytes: &[u8], content_type: &str) -> Result<&'static str, ApiError> {
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("背景图不能为空".into()));
    }
    if bytes.len() > MAX_BACKGROUND_IMAGE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "背景图超过 {}MB 上限",
            MAX_BACKGROUND_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    // SVG 可在直接导航时以同源文档执行脚本，背景图功能只接受不可执行的位图格式。
    if content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("image/svg+xml")
    {
        return Err(ApiError::BadRequest("不支持 SVG 背景图".into()));
    }
    let detected = ext_from_magic(bytes).ok_or_else(|| {
        ApiError::BadRequest("无法识别图片格式，仅支持 PNG/JPEG/GIF/WebP/BMP/ICO".into())
    })?;
    if let Some(declared) = ext_from_content_type(content_type) {
        if declared != detected {
            return Err(ApiError::BadRequest(format!(
                "图片声明类型与实际内容不一致: {declared} != {detected}"
            )));
        }
    }
    Ok(detected)
}

/// 生成不会与 Windows 设备名、ADS 或已有文件冲突的背景图文件名。
fn background_filename(original: Option<String>, ext: &str) -> String {
    let safe = safe_filename(original);
    let stem = std::path::Path::new(&safe)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let normalized: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect();
    let stem = if normalized.is_empty() {
        "image"
    } else {
        &normalized
    };
    format!("bg-{stem}-{}.{}", uuid::Uuid::new_v4(), ext)
}

/// 在读取远程响应时执行实际字节数上限，不能只信任可缺失或可伪造的 Content-Length。
async fn read_response_limited(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ApiError::BadRequest(format!(
            "远程图片超过 {}MB 上限",
            limit / (1024 * 1024)
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::Internal(format!("读取图片字节失败: {e}")))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::BadRequest(format!(
                "远程图片超过 {}MB 上限",
                limit / (1024 * 1024)
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// GET /api/background/{filename} — 获取背景图
///
/// 返回原始图片字节 + 正确 Content-Type，供前端 CSS url() 直接引用。
pub async fn get_background(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // 防路径穿越：提取安全文件名，并拒绝包含 `..` 的输入
    if filename.contains("..") {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let safe_name = safe_filename(Some(filename));
    let dir = background_dir(&config);
    let path = dir.join(&safe_name);
    // 确保最终路径仍在背景图目录之内
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("背景图 {} 不存在", safe_name)));
    }
    let bytes = tokio::fs::read(&path).await?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "svg" {
        return Err(ApiError::BadRequest("不支持 SVG 背景图".into()));
    }
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    Ok(([(header::CONTENT_TYPE, mime)], bytes))
}

/// POST /api/background/upload — 上传背景图（multipart/form-data，字段名 file）
pub async fn upload_background(
    State(config): State<Arc<dyn ConfigApi>>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<Value>, ApiError> {
    let dir = background_dir(&config);
    tokio::fs::create_dir_all(&dir).await?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart 解析失败: {e}")))?
    {
        if field.name() == Some("file") {
            let original = field.file_name().map(|s| s.to_string());
            let content_type = field
                .content_type()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("读取文件字节失败: {e}")))?;
            let ext = validate_background_image(&bytes, &content_type)?;
            let filename = background_filename(original, ext);
            let path = dir.join(&filename);
            tokio::fs::write(&path, &bytes).await?;
            tracing::info!(file = %filename, size = bytes.len() as u64, "背景图上传成功");
            return Ok(data(serde_json::json!({
                "filename": filename,
                "url": format!("/api/background/{}", filename),
            })));
        }
    }
    Err(ApiError::BadRequest("缺少 file 字段".into()))
}

/// POST /api/background/fetch-url — 从 URL 获取背景图
pub async fn fetch_url_background(
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<BackgroundFetchBody>,
) -> Result<Json<Value>, ApiError> {
    // 本端点仅允许 HTTPS（SSRF 防护：scheme 校验 + DNS 钉扎 + 逐跳重定向
    // 校验统一由 secure_get 提供，修复"校验与请求二次解析"的 TOCTOU 缺口）
    let parsed =
        url::Url::parse(&body.url).map_err(|e| ApiError::BadRequest(format!("无效 URL: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(ApiError::BadRequest("仅允许 HTTPS URL".into()));
    }
    let (response, _) =
        crate::web::ssrf::secure_get(&body.url, std::time::Duration::from_secs(30), "Campus-Auth")
            .await
            .map_err(ApiError::BadRequest)?;
    // 验证 Content-Type 为图片类型，防止下载非图片内容
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.starts_with("image/") {
        return Err(ApiError::BadRequest(format!(
            "URL 返回非图片类型: {}",
            content_type
        )));
    }
    let bytes = read_response_limited(response, MAX_BACKGROUND_IMAGE_BYTES).await?;
    let dir = background_dir(&config);
    tokio::fs::create_dir_all(&dir).await?;
    // 从 URL 路径提取文件名，失败则用 UUID 生成
    let extracted = body
        .url
        .split('?')
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let ext = validate_background_image(&bytes, &content_type)?;
    let filename = background_filename(extracted, ext);
    let path = dir.join(&filename);
    tokio::fs::write(&path, &bytes).await?;
    // 只记目标 host，不记录完整 URL（query 可能携带敏感参数）
    tracing::info!(
        host = parsed.host_str().unwrap_or(""),
        file = %filename,
        size = bytes.len() as u64,
        "背景图从 URL 拉取成功"
    );
    Ok(data(serde_json::json!({
        "filename": filename,
        "url": format!("/api/background/{}", filename),
    })))
}

/// DELETE /api/background/{filename} — 删除背景图
pub async fn delete_background(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(filename): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // 防路径穿越：提取安全文件名，并拒绝包含 `..` 的输入
    if filename.contains("..") {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let safe_name = safe_filename(Some(filename));
    let dir = background_dir(&config);
    let path = dir.join(&safe_name);
    // 确保最终路径仍在背景图目录之内
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("背景图 {} 不存在", safe_name)));
    }
    tokio::fs::remove_file(&path).await?;
    tracing::info!(file = %safe_name, "背景图已删除");
    Ok(data(Value::String("ok".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ 背景图扩展名识别 ============

    #[test]
    fn ext_from_content_type_maps_known_types() {
        assert_eq!(ext_from_content_type("image/png"), Some("png"));
        assert_eq!(ext_from_content_type("image/jpeg"), Some("jpg"));
        assert_eq!(ext_from_content_type("image/webp"), Some("webp"));
        assert_eq!(ext_from_content_type("image/svg+xml"), None);
        // 带参数 / 大小写混合 / 未知类型
        assert_eq!(
            ext_from_content_type("image/PNG; charset=utf-8"),
            Some("png")
        );
        assert_eq!(ext_from_content_type("application/octet-stream"), None);
        assert_eq!(ext_from_content_type(""), None);
    }

    #[test]
    fn ext_from_magic_recognizes_common_formats() {
        assert_eq!(ext_from_magic(b"\x89PNG\r\n\x1a\nxxxx"), Some("png"));
        assert_eq!(ext_from_magic(b"\xFF\xD8\xFF\xE0xxxx"), Some("jpg"));
        assert_eq!(ext_from_magic(b"GIF89a"), Some("gif"));
        assert_eq!(ext_from_magic(b"BMxxxx"), Some("bmp"));
        assert_eq!(ext_from_magic(b"\x00\x00\x01\x00xxxx"), Some("ico"));
        // WEBP: RIFF....WEBP
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(b"0000WEBP");
        assert_eq!(ext_from_magic(&webp), Some("webp"));
        // 未知 magic
        assert_eq!(ext_from_magic(b"hello"), None);
    }

    #[test]
    fn validate_background_image_rejects_svg_and_type_mismatch() {
        let png = b"\x89PNG\r\n\x1a\n1";
        assert_eq!(validate_background_image(png, "image/png").unwrap(), "png");
        assert!(validate_background_image(b"<svg></svg>", "image/svg+xml").is_err());
        assert!(validate_background_image(png, "image/jpeg").is_err());
        assert!(validate_background_image(b"not-image", "image/png").is_err());
    }

    #[test]
    fn background_filename_uses_safe_unique_name() {
        let filename = background_filename(Some("..\\CON:evil.png".into()), "png");
        assert!(filename.starts_with("bg-CONevil-"));
        assert!(filename.ends_with(".png"));
        assert!(!filename.contains(':'));
        assert!(!filename.contains(".."));
    }

    // ============ 背景图文件名安全 ============

    #[test]
    fn safe_filename_strips_path_components() {
        // 路径穿越尝试：只取 file_name
        assert_eq!(safe_filename(Some("../../etc/passwd".into())), "passwd");
        // 正常文件名
        assert_eq!(safe_filename(Some("sunset.png".into())), "sunset.png");
    }

    #[test]
    fn safe_filename_falls_back_to_uuid_on_empty() {
        let fallback = safe_filename(None);
        assert!(!fallback.is_empty());
        assert!(fallback.starts_with("bg-"));
    }

    // SSRF 私网 IP 判定测试已随 is_private_ip 收敛至 crate::web::ssrf 模块
}

#[cfg(test)]
mod route_tests {
    use std::sync::Arc;

    use super::{delete_background, fetch_url_background, get_background, upload_background};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt; // oneshot

    use super::super::test_support::{MockConfigApi, body_json};

    fn mock_app(
        base: &std::path::Path,
    ) -> (
        axum::Router,
        Arc<std::sync::Mutex<super::super::test_support::MockConfigInner>>,
    ) {
        let (config, inner) = MockConfigApi::mocked();
        inner.lock().unwrap().base_path = base.to_path_buf();
        let app = axum::Router::new()
            .route(
                "/api/background/{filename}",
                get(get_background).delete(delete_background),
            )
            .route("/api/background/upload", post(upload_background))
            .route("/api/background/fetch-url", post(fetch_url_background))
            .with_state(config);
        (app, inner)
    }

    fn png_bytes() -> Vec<u8> {
        // 最小合法 PNG：magic + 1 字节载荷（validate 仅校验 magic 与类型一致）
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.push(0);
        v
    }

    /// 路径穿越直接 400，不触盘
    #[tokio::test]
    async fn get_and_delete_reject_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _) = mock_app(tmp.path());
        for (method, uri) in [
            ("GET", "/api/background/..%2Fsecret"),
            ("DELETE", "/api/background/..%2Fsecret"),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND,
                "{method}: {}",
                resp.status()
            );
        }
    }

    /// 缺失文件 → 404；存在 → 200 + MIME
    #[tokio::test]
    async fn get_missing_and_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config").join("background");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), png_bytes()).unwrap();
        let (app, _) = mock_app(tmp.path());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/background/nope.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/background/a.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "image/png");
    }

    fn multipart_file(boundary: &str, filename: &str, content_type: &str, bytes: &[u8]) -> Vec<u8> {
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    /// 上传 PNG 落盘并返回可访问 URL；缺 file 字段 → 400
    #[tokio::test]
    async fn upload_roundtrip_and_missing_field() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _) = mock_app(tmp.path());
        let boundary = "TESTBOUNDARY";
        let body = multipart_file(boundary, "up.png", "image/png", &png_bytes());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/background/upload")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let url = v["data"]["url"].as_str().unwrap().to_string();
        assert!(url.starts_with("/api/background/"));
        // 返回的 URL 立即可读
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(&url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 缺 file 字段
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nx\r\n--{boundary}--\r\n"
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/background/upload")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 删除往返：删后再次读取 404
    #[tokio::test]
    async fn delete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config").join("background");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("d.png"), png_bytes()).unwrap();
        let (app, _) = mock_app(tmp.path());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/background/d.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/background/d.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 非 HTTPS 直接 400（SSRF，第一道门；不断网、不出环）
    #[tokio::test]
    async fn fetch_url_rejects_non_https() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _) = mock_app(tmp.path());
        for url in [
            "http://127.0.0.1:18765/captcha",
            "ftp://example.com/a.png",
            "not-a-url",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/background/fetch-url")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(r#"{{"url":{url:?}}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "url={url}");
        }
    }
}
