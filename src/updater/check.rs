//! 版本检查：latest.json 拉取 + semver 比较 + 平台选择
//!
//! 负责从配置的发布源拉取 `latest.json`，按当前平台（`target_os`/`target_arch`）
//! 选择下载包，并通过 `semver` 比较判断是否存在可用更新。清单在接受前经过
//! ed25519 签名严格校验（嵌公钥构建；见 [`super::signature`] 模块 doc）：
//! 自定清单文件直接校验随文件携带的 `signature` 字段；GitHub Release 路径
//! 优先使用发布方随发布上传的已签名 `latest.json` 资产——嵌公钥构建下资产
//! 缺失或不可用（含签名校验失败）一律直接拒绝，绝不回退逐资产合成清单
//! （合成清单无签名字段，纯 TLS 信任与签名信任锚不可共存）；未嵌公钥构建
//! 不做签名校验，资产不可用时照旧回退合成清单。

use std::collections::HashMap;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config::UpdateChannel;
use crate::updater::error::UpdaterError;
use crate::updater::signature;

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
/// GitHub Release 的包清单（远程 `latest.json` 的反序列化目标）：版本、日期、
/// 更新说明与各平台（`{os}-{arch}` → 下载包）映射
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
    /// 预期 SHA256 hex 摘要（64 字符小写）；检查阶段保证命中平台包非空，
    /// 缺失即 ChecksumUnavailable（UPD-5），下载端 MissingChecksum 兜底拒绝
    pub sha256: String,
    /// 预期文件大小（字节），与 Content-Length / 实际下载字节交叉核对（UPD-7）
    #[serde(default)]
    pub size: Option<u64>,
}

