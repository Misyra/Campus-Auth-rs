//! GitHub 镜像代理清单：uv 下载、uv 托管 Python 下载与更新包下载共用的镜像加速配置
//!
//! 镜像只是传输加速，所有经由镜像下载的内容都仍受 SHA256（uv / 更新包）或
//! ed25519 清单签名（更新清单）完整性校验把关；镜像不可达或被污染不会破坏
//! 信任锚，只会回退到下一候选。名单中的失效镜像无需手工摘除——并发测速
//! 会自动把它们排到尝试序列末尾。

/// GitHub 代理镜像列表（用于国内网络环境；前缀拼接原 URL 即镜像地址）
pub const GITHUB_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
];
/// GitHub API 代理镜像（用于获取版本号）
pub const GITHUB_API_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
];

/// 判断 URL 是否为 GitHub Release 下载资产（host 精确为 github.com 且路径
/// 含 `/releases/download/`）。仅这类 URL 可以安全套镜像前缀——镜像代理
/// 只转发 GitHub 域名，对自建下载源（自定义 source_url）套前缀必失败。
pub fn is_github_release_asset_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    else {
        return false;
    };
    // 路径需要 owner/repo/releases/download/tag/asset 完整形态，防御性容忍尾斜杠
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let trimmed = path.trim_matches('/');
    let Some(idx) = trimmed.find("/releases/download/") else {
        return false;
    };
    let after = &trimmed[idx + "/releases/download/".len()..];
    !after.is_empty()
}

/// 为 GitHub Release 资产生成镜像代理候选 URL 列表（保持镜像名单顺序）
///
/// 非 GitHub Release 资产（自定义下载源等）返回空列表，调用方按原 URL 单源重试。
pub fn github_release_asset_mirror_urls(url: &str) -> Vec<String> {
    if !is_github_release_asset_url(url) {
        return Vec::new();
    }
    GITHUB_MIRRORS
        .iter()
        .map(|mirror| format!("{mirror}{url}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UV_ASSET: &str =
        "https://github.com/astral-sh/uv/releases/download/0.8.14/uv-x86_64-pc-windows-msvc.zip";

    /// GitHub Release 资产识别：正例命中、查询串剥离、大小写敏感
    #[test]
    fn test_is_github_release_asset_url() {
        assert!(is_github_release_asset_url(UV_ASSET));
        assert!(is_github_release_asset_url(&format!("{UV_ASSET}?sig=1")));
        // 非 https（自建 http 源）不套镜像
        assert!(!is_github_release_asset_url(
            "http://192.168.1.10:8080/pkg.zip"
        ));
        // 非 github.com 域不套镜像
        assert!(!is_github_release_asset_url(
            "https://objects.githubusercontent.com/pkg.zip"
        ));
        // releases 列表页不是下载资产
        assert!(!is_github_release_asset_url(
            "https://github.com/astral-sh/uv/releases/latest"
        ));
        // releases/download/ 缺资产段
        assert!(!is_github_release_asset_url(
            "https://github.com/astral-sh/uv/releases/download/"
        ));
    }

    /// 镜像候选：按名单顺序生成前缀 URL；非 GitHub 资产返回空
    #[test]
    fn test_github_release_asset_mirror_urls() {
        let urls = github_release_asset_mirror_urls(UV_ASSET);
        assert_eq!(urls.len(), GITHUB_MIRRORS.len());
        assert!(urls[0].starts_with(GITHUB_MIRRORS[0]));
        assert!(urls[0].ends_with(UV_ASSET));
        assert!(github_release_asset_mirror_urls("https://example.com/pkg.zip").is_empty());
    }
}
