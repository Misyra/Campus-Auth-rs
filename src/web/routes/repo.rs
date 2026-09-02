//! 仓库代理路由：代理获取远程任务仓库索引和任务配置，避免前端跨域问题
//!
//! 参考原版 `app/api/repo.py` + `app/utils/repo_proxy.py`，Rust 重写版。
//! SSRF 防护（scheme 校验、私网地址拒绝、DNS 钉扎防 TOCTOU、逐跳重定向
//! 校验）统一由 `crate::web::ssrf` 提供；本模块负责 URL 归一化与 JSON 校验。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};
use crate::web::ssrf::secure_get_proxied;

/// 代理响应体大小上限（8 MiB）
///
/// 仓库索引/任务配置是小型 JSON；恶意或误配置的远端可能返回超大响应，
/// 无上限的 `resp.json()` 会将其整体读入内存。
const MAX_REPO_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 将一个 chunk 追加到缓冲区，超过上限返回 None（不追加任何字节）
///
/// 独立成纯函数以便单测覆盖边界判定逻辑
fn append_within_limit(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Option<()> {
    // 先判后拼：超限 chunk 一个字节都不落入缓冲，避免无谓的内存增长
    if buf.len().checked_add(chunk.len()).is_none_or(|n| n > limit) {
        return None;
    }
    buf.extend_from_slice(chunk);
    Some(())
}

/// 归一化仓库 URL：将 GitHub/Gitee blob 页面链接转换为 raw 链接
fn normalize_repo_url(raw: &str) -> String {
    let Ok(parsed) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let host = parsed.host_str().unwrap_or("");
    let path = parsed.path();

    // GitHub: github.com/USER/REPO/blob/BRANCH/PATH → raw.githubusercontent.com/USER/REPO/BRANCH/PATH
    if host == "github.com" {
        if let Some(rest) = path.strip_prefix('/') {
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
    }

    // Gitee: gitee.com/USER/REPO/blob/BRANCH/PATH → gitee.com/USER/REPO/raw/BRANCH/PATH
    if host == "gitee.com" {
        if let Some(rest) = path.strip_prefix('/') {
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
    let mut body: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            tracing::debug!(host = %host, "仓库响应读取失败: {e}");
            ApiError::Internal(format!("{label}响应读取失败: {e}"))
        })?;
        append_within_limit(&mut body, &chunk, MAX_REPO_BODY_BYTES).ok_or_else(|| {
            tracing::debug!(host = %host, "仓库响应体超过大小上限，已中止下载");
            ApiError::BadRequest(format!(
                "{label}响应体超过 {} MiB 上限，已中止下载",
                MAX_REPO_BODY_BYTES / (1024 * 1024)
            ))
        })?;
    }
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
pub async fn repo_fetch_task(
    State(config): State<Arc<dyn ConfigApi>>,
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let proxy = updater_proxy(&config).await;
    let task = repo_fetch_json(&url, false, "任务", proxy.as_deref()).await?;
    Ok(data(task))
}

#[derive(Deserialize)]
pub struct RepoUrlQuery {
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // SSRF 私网判定测试已随 is_restricted 移至 crate::web::ssrf

    // ============ 响应体上限判定 ============

    /// 追加不超过上限时成功，恰好达到上限仍允许（边界为“不超过”）
    #[test]
    fn test_append_within_limit_accepts_up_to_boundary() {
        let mut buf = Vec::new();
        assert!(append_within_limit(&mut buf, b"abc", 8).is_some());
        assert_eq!(buf, b"abc");
        // 恰好填满到上限：允许
        assert!(append_within_limit(&mut buf, b"defgh", 8).is_some());
        assert_eq!(buf.len(), 8);
    }

    /// 超过上限返回 None 且不追加任何字节（缓冲长度保持不变）
    #[test]
    fn test_append_within_limit_rejects_overflow_without_append() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"0123456789"); // 已有 10 字节
        assert!(append_within_limit(&mut buf, b"abc", 12).is_none());
        assert_eq!(buf.len(), 10, "超限 chunk 不应部分或全部落入缓冲");
        // 恰好等于上限（10 + 2 = 12）仍允许
        assert!(append_within_limit(&mut buf, b"ab", 12).is_some());
        assert_eq!(buf.len(), 12);
    }

    /// 常量与换算：上限为 8 MiB
    #[test]
    fn test_repo_body_limit_constant() {
        assert_eq!(MAX_REPO_BODY_BYTES, 8 * 1024 * 1024);
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
