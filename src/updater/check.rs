//! 版本检查：latest.json 拉取 + semver 比较 + 平台选择
//!
//! 负责从配置的发布源拉取 `latest.json`，按当前平台（`target_os`/`target_arch`）
//! 选择下载包，并通过 `semver` 比较判断是否存在可用更新。

use std::collections::HashMap;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

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
        // G11：抽纯函数按 "os-arch" 推断平台键，多资产 release 中
        // windows-x64 / windows-arm64 各占一键互不覆盖
        for (platform_key, dl_url, size, name) in collect_package_assets(&assets) {
            // 从 release assets 中找 `.sha256` 伴随文件，拿到真实哈希（U2：默认 GitHub
            // 源此前 sha256 恒为空，更新包无任何完整性校验）
            let asset_refs: Vec<&serde_json::Value> = assets.iter().collect();
            let sha256 =
                fetch_sha256_with_retry(|| fetch_sha256_assoc(client, &asset_refs, &name)).await;
            if sha256.is_empty() {
                tracing::warn!(
                    "GitHub 发布中未找到 {name} 的 .sha256 伴随文件（重试后仍为空），\
                     降级为信任 HTTPS"
                );
            }
            platforms.insert(
                platform_key,
                PlatformPackage {
                    url: dl_url,
                    sha256,
                    size,
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

/// 从 GitHub release assets 中提取平台压缩包（zip / tar.gz，纯函数，G11 便于单测）
///
/// 过滤规则：
/// - `.sha256` 伴随文件不是下载包本身，跳过（由 `fetch_sha256_assoc` 使用）；
/// - 无法推断平台键（含 `os` 但无法识别 `arch`）的资产 warn 后跳过。
///
/// 返回 `(平台键, 下载 URL, 大小, 小写资产名)` 列表；资产名供后续
/// `.sha256` 伴随文件查找复用。
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

/// 拉取伴随 `.sha256` 内容，为空/缺失时重试一次（G12）
///
/// 抽成泛型小函数便于单测重试决策：首次成功非空只拉一次；
/// 为空（镜像返回空体/未命中伴随文件）再拉一次，仍为空由调用方降级。
async fn fetch_sha256_with_retry<F, Fut>(mut fetch: F) -> String
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let first = fetch().await;
    if !first.is_empty() {
        return first;
    }
    tracing::warn!("伴随 .sha256 拉取为空，重试一次");
    fetch().await
}

/// 从 GitHub release assets 中查找 zip 对应的 `.sha256` 伴随文件并下载其内容
///
/// 返回伴随文件首行首个空白分隔字段（即哈希值）；找不到 / 下载失败时返回空串
/// （调用方据此降级为信任 HTTPS）。
/// pub(crate)：environment/git.rs（MinGit 校验，R3）复用同一伴随文件模式。
pub(crate) async fn fetch_sha256_assoc(
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
    let resp = match client
        .get(url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("下载 .sha256 伴随文件失败 {url}: {e}");
            return String::new();
        }
    };
    let resp = match resp.error_for_status() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("下载 .sha256 伴随文件失败 {url}: {e}");
            return String::new();
        }
    };
    match resp.text().await {
        Ok(t) => t.split_whitespace().next().unwrap_or("").to_string(),
        Err(e) => {
            tracing::warn!("读取 .sha256 伴随文件失败 {url}: {e}");
            String::new()
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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

    /// 当前为预发布版：仅接受预发布标识符前缀一致的远程版本
    #[test]
    fn test_compare_versions_prerelease_prefix_match() {
        // alpha 前缀一致 → 接受
        assert!(compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0-alpha.2")));
        assert!(compare_versions(&v("5.0.0-alpha"), &v("5.0.0-alpha.1")));
        // 前缀不一致 → 拒绝（alpha → beta / 正式版）
        assert!(!compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0-beta.1")));
        assert!(!compare_versions(&v("5.0.0-alpha.1"), &v("5.0.0")));
        assert!(!compare_versions(&v("5.0.0-beta.1"), &v("5.0.0-alpha.2")));
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

    /// G12：伴随 .sha256 首次拉取非空只拉一次；为空时重试一次
    #[tokio::test]
    async fn test_fetch_sha256_with_retry() {
        use std::sync::atomic::{AtomicU32, Ordering};

        // 首次即非空：只调用一次
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let result = fetch_sha256_with_retry(move || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                "abc123".to_string()
            }
        })
        .await;
        assert_eq!(result, "abc123");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 首次为空：重试一次拿到值
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let result = fetch_sha256_with_retry(move || {
            let calls = calls2.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    String::new()
                } else {
                    "def456".to_string()
                }
            }
        })
        .await;
        assert_eq!(result, "def456");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // 两次均为空：维持空串（调用方降级信任 HTTPS）
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let result = fetch_sha256_with_retry(move || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                String::new()
            }
        })
        .await;
        assert_eq!(result, "");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
