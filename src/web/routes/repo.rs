//! 仓库代理路由：代理获取远程任务仓库索引和任务配置，避免前端跨域问题
//!
//! 参考原版 `app/api/repo.py` + `app/utils/repo_proxy.py`，Rust 重写版。
//! 提供 SSRF 防护（仅 http/https、拒绝私有/保留地址）、GitHub/Gitee blob→raw 归一化。

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};

fn repo_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("Campus-Auth")
            .build()
            .expect("构建 HTTP 客户端失败")
    })
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

/// SSRF 防护：校验 URL scheme、解析地址，拒绝访问内网/保留地址
async fn validate_remote_url(url: &str) -> Result<String, ApiError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ApiError::BadRequest(format!("无效的 URL: {e}")))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ApiError::BadRequest(format!(
            "不支持的 URL 协议: {scheme}，仅支持 http/https"
        )));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest("URL 缺少主机名".to_string()))?;

    // IP 字面量：直接检查
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_restricted(ip) {
            return Err(ApiError::BadRequest(format!("禁止访问内网/保留地址: {host}")));
        }
        return Ok(url.to_string());
    }

    // 域名：DNS 解析后检查；解析失败时拒绝请求
    let addrs = tokio::net::lookup_host((host, 0u16))
        .await
        .map_err(|_| ApiError::BadRequest(format!("DNS 解析失败，拒绝访问: {host}")))?;
    for addr in addrs {
        if is_restricted(addr.ip()) {
            return Err(ApiError::BadRequest(format!("禁止访问内网/保留地址: {host}")));
        }
    }

    Ok(url.to_string())
}

fn is_restricted(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_link_local()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
        }
    }
}

/// 获取远程 JSON 并校验类型（数组或对象）
async fn repo_fetch_json(url: &str, expected_list: bool, label: &str) -> Result<Value, ApiError> {
    let client = repo_client();
    let resp = client.get(url).send().await.map_err(|e| {
        ApiError::ServiceUnavailable(format!("获取{label}失败: {e}"))
    })?;
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
    let _ = validate_remote_url(&url).await?;
    let index = repo_fetch_json(&url, true, "索引").await?;
    Ok(data(index))
}

/// GET /api/repo/task — 代理获取远程任务配置（返回 JSON 对象）
pub async fn repo_fetch_task(
    Query(params): Query<RepoUrlQuery>,
) -> Result<Json<Value>, ApiError> {
    let url = normalize_repo_url(&params.url);
    let _ = validate_remote_url(&url).await?;
    let task = repo_fetch_json(&url, false, "任务").await?;
    Ok(data(task))
}

#[derive(Deserialize)]
pub struct RepoUrlQuery {
    pub url: String,
}
