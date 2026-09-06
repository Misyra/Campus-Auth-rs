//! 版本检查：latest.json 拉取 + semver 比较 + 平台选择
//!
//! 负责从配置的发布源拉取 `latest.json`，按当前平台（`target_os`/`target_arch`）
//! 选择下载包，并通过 `semver` 比较判断是否存在可用更新。

use std::collections::HashMap;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config::UpdateChannel;
use crate::updater::error::UpdaterError;

/// 更新源 URL 白名单：`https` 任意主机放行（拒绝 userinfo）；`http` 仅精确回环主机
///
/// 拒绝字符串前缀校验（`http://127.0.0.1.evil.com` 曾被误放行）；
/// 回环白名单为精确 host：`127.0.0.1` / `localhost` / `::1`。
pub(crate) fn is_allowed_update_url(url_str: &str) -> bool {
    let Ok(url) = url::Url::parse(url_str) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        ),
        _ => false,
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// 远程版本号（serde 直接反序列化为 `semver::Version`）
    pub version: Version,
    /// 发布日期（"2026-07-15"），仅展示用
    #[serde(default)]
    pub release_date: Option<String>,
    /// 更新说明（中文），展示在前端
    #[serde(default)]
    pub changelog: Option<String>,
    /// 平台 → 下载包映射，键为 `"{os}-{arch}"`
    pub platforms: HashMap<String, PlatformPackage>,
}

/// 平台下载包信息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlatformPackage {
    /// zip 下载 URL（必须 HTTPS）
    pub url: String,
    /// 预期 SHA256 hex 摘要（64 字符小写）；为空时表示未取得校验值（降级信任 HTTPS）
    pub sha256: String,
    /// 预期文件大小（字节），用于进度计算
    #[serde(default)]
    pub size: Option<u64>,
}

/// 当前平台键（编译期常量），如 `"windows-x64"`
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "windows-x64";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "windows-arm64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-arm64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-x64";
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "unknown";

/// 发布清单默认 URL（配置为空时回退）
pub(crate) const DEFAULT_MANIFEST_URL: &str =
    "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest";
/// 清单拉取超时
pub(crate) const MANIFEST_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// 拉取并解析发布清单
///
/// 支持两种格式：
/// 1. 自定 `latest.json`（`{ version, platforms, changelog, ... }`）
/// 2. GitHub Release API（`{ tag_name, assets, body, ... }`）
///
/// `source_url` 为空时回退到 [`DEFAULT_MANIFEST_URL`]。
pub(crate) async fn fetch_manifest(
    client: &reqwest::Client,
    source_url: &str,
) -> Result<ReleaseManifest, UpdaterError> {
    let url = if source_url.is_empty() {
        DEFAULT_MANIFEST_URL
    } else {
        source_url
    };
    // 严格校验：https 放行，http 仅精确回环 host（拒绝前缀绕过与 userinfo）
    if !is_allowed_update_url(url) {
        return Err(UpdaterError::HttpsRequired(url.to_string()));
    }
    tracing::debug!(url = %url, "拉取发布清单");
    let response = client
        .get(url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;
    // 重定向收敛：最终 URL 仍须通过白名单（防 https→http evil 跳转）
    if !is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }

    // 处理 GitHub API 速率限制：未认证 REST 配额耗尽回 403 + 配额头耗尽
    // （GitHub 主限流按类型可能回 403 或 429），只认 429 会把主限流误报成
    // ManifestFetchFailed(403)，用户既看不懂也拿不到建议等待时间
    if matches!(
        response.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::FORBIDDEN
    ) {
        if let Some(retry_after) = rate_limit_retry_after(response.headers()) {
            return Err(UpdaterError::RateLimited { retry_after });
        }
        // 429 但无配额头（代理/网关限流）：沿用 retry-after 头，缺省 60s
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            return Err(UpdaterError::RateLimited { retry_after });
        }
    }

    let response = response
        .error_for_status()
        .map_err(UpdaterError::ManifestFetchFailed)?;
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;

    // 格式 1：自定 latest.json（含 version 字段）
    if body.get("version").is_some() {
        let manifest: ReleaseManifest =
            serde_json::from_value(body).map_err(UpdaterError::ManifestParseFailed)?;
        tracing::debug!(url = %url, version = %manifest.version, "已获取发布清单");
        return Ok(manifest);
    }

    // 格式 2：GitHub Release API（含 tag_name 字段）
    if body.get("tag_name").and_then(|v| v.as_str()).is_some() {
        return manifest_from_github_release(client, &body).await;
    }

    Err(UpdaterError::ManifestParseFailed(serde::de::Error::custom(
        "无法识别的发布清单格式：既无 version 也无 tag_name",
    )))
}

