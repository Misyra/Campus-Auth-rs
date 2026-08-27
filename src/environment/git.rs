//! portable Git 下载（开发者模式）

use std::path::PathBuf;

use crate::environment::{
    EnvironmentError, EnvironmentManager, MINGIT_DOWNLOAD_TIMEOUT, MINGIT_RELEASES_BASE,
};

/// 检查 Git 是否可用
///
/// - Windows：检查 environment/git/cmd/git.exe（MinGit 本地安装）
/// - Linux/macOS：检查 `git` 是否在 PATH 中可用
pub async fn check_git(mgr: &EnvironmentManager) -> Result<bool, EnvironmentError> {
    #[cfg(target_os = "windows")]
    {
        let git_exe = mgr.env_path().join("git").join("cmd").join("git.exe");
        if git_exe.exists() {
            return Ok(true);
        }
        // 检查系统 PATH 中是否有 git
        Ok(which::which("git").is_ok())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Linux/macOS：检查系统 git 是否可用（`which git`）
        let result = tokio::process::Command::new("which")
            .arg("git")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        match result {
            Ok(status) => Ok(status.success()),
            Err(_) => Ok(false),
        }
    }
}

/// 下载 portable MinGit（仅开发者模式按需下载）
///
/// 从 GitHub Releases 下载 MinGit zip，解压到 environment/git/，
/// 返回 git 可执行文件路径 (environment/git/cmd/git.exe)。
///
/// R3：下载后先做 SHA256 完整性校验再解压——校验值取 release assets 中的
/// `.sha256` 伴随文件（复用 updater::check::fetch_sha256_assoc 模式）；
/// 校验失败不落盘不解压，返回明确错误。git-for-windows 官方 release 通常
/// 不提供伴随文件，此时降级为 warn + 信任 HTTPS 继续安装（与 updater::check
/// 的降级策略一致；对照 uv.rs——uv 官方 release 恒有 .sha256 故为强校验）。
pub async fn download_mingit(mgr: &EnvironmentManager) -> Result<PathBuf, EnvironmentError> {
    let env_path = mgr.env_path();
    let git_dir = env_path.join("git");
    let git_exe = git_dir.join("cmd").join("git.exe");

    // 如果已存在，直接返回
    if git_exe.exists() {
        return Ok(git_exe);
    }

    // 1. 查询最新版本（连同 assets 列表，供 .sha256 伴随文件查找）
    let (tag_name, version_str, assets) = fetch_latest_mingit_version(mgr).await?;

    // 2. 构造下载 URL
    // tag_name 格式: "v2.55.0.windows.2"
    // zip 文件名格式: "MinGit-2.55.0.2-64-bit.zip"
    let zip_name = format!(
        "mingit-{}-{}.zip",
        version_str,
        crate::environment::MINGIT_TARGET
    );
    let zip_url = format!(
        "{}/{}/MinGit-{}-{}.zip",
        MINGIT_RELEASES_BASE,
        tag_name,
        version_str,
        crate::environment::MINGIT_TARGET
    );

    tracing::info!("下载 MinGit: {}", zip_url);

    // 3. 流式下载 zip 到临时文件
    let tmp_zip = env_path.join("mingit.zip.tmp");
    let dl_result = tokio::time::timeout(
        MINGIT_DOWNLOAD_TIMEOUT,
        download_file(mgr, &zip_url, &tmp_zip),
    )
    .await;

    match dl_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            return Err(EnvironmentError::MinGitDownloadFailed(e.to_string()));
        }
        Err(_elapsed) => {
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            return Err(EnvironmentError::MinGitDownloadFailed(format!(
                "下载超时 (超过 {}s)",
                MINGIT_DOWNLOAD_TIMEOUT.as_secs()
            )));
        }
    }

    // 4. SHA256 完整性校验（R3）：取 release assets 中的 .sha256 伴随文件；
    //    校验失败不落盘（删除临时 zip）不解压，直接返回明确错误
    let asset_refs: Vec<&serde_json::Value> = assets.iter().collect();
    let expected_hash =
        crate::updater::check::fetch_sha256_assoc(mgr.http_client(), &asset_refs, &zip_name).await;
    if expected_hash.is_empty() {
        // 降级：官方 release 未提供伴随 .sha256 文件，无法强校验。
        // 与 updater::check 的降级一致（warn + 信任 HTTPS）；
        // uv.rs 不降级是因为 uv 官方 release 恒有 .sha256 伴随文件。
        tracing::warn!("MinGit 发布中无 {zip_name} 的 .sha256 伴随文件，降级为信任 HTTPS 安装");
    } else {
        if let Err(e) = crate::environment::uv::verify_sha256(&tmp_zip, &expected_hash).await {
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            let (expected, got) = match &e {
                EnvironmentError::UvChecksumMismatch { expected, got } => {
                    (expected.clone(), got.clone())
                }
                // verify_sha256 仅在读文件失败时返回其它变体
                other => (expected_hash.clone(), other.to_string()),
            };
            return Err(EnvironmentError::MinGitChecksumMismatch { expected, got });
        }
        tracing::info!("MinGit SHA256 校验通过");
    }

    // 5. 解压到 environment/git/
    let tmp_zip_clone = tmp_zip.clone();
    let git_dir_clone = git_dir.clone();
    tokio::task::spawn_blocking(move || extract_mingit_zip(&tmp_zip_clone, &git_dir_clone))
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))?
        .map_err(EnvironmentError::UvExtractFailed)?;

    // 清理临时文件
    let _ = tokio::fs::remove_file(&tmp_zip).await;

    // 6. 验证
    if !git_exe.exists() {
        return Err(EnvironmentError::MinGitDownloadFailed(
            "解压后未找到 git/cmd/git.exe".into(),
        ));
    }

    tracing::info!("MinGit 安装成功: {}", git_exe.display());
    Ok(git_exe)
}

