//! 版本检查：latest.json 拉取 + semver 比较 + 平台选择
//!
//! 负责从配置的发布源拉取 `latest.json`，按当前平台（`target_os`/`target_arch`）
//! 选择下载包，并通过 `semver` 比较判断是否存在可用更新。

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use semver::Version;

use crate::updater::error::UpdaterError;

/// 发布清单（latest.json）数据模型
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
    /// 预期 SHA256 hex 摘要（64 字符小写）
    pub sha256: String,
    /// 预期文件大小（字节），用于进度计算
    #[serde(default)]
    pub size: Option<u64>,
    /// 签名文件 URL（未来支持签名验证）
    #[serde(default)]
    pub sig_url: Option<String>,
}

/// 当前平台键（编译期常量），如 `"windows-x64"`
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "windows-x64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-x64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-x64";
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "unknown";

/// 发布清单默认 URL（配置为空时回退）
pub(crate) const DEFAULT_MANIFEST_URL: &str =
    "https://api.github.com/repos/Misyra/Campus-Auth/releases/latest";
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
    let response = client
        .get(url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;

    // 处理 GitHub API 速率限制（429 Too Many Requests）
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        return Err(UpdaterError::RateLimited { retry_after });
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
        return Ok(manifest);
    }

    // 格式 2：GitHub Release API（含 tag_name 字段）
    if let Some(tag) = body.get("tag_name").and_then(|v| v.as_str()) {
        let version_str = tag.strip_prefix('v').unwrap_or(tag);
        let version = Version::parse(version_str)
            .map_err(UpdaterError::VersionParseFailed)?;
        let release_date = body
            .get("published_at")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        let changelog = body
            .get("body")
            .and_then(|v| v.as_str())
            .map(|v| v.to_owned());
        let assets = body
            .get("assets")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten();
        let mut platforms: HashMap<String, PlatformPackage> = HashMap::new();
        for asset in assets {
            let name = asset
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let dl_url = asset
                .get("browser_download_url")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let size = asset.get("size").and_then(|v| v.as_u64());
            // 从 asset 文件名推断平台键
            let platform_key = if name.contains("windows") {
                "windows-x64"
            } else if name.contains("linux") {
                "linux-x64"
            } else if name.contains("macos") || name.contains("darwin") {
                if name.contains("arm") || name.contains("aarch64") {
                    "macos-arm64"
                } else {
                    "macos-x64"
                }
            } else {
                continue;
            };
            platforms.insert(
                platform_key.to_string(),
                PlatformPackage {
                    url: dl_url.to_string(),
                    sha256: String::new(), // GitHub API 不含 SHA256，下载后需另行校验
                    size,
                    sig_url: None,
                },
            );
        }
        if platforms.is_empty() {
            return Err(UpdaterError::PlatformNotAvailable(
                "发布中未找到平台下载包".into(),
            ));
        }
        return Ok(ReleaseManifest {
            version,
            release_date,
            changelog,
            platforms,
        });
    }

    Err(UpdaterError::ManifestParseFailed(serde::de::Error::custom(
        "无法识别的发布清单格式：既无 version 也无 tag_name",
    )))
}

/// 按当前平台选择下载包
pub(crate) fn select_platform(manifest: &ReleaseManifest) -> Option<&PlatformPackage> {
    manifest.platforms.get(CURRENT_PLATFORM_KEY)
}

/// 取版本预发布标识符的第一个分量（如 `alpha`）的字符串表示
fn pre_first(version: &Version) -> Option<String> {
    let pre = version.pre.as_str();
    if pre.is_empty() {
        return None;
    }
    // 取第一个 `.` 分隔的分量（如 `alpha.1` → `alpha`）
    Some(pre.split('.').next().unwrap_or(pre).to_string())
}

/// 判断远程版本是否对当前版本构成"感兴趣"的更新
///
/// - 远程版本不新于当前版本 → `false`
/// - 当前为正式版（无预发布标签）→ 接受任何更新的远程版本
/// - 当前为预发布版 → 仅接受预发布标识符前缀一致的远程版本
///   （例如当前 `alpha`，则仅匹配 `alpha.*`，不匹配 `beta` 或正式版）
pub(crate) fn compare_versions(current: &Version, remote: &Version) -> bool {
    if *remote <= *current {
        return false;
    }
    if current.pre.is_empty() {
        return true;
    }
    match (pre_first(current), pre_first(remote)) {
        (Some(c), Some(r)) => c == r,
        _ => false,
    }
}