/// 当前平台键（编译期常量），如 `"windows-x64"`
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "windows-x64";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "windows-arm64";
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    not(feature = "openwrt"),
    not(target_env = "musl")
))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-x64";
#[cfg(all(
    target_os = "linux",
    target_arch = "aarch64",
    not(feature = "openwrt"),
    not(target_env = "musl")
))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-arm64";
#[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "openwrt"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "openwrt-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "openwrt"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "openwrt-arm64";
#[cfg(all(target_os = "linux", target_arch = "arm", feature = "openwrt"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "openwrt-armv7";
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "musl",
    not(feature = "openwrt")
))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-musl-x64";
#[cfg(all(
    target_os = "linux",
    target_arch = "aarch64",
    target_env = "musl",
    not(feature = "openwrt")
))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "linux-musl-arm64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const CURRENT_PLATFORM_KEY: &str = "macos-x64";
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "arm", feature = "openwrt"),
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
    // 重定向已由客户端策略逐跳前置校验，此处兜底：最终 URL 仍须通过白名单
    // （防 https→http evil 跳转；custom policy 下恒通过，无行为漂移）
    if !is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }

    // 处理 GitHub API 速率限制：未认证 REST 配额耗尽回 403 + 配额头耗尽
    // （GitHub 主限流按类型可能回 403 或 429），只认 429 会把主限流误报成
    // ManifestFetchFailed(403)，用户既看不懂也拿不到建议等待时间
    if let Some(e) = rate_limit_error_if_any(response.status(), response.headers()) {
        return Err(e);
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
        // T6-4：清单接受前的签名严格校验（嵌公钥构建；未嵌公钥构建为直通）。
        // 校验先于结构解析：真实性优先，损坏的伪造清单不进入后续任何分支
        signature::verify_manifest_value(&body)?;
        let manifest: ReleaseManifest =
            serde_json::from_value(body).map_err(UpdaterError::ManifestParseFailed)?;
        // UPD-5：自定义清单与 GitHub 路径同口径——命中平台包必须携带非空
        // sha256，空串透传会让前端提示有更新而下载端 MissingChecksum 拒绝，
        // 检查阶段就应显式失败
        if manifest
            .platforms
            .get(CURRENT_PLATFORM_KEY)
            .is_some_and(|p| p.sha256.is_empty())
        {
            return Err(UpdaterError::ChecksumUnavailable);
        }
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

/// 将单个 GitHub Release JSON 对象转换为发布清单（生产入口）
///
/// 以编译期嵌入的签名公钥调用 [`manifest_from_github_release_with_key`]；
/// 测试注入显式密钥以覆盖嵌钥/无钥两种构建形态。
async fn manifest_from_github_release(
    client: &reqwest::Client,
    body: &serde_json::Value,
) -> Result<ReleaseManifest, UpdaterError> {
    let verifying_key = signature::embedded_verifying_key();
    manifest_from_github_release_with_key(client, body, verifying_key.as_ref()).await
}

/// [`manifest_from_github_release`] 的核心实现（`verifying_key` 可注入）
///
/// 复用[`fetch_manifest`]的 tag_name 分支与通道列表路径：解析版本号，逐平台
/// 资产拉取 `.sha256` 伴随文件（缺失的平台包直接跳过，见 G11/G12）。
async fn manifest_from_github_release_with_key(
    client: &reqwest::Client,
    body: &serde_json::Value,
    verifying_key: Option<&ed25519_dalek::VerifyingKey>,
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
    // T6-4：优先使用发布方随发布上传的已签名清单 latest.json（对完整清单的
    // 整体签名，含全平台包信息）。回退口径按构建形态二分：
    // - 嵌公钥构建（verifying_key = Some）：资产缺失或不可用（下载 / 解析 /
    //   签名校验失败）一律直接拒绝，绝不回退逐资产合成清单——合成清单无签名
    //   字段，属纯 TLS 信任，与嵌钥构建的签名信任锚不可共存（fail-closed）。
    //   资产缺失报 [`UpdaterError::SignedManifestMissing`]；签名不匹配
    //   （[`UpdaterError::ManifestSignatureInvalid`]）是篡改信号；缺当前平台
    //   包（PlatformNotAvailable）原样上抛，由通道枚举的候选回退口径处理
    //   （语义见 fetch_latest_json_manifest doc）。
    // - 未嵌公钥构建（verifying_key = None）：不做签名校验，资产取不到可用
    //   内容时回退逐资产合成清单，与现状一致。
    match latest_json_asset_url(&assets) {
        Some(url) => {
            match fetch_latest_json_manifest_with_key(client, &url, verifying_key).await {
                Ok(manifest) => return Ok(manifest),
                // 嵌公钥构建：一律不回退合成清单（含签名校验失败——篡改信号），
                // 错误原样上抛
                Err(e) if verifying_key.is_some() => return Err(e),
                Err(e) => {
                    tracing::warn!("随发布上传的已签名清单不可用（{e}），回退逐资产合成清单")
                }
            }
        }
        // 嵌公钥构建：发布未附带已签名清单 → 拒绝（fail-closed）
        None if verifying_key.is_some() => return Err(UpdaterError::SignedManifestMissing),
        None => {}
    }
    let mut platforms: HashMap<String, PlatformPackage> = HashMap::new();
    // 仅拉取当前平台的校验信息：检查失败一条日志足够，且避免多平台全拉的刷屏与
    // 额外 sha256 伴随文件请求（旧逻辑遍历全部平台，每平台还重试一次）。
    match collect_current_platform_asset(&assets) {
        Some((_, dl_url, size, name)) => {
            let asset_refs: Vec<&serde_json::Value> = assets.iter().collect();
            let sha256 = fetch_sha256_assoc(client, &asset_refs, &name).await;
            if sha256.is_empty() {
                // 有当前平台包但发布未附 .sha256 伴随文件：与"无平台包"区分开，
                // 便于诊断（发布流程异常 / 手工上传漏带校验文件）
                return Err(UpdaterError::ChecksumUnavailable);
            }
            platforms.insert(
                CURRENT_PLATFORM_KEY.to_string(),
                PlatformPackage {
                    url: dl_url,
                    sha256,
                    size,
                },
            );
        }
        None => {
            return Err(UpdaterError::PlatformNotAvailable(
                "发布中未找到平台下载包".into(),
            ));
        }
    }
    Ok(ReleaseManifest {
        version,
        release_date,
        changelog,
        platforms,
    })
}

/// 从发布资产中定位发布方签名的清单文件 `latest.json`（T6-4）
///
/// 文件名精确匹配（忽略大小写）；返回其下载 URL。URL 未通过更新源白名单时
/// 按缺失处理（与 `.sha256` 伴随文件同口径：URL 完全由清单侧资产清单控制，
/// 不通过即不追）。
fn latest_json_asset_url(assets: &[serde_json::Value]) -> Option<String> {
    let asset = assets.iter().find(|a| {
        a.get("name")
            .and_then(|v| v.as_str())
            .map(|n| n.eq_ignore_ascii_case("latest.json"))
            .unwrap_or(false)
    })?;
    let url = asset
        .get("browser_download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if url.is_empty() {
        return None;
    }
    if !is_allowed_update_url(url) {
        // 只记 host：该 URL 完全由清单控制，可能携带 query/userinfo，勿整串进日志
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        tracing::warn!(host, "latest.json 资产 URL 未通过更新源白名单，按缺失处理");
        return None;
    }
    Some(url.to_string())
}

/// 拉取并校验发布方签名的清单文件 `latest.json`（T6-4）
///
/// 链路：下载 → ed25519 签名严格校验（嵌公钥构建；未嵌公钥构建为直通）→
/// 反序列化为 [`ReleaseManifest`] → 当前平台包存在性校验（缺失按
/// [`UpdaterError::PlatformNotAvailable`] 报告，保持既有候选回退语义：
/// 该候选被视为"发布缺当前平台资产"，通道枚举时继续尝试更低候选；嵌公钥
/// 构建下同样原样上抛——已签名清单可信且为权威内容，不经合成清单回退）。
async fn fetch_latest_json_manifest_with_key(
    client: &reqwest::Client,
    url: &str,
    verifying_key: Option<&ed25519_dalek::VerifyingKey>,
) -> Result<ReleaseManifest, UpdaterError> {
    let response = client
        .get(url)
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .header("Accept", "application/json")
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;
    let response = response
        .error_for_status()
        .map_err(UpdaterError::ManifestFetchFailed)?;
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;
    signature::verify_manifest_with_key(&body, verifying_key)?;
    let manifest: ReleaseManifest =
        serde_json::from_value(body).map_err(UpdaterError::ManifestParseFailed)?;
    if !manifest.platforms.contains_key(CURRENT_PLATFORM_KEY) {
        return Err(UpdaterError::PlatformNotAvailable(
            "latest.json 清单中无当前平台的下载包".into(),
        ));
    }
    Ok(manifest)
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
/// **候选失败回退口径**：首选候选（按通道选取的 semver 最高者）缺当前平台资产或
/// 缺 `.sha256` 校验文件时，按 semver 降序在**全列表**内继续尝试其余候选——不再
/// 区分通道类型（预发布候选失败后允许落到更低的正式版，发布流程漏传某平台包
/// 不应让用户错过紧随其后的完好发布）；其余错误（网络 / 解析 / 限流）立即返回，
/// 全部候选失败返回最后一个可恢复错误。
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
    // 重定向已由客户端策略逐跳前置校验，此处兜底（custom policy 下恒通过）
    if !is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }
    // 列表请求与单清单请求同一限流口径：403/429 + 配额头折算带 retry_after 的错误
    if let Some(e) = rate_limit_error_if_any(response.status(), response.headers()) {
        return Err(e);
    }
    let response = response
        .error_for_status()
        .map_err(UpdaterError::ManifestFetchFailed)?;
    let releases: Vec<serde_json::Value> = response
        .json()
        .await
        .map_err(UpdaterError::ManifestFetchFailed)?;

    // 过滤草稿后按通道筛选首选候选（列表按创建时间排序，
    // 直接取首项可能选中回迁的旧版本发布）
    let best = select_release_for_channel(&releases, channel);
    let Some(best) = best else {
        // 测试版通道下无任何预发布：回退正式版清单（releases/latest）
        if channel == UpdateChannel::Prerelease {
            tracing::info!("测试版通道下远程无预发布，回退正式版清单");
            return fetch_manifest(client, source_url).await;
        }
        return Err(UpdaterError::NoMatchingRelease);
    };

    // 首选候选先试；缺平台资产/校验文件时按 semver 降序在全列表内继续尝试
    // （见函数 doc comment 的回退口径），其余错误立即返回
    let ordered = releases_sorted_by_version_desc(&releases);
    let mut last_recoverable: Option<UpdaterError> = None;
    for candidate in
        std::iter::once(best).chain(ordered.into_iter().filter(|r| !std::ptr::eq(*r, best)))
    {
        let tag = candidate
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        match manifest_from_github_release(client, candidate).await {
            Ok(manifest) => return Ok(manifest),
            Err(
                e @ (UpdaterError::PlatformNotAvailable(_) | UpdaterError::ChecksumUnavailable),
            ) => {
                tracing::info!(
                    tag,
                    "候选发布缺当前平台资产或校验文件（{e}），尝试更低版本候选"
                );
                last_recoverable = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_recoverable.unwrap_or(UpdaterError::NoMatchingRelease))
}

/// 将 Releases 列表按 semver 降序排序（过滤草稿与 tag 无法解析的条目）
fn releases_sorted_by_version_desc(releases: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    let mut parsed: Vec<(Version, &serde_json::Value)> = releases
        .iter()
        .filter(|r| !r.get("draft").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter_map(|r| {
            let tag = r.get("tag_name").and_then(|v| v.as_str())?;
            let v = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
            Some((v, r))
        })
        .collect();
    parsed.sort_by(|a, b| b.0.cmp(&a.0));
    parsed.into_iter().map(|(_, r)| r).collect()
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
/// Linux 区分 OpenWrt、musl 与 glibc，并严格区分 ARMv7 / ARM64；
/// macos 保持旧语义（无架构词默认 x64，兼容 universal 包按 x64 归类）。
/// 返回 `None` 表示无法识别的组合，调用方 warn 后跳过。
fn infer_platform_key(name: &str) -> Option<&'static str> {
    let is_x64 = name.contains("x86_64") || name.contains("x64") || name.contains("amd64");
    let is_arm64 = name.contains("aarch64") || name.contains("arm64");
    let is_arm = is_arm64 || name.contains("arm");
    // OpenWrt、musl 与桌面 glibc 资产不可互相覆盖；32 位 ARM 不得归为 ARM64。
    if name.contains("openwrt") {
        if !name.contains("musl") {
            return None;
        }
        match (is_x64, is_arm64, name.contains("armv7")) {
            (true, false, false) => Some("openwrt-x64"),
            (false, true, false) => Some("openwrt-arm64"),
            (false, false, true) => Some("openwrt-armv7"),
            _ => None,
        }
    } else if name.contains("linux") && name.contains("musl") {
        match (is_x64, is_arm64) {
            (true, false) => Some("linux-musl-x64"),
            (false, true) => Some("linux-musl-arm64"),
            _ => None,
        }
    } else if name.contains("windows") {
        match (is_x64, is_arm) {
            (true, _) => Some("windows-x64"),
            (_, true) => Some("windows-arm64"),
            // windows 资产必须带架构词：盲目归入固定架构会让 arm64 顶掉 x64
            _ => None,
        }
    } else if name.contains("linux") {
        match (is_x64, is_arm64) {
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

/// 判定响应是否为配额限流，并折算为带建议等待时间的错误
///
/// 单清单（[`fetch_manifest`]）与 Releases 列表（[`fetch_manifest_for_channel`]）
/// 请求共用同一判定口径：403/429 + 配额耗尽（`X-RateLimit-Remaining: 0`）→
/// [`UpdaterError::RateLimited`]；429 但无配额头（代理/网关限流）沿用
/// `retry-after` 头，缺省 60s。纯函数（只读状态码与 HeaderMap）便于单测。
fn rate_limit_error_if_any(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Option<UpdaterError> {
    if !matches!(
        status,
        reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::FORBIDDEN
    ) {
        return None;
    }
    if let Some(retry_after) = rate_limit_retry_after(headers) {
        return Some(UpdaterError::RateLimited { retry_after });
    }
    // 429 但无配额头（代理/网关限流）：沿用 retry-after 头，缺省 60s
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        return Some(UpdaterError::RateLimited { retry_after });
    }
    None
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
#[cfg(test)]
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
    // 与主包下载同一白名单：清单指向的校验文件也可能被诱导到 http/内网地址；
    // 未通过按「缺失」处理，沿用既有 ChecksumUnavailable → 下一候选回退口径
    if !is_allowed_update_url(url) {
        // 只记 host：该 URL 完全由清单控制，可能携带 query/userinfo，勿整串进日志
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        tracing::warn!(host, ".sha256 伴随文件 URL 未通过更新源白名单，按缺失处理");
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

    /// 全列表 semver 降序：过滤草稿与 tag 无法解析的条目
    #[test]
    fn test_releases_sorted_by_version_desc() {
        let releases: Vec<serde_json::Value> = [
            r#"{"tag_name": "v1.0.0", "draft": false}"#,
            r#"{"tag_name": "v9.0.0", "draft": false}"#,
            r#"{"tag_name": "v3.0.0", "draft": true}"#, // 草稿排除
            r#"{"tag_name": "legacy", "draft": false}"#, // 无法解析排除
            r#"{"tag_name": "v2.0.0", "draft": false}"#,
        ]
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
        let ordered = releases_sorted_by_version_desc(&releases);
        let tags: Vec<&str> = ordered
            .iter()
            .map(|r| r["tag_name"].as_str().unwrap())
            .collect();
        assert_eq!(tags, vec!["v9.0.0", "v2.0.0", "v1.0.0"]);
    }

    // ============ 回环 mock：候选回退与限流口径 ============

    use std::net::SocketAddr;

    use axum::http::StatusCode;
    use axum::routing::get as route_get;

    /// 回环直连：测试机代理（如 127.0.0.1:7890）会劫持 reqwest 回环请求导致 502
    fn loopback_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    /// 起本地 axum 服务
    async fn serve(app: axum::Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    /// 当前平台可识别的资产名（与生产 `infer_platform_key` 的 cfg 矩阵一致）
    fn platform_asset_name() -> &'static str {
        match CURRENT_PLATFORM_KEY {
            "windows-x64" => "app-windows-x64.zip",
            "windows-arm64" => "app-windows-arm64.zip",
            "linux-x64" => "app-linux-x64.zip",
            "linux-arm64" => "app-linux-arm64.zip",
            "linux-musl-x64" => "app-x86_64-unknown-linux-musl.tar.gz",
            "linux-musl-arm64" => "app-aarch64-unknown-linux-musl.tar.gz",
            "openwrt-x64" => "app-openwrt-x86_64-unknown-linux-musl.tar.gz",
            "openwrt-arm64" => "app-openwrt-aarch64-unknown-linux-musl.tar.gz",
            "openwrt-armv7" => "app-openwrt-armv7-unknown-linux-musleabihf.tar.gz",
            "macos-x64" => "app-macos-x64.zip",
            "macos-arm64" => "app-macos-arm64.zip",
            _ => "app-unknown.zip",
        }
    }

    #[test]
    fn openwrt_assets_do_not_replace_desktop_or_other_architectures() {
        for (name, key) in [
            (
                "app-openwrt-x86_64-unknown-linux-musl.tar.gz",
                Some("openwrt-x64"),
            ),
            (
                "app-openwrt-aarch64-unknown-linux-musl.tar.gz",
                Some("openwrt-arm64"),
            ),
            (
                "app-openwrt-armv7-unknown-linux-musleabihf.tar.gz",
                Some("openwrt-armv7"),
            ),
            (
                "app-x86_64-unknown-linux-musl.tar.gz",
                Some("linux-musl-x64"),
            ),
            (
                "app-aarch64-unknown-linux-musl.tar.gz",
                Some("linux-musl-arm64"),
            ),
            ("app-x86_64-unknown-linux-gnu.tar.gz", Some("linux-x64")),
            ("app-openwrt-mipsel-unknown-linux-musl.tar.gz", None),
            ("app-openwrt-aarch64-unknown-linux-gnu.tar.gz", None),
            ("app-armv7-unknown-linux-gnueabihf.tar.gz", None),
        ] {
            assert_eq!(infer_platform_key(name), key, "{name}");
        }
    }

    /// 最高候选缺平台资产 → 回退次高候选（P1 实锤项回归：此前直接失败不回退）
    #[tokio::test]
    async fn fetch_manifest_for_channel_falls_back_to_next_candidate() {
        // 先绑定端口拿到地址，让"次高候选"的 .sha256 伴随文件指向本 mock
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sha_url = format!("http://{addr}/pkg.zip.sha256");
        let app = axum::Router::new()
            .route(
                "/repos/o/r/releases",
                route_get(move || {
                    let sha_url = sha_url.clone();
                    async move {
                        // 最高候选（正式 + 预发布各一）都没有当前平台资产；
                        // 次高候选资产齐备且带 .sha256 伴随文件
                        let good = serde_json::json!({
                            "tag_name": "v2.0.0",
                            "prerelease": false,
                            "draft": false,
                            "assets": [
                                {
                                    "name": platform_asset_name(),
                                    "browser_download_url": format!("http://{addr}/pkg.zip"),
                                    "size": 16,
                                },
                                {
                                    "name": format!("{}.sha256", platform_asset_name()),
                                    "browser_download_url": sha_url,
                                },
                            ],
                        });
                        let bad_stable = serde_json::json!({
                            "tag_name": "v9.0.0",
                            "prerelease": false,
                            "draft": false,
                            "assets": [
                                {"name": "tool.zip", "browser_download_url": format!("http://{addr}/tool.zip")},
                            ],
                        });
                        let bad_pre = serde_json::json!({
                            "tag_name": "v9.5.0-beta.1",
                            "prerelease": true,
                            "draft": false,
                            "assets": [],
                        });
                        axum::Json(serde_json::json!([bad_stable, bad_pre, good]))
                    }
                }),
            )
            .route(
                "/pkg.zip.sha256",
                route_get(|| async { "a0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = loopback_client();

        // 全通道：最高正式版缺资产 → 落到 v2.0.0
        let manifest = fetch_manifest_for_channel(
            &client,
            &format!("http://{addr}/repos/o/r/releases/latest"),
            UpdateChannel::All,
        )
        .await
        .expect("最高候选缺资产应回退次高候选");
        assert_eq!(manifest.version, Version::parse("2.0.0").unwrap());

        // 测试版通道：最高预发布缺资产 → 继续降序迭代，落到正式版 v2.0.0
        //（回退口径不区分通道类型，见 fetch_manifest_for_channel doc comment）
        let manifest = fetch_manifest_for_channel(
            &client,
            &format!("http://{addr}/repos/o/r/releases/latest"),
            UpdateChannel::Prerelease,
        )
        .await
        .expect("预发布候选缺资产应回退更低候选");
        assert_eq!(manifest.version, Version::parse("2.0.0").unwrap());
    }

    /// 列表请求命中限流（403 + 配额耗尽头）→ 带 retry_after 的 RateLimited，
    /// 与单清单请求同一口径（P2：此前列表路径不复用该判定）
    #[tokio::test]
    async fn fetch_manifest_for_channel_reports_rate_limit_on_list() {
        let app = axum::Router::new().route(
            "/repos/o/r/releases",
            route_get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    [("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1")],
                    axum::Json(serde_json::json!([])),
                )
            }),
        );
        let addr = serve(app).await;
        let client = loopback_client();
        let err = fetch_manifest_for_channel(
            &client,
            &format!("http://{addr}/repos/o/r/releases/latest"),
            UpdateChannel::All,
        )
        .await
        .expect_err("限流响应应报 RateLimited");
        assert!(
            matches!(err, UpdaterError::RateLimited { retry_after: 1 }),
            "{err}"
        );
    }

    /// `.sha256` 伴随文件 URL 未通过更新源白名单 → 按「缺失」处理返回空串（B4）
    ///
    /// 白名单在 `client.get` 前短路，无需起 mock 服务；`http://example.com`
    /// 非 loopback 主机，恒被拒绝。
    #[tokio::test]
    async fn fetch_sha256_assoc_rejects_non_whitelisted_url() {
        let assets = [serde_json::json!({
            "name": "app.zip.sha256",
            "browser_download_url": "http://example.com/app.zip.sha256",
        })];
        let asset_refs: Vec<&serde_json::Value> = assets.iter().collect();
        let sha = fetch_sha256_assoc(&loopback_client(), &asset_refs, "app.zip").await;
        assert!(
            sha.is_empty(),
            "非白名单伴随 URL 应按缺失返回空串，实际 {sha:?}"
        );
    }

    // ============ T6-4：发布方签名清单（latest.json 资产） ============

    /// latest.json 资产定位：精确名（忽略大小写）命中，伴随文件/白名单外/缺 URL 按缺失处理
    #[test]
    fn test_latest_json_asset_url() {
        let mk =
            |name: &str, url: &str| serde_json::json!({"name": name, "browser_download_url": url});
        // 精确命中（多资产中找出清单文件）
        assert_eq!(
            latest_json_asset_url(&[
                mk("pkg.zip", "https://x/pkg.zip"),
                mk("latest.json", "https://x/latest.json"),
            ])
            .as_deref(),
            Some("https://x/latest.json")
        );
        // 大小写不敏感
        assert_eq!(
            latest_json_asset_url(&[mk("Latest.JSON", "https://x/latest.json")]).as_deref(),
            Some("https://x/latest.json")
        );
        // `.sha256` 伴随文件不是清单文件本身
        assert_eq!(
            latest_json_asset_url(&[mk("latest.json.sha256", "https://x/latest.json.sha256")]),
            None
        );
        // URL 未通过白名单（非回环 http）→ 按缺失处理
        assert_eq!(
            latest_json_asset_url(&[mk("latest.json", "http://example.com/latest.json")]),
            None
        );
        // 缺 URL 字段 → 按缺失处理
        assert_eq!(
            latest_json_asset_url(&[serde_json::json!({"name": "latest.json"})]),
            None
        );
        assert_eq!(latest_json_asset_url(&[]), None);
    }

    /// GitHub Release 路径优先使用已签名 latest.json 资产：
    /// 版本与平台信息来自资产文件而非 release tag / 逐资产合成
    #[tokio::test]
    async fn manifest_from_github_release_prefers_signed_latest_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (secret_b64, _pubkey_b64) = signature::generate_signing_keypair();
        // 清单版本 3.1.4 与 release tag v3.0.0 不同：命中即证明内容确实来自资产文件
        let manifest_body = format!(
            r#"{{"version":"3.1.4","platforms":{{"{}":{{"url":"https://example.invalid/pkg.zip","sha256":"{}"}}}}}}"#,
            CURRENT_PLATFORM_KEY,
            "a".repeat(64)
        );
        let signed = signature::sign_manifest_document(&manifest_body, &secret_b64).unwrap();
        let latest_value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        let app = axum::Router::new()
            .route(
                "/repo/release",
                route_get(move || async move {
                    axum::Json(serde_json::json!({
                        "tag_name": "v3.0.0",
                        "prerelease": false,
                        "draft": false,
                        "assets": [
                            {
                                "name": "latest.json",
                                "browser_download_url": format!("http://{addr}/latest.json"),
                            },
                            {"name": "app.zip", "browser_download_url": "https://example.invalid/app.zip"},
                        ],
                    }))
                }),
            )
            .route(
                "/latest.json",
                route_get(move || {
                    let body = latest_value.clone();
                    async move { axum::Json(body) }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let manifest = fetch_manifest(&loopback_client(), &format!("http://{addr}/repo/release"))
            .await
            .expect("发布附带已签名清单时应直接使用");
        assert_eq!(
            manifest.version,
            Version::parse("3.1.4").unwrap(),
            "版本应来自已签名 latest.json 资产而非 release tag"
        );
    }

    // ============ T6-4b：嵌钥构建下拒绝合成清单回退（fail-closed） ============

    /// 测试用 SHA256 伴随文件内容（64 位 hex；检查阶段不校验长度）
    const TEST_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// 生成测试密钥对并签发清单文档，返回签名文本与发布方验证公钥
    fn sign_with_new_keypair(body: &str) -> (String, ed25519_dalek::VerifyingKey) {
        let (secret_b64, pubkey_b64) = signature::generate_signing_keypair();
        let signed = signature::sign_manifest_document(body, &secret_b64).unwrap();
        let pubkey = signature::decode_pubkey(&pubkey_b64).expect("测试公钥应可解析");
        (signed, pubkey)
    }

    /// 当前平台包齐备的最小清单文档（`platforms` 由调用方给定以便构造缺平台场景）
    fn manifest_body_with_platforms(version: &str, platforms: &str) -> String {
        format!(r#"{{"version":"{version}","platforms":{platforms}}}"#)
    }

    /// 当前平台包齐备的清单文档
    fn full_platform_manifest_body(version: &str) -> String {
        manifest_body_with_platforms(
            version,
            &format!(
                r#"{{"{}":{{"url":"https://example.invalid/pkg.zip","sha256":"{}"}}}}"#,
                CURRENT_PLATFORM_KEY,
                "a".repeat(64)
            ),
        )
    }

    /// 组装「逐资产合成可完整成功」的 release JSON：当前平台包 + `.sha256`
    /// 伴随文件齐备；`latest_url` 为 `Some` 时附带已签名清单资产。
    /// 嵌钥构建下合成回退被拒绝的断言依赖本基座——若回退未被拒，测试会拿到
    /// 合成清单的 Ok（版本取 tag v2.0.0）而非期望错误
    fn synthetic_capable_release(addr: SocketAddr, latest_url: Option<&str>) -> serde_json::Value {
        let mut assets = vec![
            serde_json::json!({
                "name": platform_asset_name(),
                "browser_download_url": format!("http://{addr}/{}", platform_asset_name()),
                "size": 16,
            }),
            serde_json::json!({
                "name": format!("{}.sha256", platform_asset_name()),
                "browser_download_url": format!("http://{addr}/{}.sha256", platform_asset_name()),
            }),
        ];
        if let Some(url) = latest_url {
            assets.push(serde_json::json!({
                "name": "latest.json",
                "browser_download_url": url,
            }));
        }
        serde_json::json!({
            "tag_name": "v2.0.0",
            "prerelease": false,
            "draft": false,
            "assets": assets,
        })
    }

    /// 嵌钥构建 + 发布未附带 latest.json 资产 → 拒绝（SignedManifestMissing），
    /// 不落合成清单：合成基座完整可用（`.sha256` 伴随文件可取），若回退未被拒
    /// 本测试将拿到 Ok(v2.0.0) 而非期望错误
    #[tokio::test]
    async fn signed_asset_missing_rejects_synthetic_fallback_when_pubkey_embedded() {
        let (_, pubkey) = sign_with_new_keypair(&full_platform_manifest_body("3.1.4"));
        // 先绑定端口拿到地址，再据此构造 release JSON（伴随文件 URL 需含端口）
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let release = synthetic_capable_release(addr, None);
        let sha_path = format!("/{}.sha256", platform_asset_name());
        let app = axum::Router::new().route(&sha_path, route_get(|| async { TEST_SHA256 }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let err =
            manifest_from_github_release_with_key(&loopback_client(), &release, Some(&pubkey))
                .await
                .expect_err("嵌钥构建下资产缺失应拒绝回退合成清单");
        assert!(
            matches!(err, UpdaterError::SignedManifestMissing),
            "应报 SignedManifestMissing，实际 {err}"
        );
    }

    /// 嵌钥构建 + latest.json 资产签名不匹配（内容被篡改）→ 直接拒绝
    /// （ManifestSignatureInvalid），不得回退到完整可用的合成清单——那是篡改信号
    #[tokio::test]
    async fn signature_failure_does_not_fall_back_when_pubkey_embedded() {
        let (signed, pubkey) = sign_with_new_keypair(&full_platform_manifest_body("3.1.4"));
        // 签名后篡改版本号 → 签名与规范化字节不再匹配
        let mut tampered: serde_json::Value = serde_json::from_str(&signed).unwrap();
        tampered["version"] = serde_json::Value::String("9.9.9".into());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sha_path = format!("/{}.sha256", platform_asset_name());
        let release = synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json")));
        let app = axum::Router::new()
            .route(
                "/release",
                route_get(move || async move { axum::Json(release.clone()) }),
            )
            .route(
                "/latest.json",
                route_get(move || {
                    let body = tampered.clone();
                    async move { axum::Json(body) }
                }),
            )
            .route(&sha_path, route_get(|| async { TEST_SHA256 }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let err = manifest_from_github_release_with_key(
            &loopback_client(),
            &synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json"))),
            Some(&pubkey),
        )
        .await
        .expect_err("嵌钥构建下签名校验失败应直接拒绝");
        assert!(
            matches!(err, UpdaterError::ManifestSignatureInvalid),
            "应报 ManifestSignatureInvalid，实际 {err}"
        );
    }

    /// 嵌钥构建 + latest.json 资产拉取失败（500）→ 错误原样上抛，不回退合成清单
    /// （嵌钥构建的回退通道整体不存在，传输类错误同样终止）
    #[tokio::test]
    async fn signed_asset_fetch_failure_terminates_without_fallback_when_pubkey_embedded() {
        let (_, pubkey) = sign_with_new_keypair(&full_platform_manifest_body("3.1.4"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sha_path = format!("/{}.sha256", platform_asset_name());
        let release = synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json")));
        let app = axum::Router::new()
            .route(
                "/release",
                route_get(move || async move { axum::Json(release.clone()) }),
            )
            .route(
                "/latest.json",
                route_get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route(&sha_path, route_get(|| async { TEST_SHA256 }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let err = manifest_from_github_release_with_key(
            &loopback_client(),
            &synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json"))),
            Some(&pubkey),
        )
        .await
        .expect_err("嵌钥构建下资产拉取失败应原样上抛");
        assert!(
            matches!(err, UpdaterError::ManifestFetchFailed(_)),
            "应报 ManifestFetchFailed，实际 {err}"
        );
    }

    /// 嵌钥构建 + 已签名清单可信但缺当前平台包 → PlatformNotAvailable 原样上抛
    /// （签名清单是权威内容，不经合成清单"救回"；候选回退口径由通道枚举处理）
    #[tokio::test]
    async fn platform_missing_in_signed_manifest_propagates_without_synthetic_fallback() {
        let body = manifest_body_with_platforms(
            "3.1.4",
            r#"{"not-a-real-platform":{"url":"https://example.invalid/pkg.zip","sha256":"aa"}}"#,
        );
        let (signed, pubkey) = sign_with_new_keypair(&body);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sha_path = format!("/{}.sha256", platform_asset_name());
        let release = synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json")));
        let latest_value: serde_json::Value = serde_json::from_str(&signed).unwrap();
        let app = axum::Router::new()
            .route(
                "/release",
                route_get(move || async move { axum::Json(release.clone()) }),
            )
            .route(
                "/latest.json",
                route_get(move || {
                    let body = latest_value.clone();
                    async move { axum::Json(body) }
                }),
            )
            .route(&sha_path, route_get(|| async { TEST_SHA256 }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let err = manifest_from_github_release_with_key(
            &loopback_client(),
            &synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json"))),
            Some(&pubkey),
        )
        .await
        .expect_err("缺当前平台包应原样上抛");
        assert!(
            matches!(err, UpdaterError::PlatformNotAvailable(_)),
            "应报 PlatformNotAvailable，实际 {err}"
        );
    }

    /// 无公钥构建（显式传 None 对照）：资产缺失 / 拉取失败均照旧回退合成清单，
    /// 兼容语义与修复前完全一致
    #[tokio::test]
    async fn no_pubkey_build_fallback_behavior_unchanged() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sha_path = format!("/{}.sha256", platform_asset_name());
        let missing = synthetic_capable_release(addr, None);
        let broken = synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json")));
        let app = axum::Router::new()
            .route(
                "/release-missing",
                route_get(move || async move { axum::Json(missing.clone()) }),
            )
            .route(
                "/release-broken",
                route_get(move || async move { axum::Json(broken.clone()) }),
            )
            .route(
                "/latest.json",
                route_get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route(&sha_path, route_get(|| async { TEST_SHA256 }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = loopback_client();

        // 资产缺失 → 合成清单（版本取 tag，摘要取伴随文件）
        let manifest = manifest_from_github_release_with_key(
            &client,
            &synthetic_capable_release(addr, None),
            None,
        )
        .await
        .expect("无钥构建下资产缺失应照旧回退合成清单");
        assert_eq!(manifest.version, Version::parse("2.0.0").unwrap());
        assert_eq!(
            manifest.platforms[CURRENT_PLATFORM_KEY].sha256, TEST_SHA256,
            "合成清单摘要应来自 .sha256 伴随文件"
        );

        // 资产拉取失败 → 同样回退合成清单
        let manifest = manifest_from_github_release_with_key(
            &client,
            &synthetic_capable_release(addr, Some(&format!("http://{addr}/latest.json"))),
            None,
        )
        .await
        .expect("无钥构建下资产拉取失败应照旧回退合成清单");
        assert_eq!(manifest.version, Version::parse("2.0.0").unwrap());
    }
}