/// 将单个 GitHub Release JSON 对象转换为发布清单
///
/// 复用[`fetch_manifest`]的 tag_name 分支与通道列表路径：解析版本号，逐平台
/// 资产拉取 `.sha256` 伴随文件（缺失的平台包直接跳过，见 G11/G12）。
async fn manifest_from_github_release(
    client: &reqwest::Client,
    body: &serde_json::Value,
) -> Result<ReleaseManifest, UpdaterError> {
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            UpdaterError::ManifestParseFailed(serde::de::Error::custom("缺少 tag_name 字段"))
        })?;
    let version_str = tag.strip_prefix('v').unwrap_or(tag);
    let version = Version::parse(version_str).map_err(UpdaterError::VersionParseFailed)?;
    let release_date = body
        .get("published_at")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let changelog = body
        .get("body")
        .and_then(|v| v.as_str())
        .map(|v| v.to_owned());
    let assets: Vec<serde_json::Value> = body
        .get("assets")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let mut platforms: HashMap<String, PlatformPackage> = HashMap::new();
    // 仅拉取当前平台的校验信息：检查失败一条日志足够，且避免多平台全拉的刷屏与
    // 额外 sha256 伴随文件请求（旧逻辑遍历全部平台，每平台还重试一次）。
    if let Some((_, dl_url, size, name)) = collect_current_platform_asset(&assets) {
        let asset_refs: Vec<&serde_json::Value> = assets.iter().collect();
        let sha256 = fetch_sha256_assoc(client, &asset_refs, &name).await;
        if !sha256.is_empty() {
            platforms.insert(
                CURRENT_PLATFORM_KEY.to_string(),
                PlatformPackage {
                    url: dl_url,
                    sha256,
                    size,
                },
            );
        }
    }
    if platforms.is_empty() {
        return Err(UpdaterError::PlatformNotAvailable(
            "发布中未找到平台下载包".into(),
        ));
    }
    Ok(ReleaseManifest {
        version,
        release_date,
        changelog,
        platforms,
    })
}

/// 从发布源 URL 推导 GitHub Releases 列表 API 地址（通道枚举用）
///
/// 来源为 `.../releases/latest` 结尾的路径时返回 `.../releases?per_page=100`，
/// 主机不限（GitHub / 自建镜像 / 回环测试源均可）；拒绝 userinfo。
/// 其余形态（自定 latest.json 镜像等）无法枚举历史发布，返回 `None`
/// 由调用方回退单清单语义。
fn releases_list_url(source_url: &str) -> Option<String> {
    let url = url::Url::parse(source_url).ok()?;
    // 形态匹配不限定主机：自建镜像（https 任意主机）与回环测试源
    // （http 仅精确回环）的信任边界由派生后列表 URL 的白名单复核兜底，
    // 与单清单路径（fetch_manifest 对 source_url 的校验）同一信任级别
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let segments: Vec<&str> = url.path().trim_end_matches('/').split('/').collect();
    // 期望形如 /{owner}/{repo}/releases/latest（或带前置路径的同类结构）
    if segments.len() < 3
        || *segments.last()? != "latest"
        || *segments.iter().nth_back(1)? != "releases"
    {
        return None;
    }
    let mut list = url.clone();
    // 去掉末尾 "latest" 段后即为 .../releases 路径（segments[0] 为空前导空段）
    list.set_path(&segments[..segments.len() - 1].join("/"));
    list.set_query(Some("per_page=100"));
    Some(list.to_string())
}

