//! 仓库代理路由：代理获取远程任务仓库索引和任务配置，避免前端跨域问题
//!
//! 参考原版 `app/api/repo.py` + `app/utils/repo_proxy.py`，Rust 重写版。
//! SSRF 防护（scheme 校验、私网地址拒绝、DNS 钉扎防 TOCTOU、逐跳重定向
//! 校验）统一由 `crate::web::ssrf` 提供；本模块负责 URL 归一化与 JSON 校验。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};
use crate::web::ssrf::secure_get_proxied;

// ---- 契约 DTO（T5-1b repo 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。索引/任务配置是**远程
// 仓库内容原样透传**（形状由远端定义，本地只校验数组/对象顶层类型），
// 故以 `DataEnvelope<Value>` 标注、不编造内部形状；截图代理是二进制
// 图片响应（非 JSON），按真实下发的六种 MIME 逐一声明 binary 内容。

/// GET /api/repo/image 200 响应体 schema 载体
///
/// wire 上是原始图片字节流（按 magic bytes 识别的真实 MIME 下发），不是
/// JSON 包裹；本类型仅用于把 OpenAPI schema 声明为
/// `{"type": "string", "format": "binary"}`，不参与序列化。
#[derive(Debug, ToSchema)]
#[schema(value_type = String, format = Binary)]
// 纯 schema 载体：仅在 OpenAPI 生成时被 utoipa 引用，字段值不参与任何
// 运行时路径（handler 直接下发字节流），dead_code 属预期
#[allow(dead_code)]
pub struct RepoImageBody(Vec<u8>);

/// 代理响应体大小上限（8 MiB）
///
/// 仓库索引/任务配置是小型 JSON；截图是二进制图片。恶意或误配置的远端
/// 可能返回超大响应，无上限读取会将其整体读入内存，故统一截断。
const MAX_REPO_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 截图代理允许的目标 host（任务站 raw 域）
///
/// 仓库索引的 `screenshot` 字段指向 GitHub/Gitee raw 图片。截图经 `<img>`
/// 直接引用，无法携带鉴权头，故中间件对本端点 GET 豁免；为避免豁免被滥用
/// 为开放 SSRF 出口，出站目标仅放行任务站 raw 三 host，拒绝任意 URL。
fn is_allowed_screenshot_host(host: &str) -> bool {
    matches!(
        host,
        "raw.githubusercontent.com" | "raw.giteeusercontent.com" | "gitee.com"
    )
}

// 图片 magic 识别与「流式读 + 字节上限」已上提至 `crate::web::validate`
// （与 background 域共用同一实现），本域经导入取用
use crate::web::validate::{detect_image_mime, read_stream_capped};

/// 归一化仓库 URL：将 GitHub/Gitee blob 页面链接转换为 raw 链接
fn normalize_repo_url(raw: &str) -> String {
    let Ok(parsed) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let host = parsed.host_str().unwrap_or("");
    let path = parsed.path();

    // GitHub: github.com/USER/REPO/blob/BRANCH/PATH → raw.githubusercontent.com/USER/REPO/BRANCH/PATH
    if host == "github.com"
        && let Some(rest) = path.strip_prefix('/')
    {
        let parts: Vec<&str> = rest.splitn(4, '/').collect();
        if parts.len() >= 4 && parts[2] == "blob" {
            let extra: Vec<&str> = parts[3..].to_vec();
            return format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                parts[0],
                parts[1],
                extra.join("/"),
            );
        }
    }

    // Gitee: gitee.com/USER/REPO/blob/BRANCH/PATH → gitee.com/USER/REPO/raw/BRANCH/PATH
    if host == "gitee.com"
        && let Some(rest) = path.strip_prefix('/')
    {
        let parts: Vec<&str> = rest.splitn(4, '/').collect();
        if parts.len() >= 4 && parts[2] == "blob" {
            let extra: Vec<&str> = parts[3..].to_vec();
            return format!(
                "https://gitee.com/{}/{}/raw/{}",
                parts[0],
                parts[1],
                extra.join("/"),
            );
        }
    }

    raw.to_string()
}

/// 读取更新器代理设置：启用时返回 `proxy_url`（仓库任务与更新共用同一代理配置
/// ——国内访问 GitHub raw 常需代理），未启用或地址为空返回 None（直连/系统代理）。
async fn updater_proxy(config: &Arc<dyn ConfigApi>) -> Option<String> {
    let updater = config.load_settings_async().await.global.updater;
    (updater.use_proxy && !updater.resolved_proxy_url().is_empty())
        .then(|| updater.resolved_proxy_url())
}

