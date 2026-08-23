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
pub async fn download_mingit(mgr: &EnvironmentManager) -> Result<PathBuf, EnvironmentError> {
    let env_path = mgr.env_path();
    let git_dir = env_path.join("git");
    let git_exe = git_dir.join("cmd").join("git.exe");

    // 如果已存在，直接返回
    if git_exe.exists() {
        return Ok(git_exe);
    }

    // 1. 查询最新版本
    let (tag_name, version_str) = fetch_latest_mingit_version(mgr).await?;

    // 2. 构造下载 URL
    // tag_name 格式: "v2.55.0.windows.2"
    // zip 文件名格式: "MinGit-2.55.0.2-64-bit.zip"
    let zip_url = format!(
        "{}/{}/MinGit-{}-{}.zip",
        MINGIT_RELEASES_BASE, tag_name, version_str, crate::environment::MINGIT_TARGET
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

    // 4. 解压到 environment/git/
    let tmp_zip_clone = tmp_zip.clone();
    let git_dir_clone = git_dir.clone();
    tokio::task::spawn_blocking(move || extract_mingit_zip(&tmp_zip_clone, &git_dir_clone))
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))?
        .map_err(EnvironmentError::UvExtractFailed)?;

    // 清理临时文件
    let _ = tokio::fs::remove_file(&tmp_zip).await;

    // 5. 验证
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
/// 返回 (tag_name, version_str)，例如 ("v2.55.0.windows.2", "2.55.0.2")
async fn fetch_latest_mingit_version(
    mgr: &EnvironmentManager,
) -> Result<(String, String), EnvironmentError> {
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

    Ok((tag_name.to_string(), version_str))
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
    Ok(format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[4]))
}

/// 流式下载文件到指定路径
async fn download_file(
    mgr: &EnvironmentManager,
    url: &str,
    dest: &std::path::Path,
) -> Result<(), EnvironmentError> {
    crate::utils::io::download_streaming(
        mgr.http_client(),
        url,
        dest,
        512 * 1024 * 1024,
    )
        .await
        .map_err(|e| EnvironmentError::MinGitDownloadFailed(e.to_string()))
}

/// 解压 MinGit zip 到目标目录（同步，在 spawn_blocking 中执行）
fn extract_mingit_zip(zip_path: &std::path::Path, dest_dir: &std::path::Path) -> std::io::Result<()> {
    // 清理旧目录后全量解压（无过滤，保留 MinGit 目录结构）
    if dest_dir.exists() {
        std::fs::remove_dir_all(dest_dir)?;
    }
    std::fs::create_dir_all(dest_dir)?;
    crate::utils::io::extract_zip(zip_path, dest_dir, |_| true)
}
