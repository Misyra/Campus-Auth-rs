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
                // fe80::/10 链路本地地址：内网可寻址，必须拦截（SSRF 缺口修复）
                || v6.is_unicast_link_local()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // ============ SSRF 防护：is_restricted ============

    #[test]
    fn test_is_restricted_rejects_private_and_reserved_ipv4() {
        // 私有地址段
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        // 回环 / 未指定 / 链路本地 / 组播 / 广播
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        assert!(is_restricted(IpAddr::V4(Ipv4Addr::BROADCAST)));
    }

    #[test]
    fn test_is_restricted_rejects_reserved_ipv6() {
        assert!(is_restricted(IpAddr::V6(Ipv6Addr::LOCALHOST))); // ::1
        assert!(is_restricted(IpAddr::V6(Ipv6Addr::UNSPECIFIED))); // ::
        assert!(is_restricted(IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)))); // fc00::
        assert!(is_restricted(IpAddr::V6("fe80::1".parse().unwrap()))); // 链路本地
        assert!(is_restricted(IpAddr::V6("ff02::1".parse().unwrap()))); // 组播
    }

    #[test]
    fn test_is_restricted_allows_public_addresses() {
        assert!(!is_restricted(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_restricted(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_restricted(IpAddr::V6("2606:4700:4700::1111".parse().unwrap())));
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