/// 获取远程 JSON 并校验类型（数组或对象）
///
/// SSRF 防护由 `secure_get_proxied` 统一提供（DNS 钉扎 + 逐跳重定向校验）
async fn repo_fetch_json(
    url: &str,
    expected_list: bool,
    label: &str,
    proxy: Option<&str>,
) -> Result<Value, ApiError> {
    // 失败路径的 debug 日志只记 host，不记录完整 URL（query 可能携带敏感参数）
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let (resp, _) = secure_get_proxied(url, Duration::from_secs(15), "Campus-Auth", proxy)
        .await
        .map_err(|e| {
            tracing::debug!(host = %host, "仓库请求失败: {e}");
            ApiError::BadRequest(e)
        })?;
    let status = resp.status();
    if !status.is_success() {
        tracing::debug!(host = %host, status = %status, "仓库请求返回非成功状态");
        return Err(ApiError::ServiceUnavailable(format!(
            "远程返回 HTTP {status} ({url})"
        )));
    }
    // 流式累积读取响应体，超过上限立即中止；
    // bytes_stream 已由 secure_get 内部的 reqwest 客户端完成 gzip 解码
    let body = read_stream_capped(
        resp,
        MAX_REPO_BODY_BYTES,
        || {
            tracing::debug!(host = %host, "仓库响应体超过大小上限，已中止下载");
            ApiError::BadRequest(format!(
                "{label}响应体超过 {} MiB 上限，已中止下载",
                MAX_REPO_BODY_BYTES / (1024 * 1024)
            ))
        },
        |e| {
            tracing::debug!(host = %host, "仓库响应读取失败: {e}");
            ApiError::Internal(format!("{label}响应读取失败: {e}"))
        },
    )
    .await?;
    let json: Value = serde_json::from_slice(&body).map_err(|e| {
        tracing::debug!(host = %host, "仓库响应 JSON 解析失败: {e}");
        ApiError::Internal(format!("{label} JSON 解析失败: {e}"))
    })?;
    let type_name = if expected_list {
        "JSON 数组"
    } else {
        "JSON 对象"
    };
    if (expected_list && !json.is_array()) || (!expected_list && !json.is_object()) {
        tracing::debug!(host = %host, expected = type_name, "仓库响应类型不正确");
        return Err(ApiError::Internal(format!(
            "{label}格式不正确，应为 {type_name}"
        )));
    }
    Ok(json)
}

/// GET /api/repo/fetch — 代理获取远程任务仓库索引（返回 JSON 数组）
///
/// URL 归一化（GitHub/Gitee blob 页面链接转 raw）+ SSRF 防护后原样透传
/// 远端 JSON；仅校验顶层为数组，条目形状由远端仓库定义。
#[utoipa::path(
    get,
    path = "/api/repo/fetch",
    operation_id = "get_api_repo_fetch",
    tag = "repo",
    params(RepoUrlQuery),
    responses(
        (status = 200, description = "远程仓库索引 JSON 数组原样透传（data 为任意 JSON 数组，形状由远端定义）", body = DataEnvelope<Value>),
        (status = 400, description = "URL 非法、scheme 不支持、目标为私网地址（SSRF 防护）或响应体超过 8 MiB 上限", body = ErrorResponse),
        (status = 503, description = "远程返回非成功状态码", body = ErrorResponse),
        (status = 500, description = "响应读取失败或响应不是合法 JSON", body = ErrorResponse),
    )
)]
pub async fn repo_fetch_index(
    State(config): State<Arc<dyn ConfigApi>>,
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let proxy = updater_proxy(&config).await;
    let index = repo_fetch_json(&url, true, "索引", proxy.as_deref()).await?;
    Ok(data(index))
}

/// GET /api/repo/task — 代理获取远程任务配置（返回 JSON 对象）
///
/// URL 归一化 + SSRF 防护后原样透传远端 JSON；仅校验顶层为对象，
/// 任务配置形状由远端仓库定义（与本地任务文件同源格式）。
#[utoipa::path(
    get,
    path = "/api/repo/task",
    operation_id = "get_api_repo_task",
    tag = "repo",
    params(RepoUrlQuery),
    responses(
        (status = 200, description = "远程任务配置 JSON 对象原样透传（data 为任意 JSON 对象，形状由远端定义）", body = DataEnvelope<Value>),
        (status = 400, description = "URL 非法、scheme 不支持、目标为私网地址（SSRF 防护）或响应体超过 8 MiB 上限", body = ErrorResponse),
        (status = 503, description = "远程返回非成功状态码", body = ErrorResponse),
        (status = 500, description = "响应读取失败或响应不是合法 JSON", body = ErrorResponse),
    )
)]
pub async fn repo_fetch_task(
    State(config): State<Arc<dyn ConfigApi>>,
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let proxy = updater_proxy(&config).await;
    let task = repo_fetch_json(&url, false, "任务", proxy.as_deref()).await?;
    Ok(data(task))
}

