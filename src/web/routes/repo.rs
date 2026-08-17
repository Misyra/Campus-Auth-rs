//! 仓库代理路由：代理获取远程任务仓库索引和任务配置，避免前端跨域问题
//!
//! 参考原版 `app/api/repo.py` + `app/utils/repo_proxy.py`，Rust 重写版。
//! SSRF 防护（scheme 校验、私网地址拒绝、DNS 钉扎防 TOCTOU、逐跳重定向
//! 校验）统一由 `crate::web::ssrf` 提供；本模块负责 URL 归一化与 JSON 校验。

use std::time::Duration;

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::ssrf::secure_get;

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

/// 获取远程 JSON 并校验类型（数组或对象）
///
/// SSRF 防护由 `secure_get` 统一提供（DNS 钉扎 + 逐跳重定向校验）
async fn repo_fetch_json(url: &str, expected_list: bool, label: &str) -> Result<Value, ApiError> {
    let (resp, _) = secure_get(url, Duration::from_secs(15), "Campus-Auth")
        .await
        .map_err(ApiError::BadRequest)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ApiError::ServiceUnavailable(format!(
            "远程返回 HTTP {status} ({url})"
        )));
    }
    let json: Value = resp.json().await.map_err(|e| {
        ApiError::Internal(format!("{label} JSON 解析失败: {e}"))
    })?;
    let type_name = if expected_list { "JSON 数组" } else { "JSON 对象" };
    if (expected_list && !json.is_array()) || (!expected_list && !json.is_object()) {
        return Err(ApiError::Internal(format!("{label}格式不正确，应为 {type_name}")));
    }
    Ok(json)
}

/// GET /api/repo/fetch — 代理获取远程任务仓库索引（返回 JSON 数组）
pub async fn repo_fetch_index(
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let index = repo_fetch_json(&url, true, "索引").await?;
    Ok(data(index))
}

/// GET /api/repo/task — 代理获取远程任务配置（返回 JSON 对象）
pub async fn repo_fetch_task(
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let task = repo_fetch_json(&url, false, "任务").await?;
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