/// 拉取发布清单并按更新通道筛选
///
/// - [`UpdateChannel::Stable`]：与旧语义一致，直接拉 `source_url`（releases/latest
///   仅返回正式发布）；
/// - [`UpdateChannel::Prerelease`]：枚举 Releases 列表，取 semver 最高的预发布；
///   无任何预发布时回退正式版清单（避免测试版通道用户长期无更新可检）；
/// - [`UpdateChannel::All`]：枚举列表取 semver 最高者（正式/预发布一起比）。
///
/// 列表枚举依赖 GitHub API 地址形态；自定义 latest.json 镜像无法枚举，
/// 记 warn 后回退单清单（通道降级为"跟随该清单"）。
pub(crate) async fn fetch_manifest_for_channel(
    client: &reqwest::Client,
    source_url: &str,
    channel: UpdateChannel,
) -> Result<ReleaseManifest, UpdaterError> {
    if channel == UpdateChannel::Stable {
        return fetch_manifest(client, source_url).await;
    }
    let Some(list_url) = releases_list_url(if source_url.is_empty() {
        DEFAULT_MANIFEST_URL
    } else {
        source_url
    }) else {
        tracing::warn!(
            source = %source_url,
            "更新源不支持通道枚举（非 releases/latest 形态），回退单清单"
        );
        return fetch_manifest(client, source_url).await;
    };
    if !is_allowed_update_url(&list_url) {
        return Err(UpdaterError::HttpsRequired(list_url));
    }
    tracing::debug!(url = %list_url, channel = ?channel, "拉取 Releases 列表");
    let response = client
        .get(&list_url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;
    if !is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }
    let response = response
        .error_for_status()
        .map_err(UpdaterError::ManifestFetchFailed)?;
    let releases: Vec<serde_json::Value> = response
        .json()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;

    // 过滤草稿后按通道筛选，取 semver 最高的候选（列表按创建时间排序，
    // 直接取首项可能选中回迁的旧版本发布）
    let best = select_release_for_channel(&releases, channel);
    let Some(best) = best else {
        // 测试版通道下无任何预发布：回退正式版清单（releases/latest）
        if channel == UpdateChannel::Prerelease {
            tracing::info!("测试版通道下远程无预发布，回退正式版清单");
            return fetch_manifest(client, source_url).await;
        }
        return Err(UpdaterError::PlatformNotAvailable(
            "Releases 列表为空".into(),
        ));
    };
    manifest_from_github_release(client, best).await
}

/// 按通道从 Releases 列表中选取目标发布（纯函数，便于单测）
///
/// 过滤草稿；`Stable` 仅保留非预发布，`Prerelease` 仅保留 prerelease=true，
/// `All` 全保留；返回 semver 最高的候选（tag 无法解析的条目跳过），
/// 无候选返回 `None`。
fn select_release_for_channel(
    releases: &[serde_json::Value],
    channel: UpdateChannel,
) -> Option<&serde_json::Value> {
    releases
        .iter()
        .filter(|r| !r.get("draft").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter(|r| {
            let pre = r
                .get("prerelease")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match channel {
                UpdateChannel::Stable => !pre,
                UpdateChannel::Prerelease => pre,
                UpdateChannel::All => true,
            }
        })
        .filter_map(|r| {
            let tag = r.get("tag_name").and_then(|v| v.as_str())?;
            Version::parse(tag.strip_prefix('v').unwrap_or(tag))
                .ok()
                .map(|v| (v, r))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r)
}

/// 从资产文件名推断平台键（`"{os}-{arch}"`，G11）
///
/// windows / linux 按 `x86_64|x64|amd64` 与 `aarch64|arm64|arm` 关键字区分
/// 架构；macos 保持旧语义（无架构词默认 x64，兼容 universal 包按 x64 归类）。
/// 返回 `None` 表示无法识别的组合，调用方 warn 后跳过。
fn infer_platform_key(name: &str) -> Option<&'static str> {
    let is_x64 = name.contains("x86_64") || name.contains("x64") || name.contains("amd64");
    let is_arm = name.contains("aarch64") || name.contains("arm64") || name.contains("arm");
    if name.contains("windows") {
        match (is_x64, is_arm) {
            (true, _) => Some("windows-x64"),
            (_, true) => Some("windows-arm64"),
            // windows 资产必须带架构词：盲目归入固定架构会让 arm64 顶掉 x64
            _ => None,
        }
    } else if name.contains("linux") {
        match (is_x64, is_arm) {
            (true, _) => Some("linux-x64"),
            (_, true) => Some("linux-arm64"),
            _ => None,
        }
    } else if name.contains("macos") || name.contains("darwin") {
        if is_arm {
            Some("macos-arm64")
        } else {
            Some("macos-x64")
        }
    } else {
        None
    }
}

/// 从响应头判定是否为配额耗尽，并折算建议等待秒数
///
/// GitHub 限流响应携带 `X-RateLimit-Remaining: 0`；`X-RateLimit-Reset` 为
/// 配额重置的 Unix 秒时间戳。未认证 REST 超限通常回 403（而非 429），
/// 故 403 也需走此判定；无 `X-RateLimit-Reset` 时回退 60s，reset 时间过去/
/// 过远分别夹取为 1s / 3600s。纯函数（只读 HeaderMap）便于单测覆盖各组合。
fn rate_limit_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let remaining_is_zero = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.trim() == "0");
    if !remaining_is_zero {
        return None;
    }
    let until_reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .map(|reset| (reset - chrono::Utc::now().timestamp()).clamp(1, 3600) as u64);
    Some(until_reset.unwrap_or(60))
}