/// GET /api/repo/image — 代理获取仓库任务截图（返回图片字节）
///
/// `<img>` 引用无法携带鉴权头（同背景图/调试截图 GET 豁免先例），且 raw 图片
/// 直连国内常需代理（与仓库任务共用 updater 代理配置）。豁免 + 代理的组合若
/// 对任意 URL 开放即成开放 SSRF 出口，故目标 host 限死任务站 raw 三 host；
/// 响应再做大小上限 + magic bytes 格式校验，MIME 按真实签名下发（防类型混淆）。
#[utoipa::path(
    get,
    path = "/api/repo/image",
    operation_id = "get_api_repo_image",
    tag = "repo",
    params(RepoUrlQuery),
    responses(
        // 图片代理端点：200 是二进制图片而非 JSON 信封，按真实下发的六种
        // MIME 逐一声明 binary 内容（错误路径仍是统一 JSON 错误信封）
        (status = 200, description = "任务站截图原始字节（MIME 按真实 magic bytes 签名下发，非 JSON 信封）",
            content(
                (RepoImageBody = "image/png"),
                (RepoImageBody = "image/jpeg"),
                (RepoImageBody = "image/gif"),
                (RepoImageBody = "image/webp"),
                (RepoImageBody = "image/bmp"),
                (RepoImageBody = "image/x-icon"),
            ),
        ),
        (status = 400, description = "URL/scheme 非法、目标 host 不在任务站 raw 白名单、重定向终点越界、响应体超过 8 MiB 上限或不是有效位图（SVG 等可执行格式拒绝）", body = ErrorResponse),
        (status = 503, description = "远程返回非成功状态码", body = ErrorResponse),
        (status = 500, description = "响应读取失败", body = ErrorResponse),
    )
)]
pub async fn repo_image(
    State(config): State<Arc<dyn ConfigApi>>,
    Query(params): Query<RepoUrlQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let url = normalize_repo_url(&params.url);
    let parsed =
        url::Url::parse(&url).map_err(|e| ApiError::BadRequest(format!("无效 URL: {e}")))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ApiError::BadRequest(format!(
            "不支持的 URL 协议: {scheme}，仅支持 http/https"
        )));
    }
    let host = parsed.host_str().unwrap_or("").to_string();
    if !is_allowed_screenshot_host(&host) {
        return Err(ApiError::BadRequest(format!(
            "截图仅允许任务站图片地址（raw.githubusercontent.com / raw.giteeusercontent.com / gitee.com）: {host}"
        )));
    }
    let proxy = updater_proxy(&config).await;
    let (resp, final_url) = secure_get_proxied(
        &url,
        Duration::from_secs(30),
        "Campus-Auth",
        proxy.as_deref(),
    )
    .await
    .map_err(|e| {
        tracing::debug!(host = %host, "仓库截图请求失败: {e}");
        ApiError::BadRequest(e)
    })?;
    // 逐跳重定向已由 secure_get_proxied 做公网校验；此处再收敛终点 host，
    // 防“白名单 URL 302 跳到任意公网地址”借豁免端点外发
    let final_host = url::Url::parse(&final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    if !is_allowed_screenshot_host(&final_host) {
        tracing::debug!(host = %final_host, "仓库截图重定向终点不在白名单，已拒绝");
        return Err(ApiError::BadRequest(format!(
            "截图重定向目标不在任务站图片域内，已拒绝: {final_host}"
        )));
    }
    let status = resp.status();
    if !status.is_success() {
        tracing::debug!(host = %host, status = %status, "仓库截图返回非成功状态");
        return Err(ApiError::ServiceUnavailable(format!(
            "远程返回 HTTP {status} ({url})"
        )));
    }
    // 流式累积读取响应体，超过上限立即中止（与 JSON 代理同口径）
    let body = read_stream_capped(
        resp,
        MAX_REPO_BODY_BYTES,
        || {
            tracing::debug!(host = %host, "仓库截图超过大小上限，已中止下载");
            ApiError::BadRequest(format!(
                "截图响应体超过 {} MiB 上限，已中止下载",
                MAX_REPO_BODY_BYTES / (1024 * 1024)
            ))
        },
        |e| {
            tracing::debug!(host = %host, "仓库截图读取失败: {e}");
            ApiError::Internal(format!("截图响应读取失败: {e}"))
        },
    )
    .await?;
    let mime = detect_image_mime(&body).ok_or_else(|| {
        tracing::debug!(host = %host, "仓库截图不是有效的位图");
        ApiError::BadRequest("截图不是有效的 PNG/JPEG/GIF/WebP/BMP/ICO 图片".into())
    })?;
    // 只记 host，不记录完整 URL（query 可能携带敏感参数）
    tracing::debug!(host = %host, size = body.len(), mime, "仓库截图代理成功");
    Ok(([(header::CONTENT_TYPE, mime)], body))
}