/// 通过 GitHub API 获取 MinGit 最新版本
///
/// 返回 (tag_name, version_str, assets)，例如
/// ("v2.55.0.windows.2", "2.55.0.2", [...])；assets 供 `.sha256`
/// 伴随文件查找（R3 完整性校验）。
async fn fetch_latest_mingit_version(
    mgr: &EnvironmentManager,
) -> Result<(String, String, Vec<serde_json::Value>), EnvironmentError> {
    let url = "https://api.github.com/repos/git-for-windows/git/releases/latest";
    let resp = mgr
        .http_client()
        .get(url)
        .header("User-Agent", "campus-auth")
        .send()
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(EnvironmentError::MinGitDownloadFailed(format!(
            "GitHub API 返回 HTTP {}",
            resp.status()
        )));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))?;

    let tag_name = json["tag_name"]
        .as_str()
        .ok_or_else(|| EnvironmentError::MinGitDownloadFailed("tag_name 字段缺失".into()))?;

    // 解析 tag_name: "v2.55.0.windows.2" -> version_str: "2.55.0.2"
    let version_str = parse_mingit_tag(tag_name)?;

    // assets 列表（可能为空——官方 release 无 .sha256 伴随文件时降级）
    let assets: Vec<serde_json::Value> = json["assets"].as_array().cloned().unwrap_or_default();

    Ok((tag_name.to_string(), version_str, assets))
}

/// 解析 MinGit tag_name 为版本字符串
///
/// "v2.55.0.windows.2" -> "2.55.0.2"
fn parse_mingit_tag(tag: &str) -> Result<String, EnvironmentError> {
    // 去掉 "v" 前缀
    let without_v = tag
        .strip_prefix('v')
        .ok_or_else(|| EnvironmentError::MinGitDownloadFailed(format!("无法解析 tag: {}", tag)))?;

    // "2.55.0.windows.2" -> ["2", "55", "0", "windows", "2"]
    // 取前 3 段 + 最后一段
    let parts: Vec<&str> = without_v.split('.').collect();
    if parts.len() < 5 || parts[3] != "windows" {
        return Err(EnvironmentError::MinGitDownloadFailed(format!(
            "无法解析 tag 格式: {}",
            tag
        )));
    }

    // "2.55.0.2"
    Ok(format!(
        "{}.{}.{}.{}",
        parts[0], parts[1], parts[2], parts[4]
    ))
}

/// 流式下载文件到指定路径
async fn download_file(
    mgr: &EnvironmentManager,
    url: &str,
    dest: &std::path::Path,
) -> Result<(), EnvironmentError> {
    crate::utils::io::download_streaming(mgr.http_client(), url, dest, 512 * 1024 * 1024)
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))
}

/// 解压 MinGit zip 到目标目录（同步，在 spawn_blocking 中执行）
fn extract_mingit_zip(
    zip_path: &std::path::Path,
    dest_dir: &std::path::Path,
) -> std::io::Result<()> {
    // 清理旧目录后全量解压（无过滤，保留 MinGit 目录结构）
    if dest_dir.exists() {
        std::fs::remove_dir_all(dest_dir)?;
    }
    std::fs::create_dir_all(dest_dir)?;
    crate::utils::io::extract_zip(zip_path, dest_dir, |_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tag 解析：windows 修订号并入四段版本；非法格式被拒
    #[test]
    fn test_parse_mingit_tag() {
        assert_eq!(parse_mingit_tag("v2.55.0.windows.2").unwrap(), "2.55.0.2");
        assert_eq!(parse_mingit_tag("v2.47.1.windows.1").unwrap(), "2.47.1.1");
        assert!(parse_mingit_tag("2.55.0.windows.2").is_err(), "缺 v 前缀");
        assert!(
            parse_mingit_tag("v2.55.0").is_err(),
            "缺 windows 修订段应被拒"
        );
    }
}