/// 从 GitHub release assets 中提取当前平台包（纯函数，G11 便于单测）
///
/// 跳过 `.sha256` 伴随文件本身与无法推断平台键的资产；返回与
/// [`CURRENT_PLATFORM_KEY`] 对应的那一个（下载 URL/大小/小写资产名）。
/// 无匹配返回 `None`（调用方据此判定当前平台不可用）。
fn collect_current_platform_asset(
    assets: &[serde_json::Value],
) -> Option<(String, String, Option<u64>, String)> {
    for asset in assets {
        let name = asset
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if name.ends_with(".sha256") {
            continue;
        }
        let key = match infer_platform_key(&name) {
            Some(k) => k,
            None => {
                tracing::debug!("更新源资产 {name} 不含可识别的平台/架构标识，跳过");
                continue;
            }
        };
        if key != CURRENT_PLATFORM_KEY {
            continue;
        }
        let dl_url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let size = asset.get("size").and_then(|v| v.as_u64());
        return Some((key.to_string(), dl_url, size, name));
    }
    None
}

/// 从 GitHub release assets 中提取平台压缩包（zip / tar.gz，纯函数，兼容 `collect_current_platform_asset`）
///
/// - `.sha256` 伴随文件不是下载包本身，跳过；
/// - 无法推断平台键的资产 warn 后跳过；
/// - 返回 `(平台键, 下载 URL, 大小, 小写资产名)` 列表。
#[allow(dead_code)]
pub(crate) fn collect_package_assets(
    assets: &[serde_json::Value],
) -> Vec<(String, String, Option<u64>, String)> {
    let mut result = Vec::new();
    for asset in assets {
        let name = asset
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if name.ends_with(".sha256") {
            continue;
        }
        let Some(key) = infer_platform_key(&name) else {
            // 旧命名规范的资产（无平台/架构关键字，如 4.x 的 campus-auth-4.2.3.zip）
            // 属发布源常态，每次检查都会经过这里，降为 debug 避免反复刷 WARN
            tracing::debug!("更新源资产 {name} 不含可识别的平台/架构标识，跳过");
            continue;
        };
        let dl_url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let size = asset.get("size").and_then(|v| v.as_u64());
        result.push((key.to_string(), dl_url, size, name));
    }
    result
}