/// GET /api/repo/fetch、/api/repo/task 与 /api/repo/image 共用的查询参数
#[derive(Debug, Deserialize, IntoParams)]
pub struct RepoUrlQuery {
    /// 远程仓库资源地址（GitHub blob 页面地址会被归一化为 raw 地址）
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // SSRF 私网判定测试已随 is_restricted 移至 crate::web::ssrf；
    // append_within_limit 边界测试已随函数上提至 crate::web::validate

    /// 常量与换算：上限为 8 MiB
    #[test]
    fn test_repo_body_limit_constant() {
        assert_eq!(MAX_REPO_BODY_BYTES, 8 * 1024 * 1024);
    }

    // ============ 截图 host 白名单 ============

    /// 任务站 raw 三 host 放行，其余一律拒绝（含大小写与子域伪装）
    #[test]
    fn test_screenshot_host_allowlist() {
        assert!(is_allowed_screenshot_host("raw.githubusercontent.com"));
        assert!(is_allowed_screenshot_host("raw.giteeusercontent.com"));
        assert!(is_allowed_screenshot_host("gitee.com"));
        assert!(!is_allowed_screenshot_host("github.com"));
        assert!(!is_allowed_screenshot_host("example.com"));
        assert!(!is_allowed_screenshot_host(
            "evil-raw.githubusercontent.com"
        ));
        assert!(!is_allowed_screenshot_host("RAW.GITHUBUSERCONTENT.COM"));
        assert!(!is_allowed_screenshot_host(""));
    }

    // ============ 截图 magic bytes 识别 ============

    /// 常见位图按签名识别 MIME；文本/SVG/未知字节拒绝（SVG 可内嵌脚本）
    #[test]
    fn test_detect_image_mime_recognizes_bitmap_rejects_text() {
        assert_eq!(
            detect_image_mime(b"\x89PNG\r\n\x1a\nxxxx"),
            Some("image/png")
        );
        assert_eq!(
            detect_image_mime(b"\xFF\xD8\xFF\xE0xxxx"),
            Some("image/jpeg")
        );
        assert_eq!(detect_image_mime(b"GIF89axxxx"), Some("image/gif"));
        let mut webp = b"RIFF....WEBP".to_vec();
        webp[4..8].copy_from_slice(b"1234");
        assert_eq!(detect_image_mime(&webp), Some("image/webp"));
        assert_eq!(detect_image_mime(b"BMxxxx"), Some("image/bmp"));
        assert_eq!(
            detect_image_mime(b"\x00\x00\x01\x00xxxx"),
            Some("image/x-icon")
        );
        assert_eq!(detect_image_mime(b"hello"), None);
        assert_eq!(detect_image_mime(b"<svg xmlns='x'></svg>"), None);
        assert_eq!(detect_image_mime(b""), None);
    }

    // ============ URL 归一化 ============

    #[test]
    fn test_normalize_github_blob_to_raw() {
        let url = "https://github.com/user/repo/blob/main/tasks/index.json";
        assert_eq!(
            normalize_repo_url(url),
            "https://raw.githubusercontent.com/user/repo/main/tasks/index.json"
        );
    }

    #[test]
    fn test_normalize_gitee_blob_to_raw() {
        let url = "https://gitee.com/user/repo/blob/master/tasks/x.json";
        assert_eq!(
            normalize_repo_url(url),
            "https://gitee.com/user/repo/raw/master/tasks/x.json"
        );
    }

    #[test]
    fn test_normalize_github_blob_at_branch_root() {
        // blob/BRANCH 无子路径：转换到分支根目录
        let url = "https://github.com/user/repo/blob/main";
        assert_eq!(
            normalize_repo_url(url),
            "https://raw.githubusercontent.com/user/repo/main"
        );
    }

    #[test]
    fn test_normalize_keeps_non_blob_and_foreign_urls() {
        // blob 段不足（仅 user/repo/blob 两段路径）：不满足 splitn(4) 的 4 段条件，原样返回
        let plain = "https://github.com/user/repo/blob";
        assert_eq!(normalize_repo_url(plain), plain);
        // 非 github/gitee 域名：原样返回
        let foreign = "https://example.com/a/b.txt";
        assert_eq!(normalize_repo_url(foreign), foreign);
        // 非法 URL：原样返回（不 panic）
        assert_eq!(normalize_repo_url("not a url"), "not a url");
        // 已是 raw 域名：不再重复转换
        let raw = "https://raw.githubusercontent.com/user/repo/main/x.json";
        assert_eq!(normalize_repo_url(raw), raw);
    }
}