/// 从 GitHub release assets 中查找 zip 对应的 `.sha256` 伴随文件并下载其内容
///
/// 返回伴随文件首行首个空白分隔字段（即哈希值）；找不到 / 下载失败时返回空串
/// （调用方据此降级为信任 HTTPS）。
async fn fetch_sha256_assoc(
    client: &reqwest::Client,
    assets: &[&serde_json::Value],
    zip_name: &str,
) -> String {
    let assoc_name = format!("{zip_name}.sha256");
    let asset = assets.iter().find(|a| {
        a.get("name")
            .and_then(|v| v.as_str())
            .map(|n| n.to_lowercase() == assoc_name)
            .unwrap_or(false)
    });
    let Some(asset) = asset else {
        return String::new();
    };
    let url = asset
        .get("browser_download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if url.is_empty() {
        return String::new();
    }
    // 当前平台的校验失败不重试：网络错误或空体直接返回空串，调用方仅记一条
    // 失败原因，不刷屏（旧逻辑重试一次并固定 WARN）。
    let resp = match client
        .get(url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    let resp = match resp.error_for_status() {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    match resp.text().await {
        Ok(t) => t.split_whitespace().next().unwrap_or("").to_string(),
        Err(_) => String::new(),
    }
}

/// 按当前平台选择下载包
pub(crate) fn select_platform(manifest: &ReleaseManifest) -> Option<&PlatformPackage> {
    manifest.platforms.get(CURRENT_PLATFORM_KEY)
}

/// 判断远程版本是否对当前版本构成"感兴趣"的更新
///
/// 统一通道：仅按 semver 大小比较，`remote > current` 即视为更新。
/// 此前按 `alpha`/`beta` 前缀隔离的逻辑会导致 `5.0.0-alpha.5 → 5.0.0`
/// 这类正式版升级被误判为不感兴趣；现改为“哪个高收哪个”，预发布
/// 与正式版在同一比较通道内按 semver 排序（`5.0.0 > 5.0.0-alpha.*`）。
pub(crate) fn compare_versions(current: &Version, remote: &Version) -> bool {
    *remote > *current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("测试用合法版本号")
    }

    /// 远程版本不新于当前版本时一律不更新
    #[test]
    fn test_compare_versions_not_newer() {
        assert!(!compare_versions(&v("1.0.0"), &v("0.9.0")));
        assert!(!compare_versions(&v("1.0.0"), &v("1.0.0")));
        // 预发布 < 正式版，即使版本号数值前缀相同
        assert!(!compare_versions(&v("1.0.0"), &v("1.0.0-alpha")));
    }

    /// 当前为正式版：接受任何更新的远程版本
    #[test]
    fn test_compare_versions_stable_accepts_any_newer() {
        assert!(compare_versions(&v("1.0.0"), &v("1.0.1")));
        assert!(compare_versions(&v("1.0.0"), &v("2.0.0")));
        // 远程为预发布且不新于当前正式版 → 拒绝（1.0.0-beta.1 < 1.0.0）
        assert!(!compare_versions(&v("1.0.0"), &v("1.0.0-beta.1")));
        // 远程预发布版号高于当前正式版号 → 接受
        assert!(compare_versions(&v("1.0.0"), &v("1.1.0-beta.1")));
    }

    /// 统一通道：预发布与正式版按 semver 大小一起比较
    #[test]
    fn test_compare_versions_prerelease_prefix_match() {
        // 同前缀递增 → 接受
        assert!(compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0-alpha.2")));
        assert!(compare_versions(&v("5.0.0-alpha"), &v("5.0.0-alpha.1")));
        // 跨通道只要 semver 更大即接受（此前隔离，现统一）
        assert!(compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0-beta.1")));
        assert!(compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0")));
        assert!(compare_versions(&v("5.0.0-beta.1"), &v("5.0.0")));
        // 旧正式版 → 新预发布（版号更大）也接受
        assert!(compare_versions(&v("5.0.0"), &v("5.1.0-alpha.1")));
    }

    /// 平台选择：命中当前平台键返回对应包，否则 None
    #[test]
    fn test_select_platform() {
        let mut platforms = HashMap::new();
        platforms.insert(
            CURRENT_PLATFORM_KEY.to_string(),
            PlatformPackage {
                url: "https://example.com/pkg.zip".into(),
                sha256: String::new(),
                size: None,
            },
        );
        let manifest = ReleaseManifest {
            version: v("1.0.0"),
            release_date: None,
            changelog: None,
            platforms,
        };
        let picked = select_platform(&manifest).expect("当前平台应有下载包");
        assert_eq!(picked.url, "https://example.com/pkg.zip");

        let empty = ReleaseManifest {
            version: v("1.0.0"),
            release_date: None,
            changelog: None,
            platforms: HashMap::new(),
        };
        assert!(select_platform(&empty).is_none(), "无匹配平台应返回 None");
    }

    /// 预发布标识符首分量解析（pre_first 的间接验证）
    #[test]
    fn test_compare_versions_same_major_different_minor() {
        // 预发布链中版本号本身也在推进，须同时满足"更新"与"前缀一致"
        assert!(compare_versions(&v("5.0.0-alpha.1"), &v("5.1.0-alpha.1")));
    }

    /// G11：平台键按架构区分——windows/linux 资产须带架构词，
    /// 多资产 release 中 x64 与 arm64 各占一键互不覆盖
    #[test]
    fn test_infer_platform_key_arch_aware() {
        // windows 区分 x64 / arm64
        assert_eq!(
            infer_platform_key("campus-auth-windows-x64.zip"),
            Some("windows-x64")
        );
        assert_eq!(
            infer_platform_key("campus-auth_5.0.0_windows_arm64.zip"),
            Some("windows-arm64")
        );
        assert_eq!(
            infer_platform_key("campus-auth-aarch64-pc-windows-msvc.zip"),
            Some("windows-arm64")
        );
        // linux 区分 x64 / arm64
        assert_eq!(
            infer_platform_key("campus-auth-linux-x86_64.zip"),
            Some("linux-x64")
        );
        assert_eq!(
            infer_platform_key("campus-auth-linux-arm64.zip"),
            Some("linux-arm64")
        );
        assert_eq!(
            infer_platform_key("campus-auth-aarch64-unknown-linux-gnu.zip"),
            Some("linux-arm64")
        );
        // macos：arm 词归 arm64，无架构词默认 x64（兼容 universal）
        assert_eq!(
            infer_platform_key("campus-auth-macos-arm64.zip"),
            Some("macos-arm64")
        );
        assert_eq!(
            infer_platform_key("campus-auth-darwin-x64.zip"),
            Some("macos-x64")
        );
        assert_eq!(
            infer_platform_key("campus-auth-macos-universal.zip"),
            Some("macos-x64")
        );
        // windows/linux 无架构词 → 无法识别（None，调用方 warn 跳过）
        assert_eq!(infer_platform_key("campus-auth-windows.zip"), None);
        assert_eq!(infer_platform_key("campus-auth-linux.zip"), None);
        // 非 OS 关键字
        assert_eq!(infer_platform_key("checksums.txt"), None);
    }

    /// G11：多资产 release 中 arm64 不得顶掉 x64（HashMap 键并存）
    #[test]
    fn test_collect_package_assets_multi_arch_coexist() {
        let assets: Vec<serde_json::Value> = [
            r#"{"name": "Campus-Auth-5.0.0-windows-x64.zip", "browser_download_url": "https://x64", "size": 100}"#,
            r#"{"name": "Campus-Auth-5.0.0-windows-arm64.zip", "browser_download_url": "https://arm64", "size": 90}"#,
            r#"{"name": "Campus-Auth-5.0.0-macos-arm64.zip", "browser_download_url": "https://mac", "size": 80}"#,
            // 伴随 sha256 文件与无法识别架构的资产都应被过滤
            r#"{"name": "Campus-Auth-5.0.0-windows-x64.zip.sha256", "browser_download_url": "https://sha"}"#,
            r#"{"name": "Campus-Auth-5.0.0-windows.zip", "browser_download_url": "https://unknown-arch"}"#,
        ]
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();

        let mut collected = collect_package_assets(&assets);
        collected.sort();
        assert_eq!(collected.len(), 3, "sha256 伴随文件与无架构资产应被跳过");
        // 三个平台键各自独立，arm64 不再覆盖 x64
        let keys: Vec<&str> = collected.iter().map(|(k, ..)| k.as_str()).collect();
        assert_eq!(keys, vec!["macos-arm64", "windows-arm64", "windows-x64"]);
        let x64 = collected.iter().find(|(k, ..)| k == "windows-x64").unwrap();
        assert_eq!(x64.1, "https://x64");
    }

    /// URL 白名单：https 放行，http 仅精确回环，拒绝前缀绕过与 userinfo
    #[test]
    fn test_is_allowed_update_url() {
        assert!(is_allowed_update_url("https://example.com/latest.json"));
        assert!(is_allowed_update_url("https://127.0.0.1.evil.com/x"));
        assert!(is_allowed_update_url("http://127.0.0.1:8765/latest.json"));
        assert!(is_allowed_update_url("http://localhost:8765/x.zip"));
        assert!(is_allowed_update_url("http://[::1]:8765/x"));
        assert!(!is_allowed_update_url("http://127.0.0.1.evil.com/x"));
        assert!(!is_allowed_update_url("http://localhost.evil.com/x"));
        assert!(!is_allowed_update_url("http://127.0.0.1@evil.com/x"));
        assert!(!is_allowed_update_url("http://example.com/x"));
        assert!(!is_allowed_update_url("ftp://example.com/x"));
        assert!(!is_allowed_update_url("https://user:pass@example.com/x"));
        assert!(!is_allowed_update_url("not a url"));
    }

    /// 配额头判定：剩余非 0 / 无头 → None；剩余 0 时按 reset 折算并夹取
    #[test]
    fn test_rate_limit_retry_after() {
        use reqwest::header::{HeaderMap, HeaderValue};

        // 无任何头 → None
        let h = HeaderMap::new();
        assert_eq!(rate_limit_retry_after(&h), None);
        // 剩余非 0 → None（正常限流响应不触发）
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("59"));
        assert_eq!(rate_limit_retry_after(&h), None);

        // 剩余 0 但无 reset → 回退 60s
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        assert_eq!(rate_limit_retry_after(&h), Some(60));

        // 剩余 0 + reset 在过去 → 夹取为 1s
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1"));
        assert_eq!(rate_limit_retry_after(&h), Some(1));

        // 剩余 0 + reset 在远未来 → 夹取为 3600s
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        let far = chrono::Utc::now().timestamp() + 999_999;
        h.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&far.to_string()).unwrap(),
        );
        assert_eq!(rate_limit_retry_after(&h), Some(3600));
    }

    /// Releases 列表 URL 推导：releases/latest 形态 → 同源 releases 列表；
    /// 非 latest 形态 / 带 userinfo → None；主机不限（镜像/回环源均可枚举）
    #[test]
    fn test_releases_list_url() {
        let derived = releases_list_url(DEFAULT_MANIFEST_URL).expect("标准来源应可推导");
        assert!(derived.starts_with("https://api.github.com/repos/Misyra/Campus-Auth-rs/releases"));
        assert!(derived.contains("per_page=100"));
        assert!(!derived.ends_with("/latest"), "列表地址不得仍指向 latest");

        // 自建镜像（https 任意主机）同形态 → 可枚举
        let mirror = releases_list_url("https://mirror.example.com/repos/x/y/releases/latest")
            .expect("镜像应可推导");
        assert!(mirror.starts_with("https://mirror.example.com/repos/x/y/releases"));
        // 回环测试源（http 仅回环放行）同形态 → 可枚举
        let loopback = releases_list_url("http://127.0.0.1:18766/repos/o/r/releases/latest")
            .expect("回环源应可推导");
        assert!(loopback.starts_with("http://127.0.0.1:18766/repos/o/r/releases"));

        // 非 releases/latest 形态（自定 latest.json）→ None
        assert!(releases_list_url("https://api.github.com/repos/x/y/releases").is_none());
        assert!(releases_list_url("https://api.github.com/latest.json").is_none());
        // 带 userinfo → None
        assert!(
            releases_list_url("https://user:pass@api.github.com/repos/x/y/releases/latest")
                .is_none()
        );
        // 非法字符串 → None
        assert!(releases_list_url("not a url").is_none());
    }

    /// 通道选取：测试版仅取 prerelease、全通道取 semver 最高、草稿一律跳过
    #[test]
    fn test_select_release_for_channel() {
        let releases: Vec<serde_json::Value> = [
            // 创建时间序（GitHub 列表顺序）：最新创建的是旧版本号 → 必须按 semver 取最大
            r#"{"tag_name": "v5.0.0-alpha.7", "prerelease": true, "draft": false}"#,
            r#"{"tag_name": "v5.0.0", "prerelease": false, "draft": false}"#,
            r#"{"tag_name": "v5.1.0-beta.1", "prerelease": true, "draft": false}"#,
            // 草稿不参与选取
            r#"{"tag_name": "v6.0.0", "prerelease": false, "draft": true}"#,
            // tag 无法解析的条目跳过
            r#"{"tag_name": "legacy-tag", "prerelease": true, "draft": false}"#,
        ]
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();

        // 正式版：semver 最高的非草稿非预发布 → v5.0.0（5.1.0-beta.1 是预发布不入选）
        let stable = select_release_for_channel(&releases, UpdateChannel::Stable).unwrap();
        assert_eq!(stable["tag_name"], "v5.0.0");

        // 测试版：仅预发布中 semver 最高 → v5.1.0-beta.1
        let pre = select_release_for_channel(&releases, UpdateChannel::Prerelease).unwrap();
        assert_eq!(pre["tag_name"], "v5.1.0-beta.1");

        // 全通道：正式 + 预发布一起比 → v5.1.0-beta.1
        let all = select_release_for_channel(&releases, UpdateChannel::All).unwrap();
        assert_eq!(all["tag_name"], "v5.1.0-beta.1");

        // 测试版通道下无预发布 → None（调用方回退正式版清单）
        let stable_only = vec![
            serde_json::from_str::<serde_json::Value>(
                r#"{"tag_name": "v5.0.0", "prerelease": false, "draft": false}"#,
            )
            .unwrap(),
        ];
        assert!(select_release_for_channel(&stable_only, UpdateChannel::Prerelease).is_none());
        // 空列表 → None
        assert!(select_release_for_channel(&[], UpdateChannel::All).is_none());
    }
}
