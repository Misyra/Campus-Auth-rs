//! uv 下载 + 调用封装

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::environment::{
    EnvironmentError, EnvironmentManager, UV_DOWNLOAD_MAX_RETRIES, UV_DOWNLOAD_RETRY_DELAY,
    UV_DOWNLOAD_TIMEOUT, UV_EXE_NAME, UV_MIN_VERSION, UV_RELEASES_BASE, UV_SYNC_TIMEOUT, UV_TARGET,
};

/// 确定 uv 可执行文件路径：本地 `environment/uv.exe` 存在则用本地路径，否则回退到
/// PATH 中的 `uv`（`Command::new("uv")` 自动走 PATH 解析）。
///
/// 修复 5.4：bootstrap 判定 "PATH 上有 uv 即就绪" 只发生在 `uv_ready`，但阶段 2/3
/// 硬编码本地路径导致 PATH-only 机器 uv sync 必失败。统一走本 helper 后两者一致。
pub fn uv_exe_path(mgr: &EnvironmentManager) -> std::path::PathBuf {
    let local = mgr.env_path().join(UV_EXE_NAME);
    if local.exists() {
        local
    } else {
        std::path::PathBuf::from("uv")
    }
}

/// 解析 `uv --version` 输出中的版本号（形如 "uv 0.5.0 (...)"、Windows 下 "uv 0.5.0"）
fn parse_uv_version<N: AsRef<str>>(output: N) -> Option<semver::Version> {
    let line = output.as_ref().lines().next()?;
    let tok = line.split_whitespace().nth(1)?;
    semver::Version::parse(tok).ok()
}

/// 校验 PATH 上是否可调用 uv 且满足最低版本要求（UV_MIN_VERSION）
///
/// 供 `check_environment` 的 PATH 回退分支使用：PATH 上的 uv 过旧则视为未就绪，
/// 触发引导下载最新版，避免旧版 uv 语法/行为不兼容导致 sync 失败。
pub async fn check_uv_on_path() -> bool {
    let out = match tokio::process::Command::new("uv")
        .arg("--version")
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(ver) = parse_uv_version(&text) else {
        return false;
    };
    let Ok(min) = semver::Version::parse(UV_MIN_VERSION) else {
        return false;
    };
    ver >= min
}

/// 实际启动本地 uv 并检查退出状态（F11）
///
/// 仅凭 `uv.exe` 文件存在会误判半成品为就绪（上次安装 copy 回退失败残留），
/// 参照 `python_executable_works` 模式：执行 `uv --version` 验证确实可启动，
/// Windows 上加 CREATE_NO_WINDOW 避免环境检查弹黑窗。
pub(crate) async fn uv_executable_works(uv_exe: &Path) -> bool {
    if !uv_exe.is_file() {
        return false;
    }
    let mut cmd = uv_command(uv_exe);
    cmd.arg("--version");
    matches!(
        tokio::time::timeout(Duration::from_secs(5), cmd.output()).await,
        Ok(Ok(output)) if output.status.success()
    )
}

/// 从 GitHub Releases 下载 uv 二进制、SHA256 校验、解压到 environment/uv.exe
pub async fn download_uv(mgr: &EnvironmentManager) -> Result<std::path::PathBuf, EnvironmentError> {
    let env_path = mgr.env_path();
    let uv_dest = env_path.join(UV_EXE_NAME);

    // 获取版本号：优先使用锁定版本，否则查询 GitHub API
    let version = match crate::environment::UV_PINNED_VERSION {
        Some(v) => v.to_string(),
        None => fetch_latest_uv_version(mgr).await?,
    };

    let mut last_err_msg = String::new();

    for attempt in 0..UV_DOWNLOAD_MAX_RETRIES {
        // 检查取消
        if mgr.cancel_token().is_cancelled() {
            return Err(EnvironmentError::Cancelled);
        }

        // 每次重试重新获取版本号（首次已在循环外获取）
        let ver = if attempt == 0 {
            version.clone()
        } else {
            match crate::environment::UV_PINNED_VERSION {
                Some(v) => v.to_string(),
                None => fetch_latest_uv_version(mgr).await?,
            }
        };
        let sha_urls = uv_sha_urls(&ver);
        let zip_urls = uv_zip_urls(&ver);

        // 1. 下载 SHA256 校验文件（多镜像）
        let expected_hash = match download_text_with_mirrors(mgr, &sha_urls).await {
            Ok(text) => text.split_whitespace().next().unwrap_or("").to_string(),
            Err(e) => {
                tracing::warn!(
                    "下载 uv SHA256 文件失败 (尝试 {}/{}): {}",
                    attempt + 1,
                    UV_DOWNLOAD_MAX_RETRIES,
                    e
                );
                last_err_msg = e.to_string();
                tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
                continue;
            }
        };

        // 2. 流式下载 zip 到临时文件（多镜像 + 带超时）
        let tmp_zip = env_path.join("uv.zip.tmp");
        let mut zip_downloaded = false;
        for zip_url in &zip_urls {
            if mgr.cancel_token().is_cancelled() {
                return Err(EnvironmentError::Cancelled);
            }
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            let dl_result = tokio::time::timeout(
                UV_DOWNLOAD_TIMEOUT,
                download_file_streaming(mgr, zip_url, &tmp_zip),
            )
            .await;
            match dl_result {
                Ok(Ok(())) => {
                    zip_downloaded = true;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::debug!("zip 下载失败 {}: {}", zip_url, e);
                    last_err_msg = e.to_string();
                }
                Err(_) => {
                    tracing::debug!("zip 下载超时: {}", zip_url);
                    last_err_msg = format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs());
                }
            }
        }
        if !zip_downloaded {
            tracing::warn!(
                "下载 uv zip 全部镜像失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                last_err_msg
            );
            tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
            continue;
        }

        // 3. SHA256 校验
        if let Err(e) = verify_sha256(&tmp_zip, &expected_hash).await {
            tracing::warn!(
                "uv SHA256 校验失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            last_err_msg = e.to_string();
            tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
            continue;
        }

        // 4. 解压 zip 提取 uv.exe
        let tmp_exe = env_path.join("uv.exe.tmp");
        if let Err(e) = extract_uv_from_zip(&tmp_zip, &tmp_exe) {
            tracing::warn!(
                "uv 解压失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_zip).await;
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 5. 原子安装：rename 到目标位置（跨卷回退走 copy→临时名→rename）
        let _ = tokio::fs::remove_file(&tmp_zip).await;

        // F11/A6：统一走 utils::io::rename_or_copy——rename 失败（跨卷）时
        // copy 到目标同目录临时名再原子 rename，目标位置永远不会出现半成品；
        // copy 失败自动清理临时文件，残留的 tmp_exe 一并清除。
        if let Err(e) = crate::utils::io::rename_or_copy(&tmp_exe, &uv_dest).await {
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 6. 验证可执行
        let output = uv_command(&uv_dest)
            .arg("--version")
            .output()
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;

        if !output.status.success() {
            return Err(EnvironmentError::UvExtractFailed(std::io::Error::other(
                "uv --version 执行失败",
            )));
        }

        tracing::info!("uv 下载安装成功: {}", uv_dest.display());
        return Ok(uv_dest);
    }

    // 所有重试均失败
    Err(EnvironmentError::UvDownloadIoFailed {
        retries: UV_DOWNLOAD_MAX_RETRIES,
        message: last_err_msg,
    })
}

/// 通过 GitHub API 获取 uv 最新版本号（多镜像）
async fn fetch_latest_uv_version(mgr: &EnvironmentManager) -> Result<String, EnvironmentError> {
    let urls = github_api_urls();
    let mut last_err = String::new();

    for url in &urls {
        let resp = match tokio::time::timeout(
            UV_DOWNLOAD_TIMEOUT,
            mgr.http_client()
                .get(url)
                .header("User-Agent", "campus-auth")
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::debug!("GitHub API 镜像失败 {}: {}", url, e);
                last_err = e.to_string();
                continue;
            }
            Err(_) => {
                tracing::debug!(
                    "GitHub API 镜像超时 {}: {}",
                    url,
                    UV_DOWNLOAD_TIMEOUT.as_secs()
                );
                last_err = format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs());
                continue;
            }
        };

        if !resp.status().is_success() {
            tracing::debug!("GitHub API 镜像失败 {}: HTTP {}", url, resp.status());
            last_err = format!("HTTP {}", resp.status());
            continue;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!("GitHub API 镜像解析失败 {}: {}", url, e);
                last_err = e.to_string();
                continue;
            }
        };

        if let Some(tag) = json["tag_name"].as_str() {
            let version = tag.strip_prefix('v').unwrap_or(tag);
            return Ok(version.to_string());
        }
        last_err = "tag_name 字段缺失".to_string();
    }

    Err(EnvironmentError::GitHubApiError(format!(
        "所有 GitHub API 镜像均失败: {last_err}"
    )))
}

/// 下载文本内容（用于获取 SHA256 校验文件）
async fn download_text(mgr: &EnvironmentManager, url: &str) -> Result<String, EnvironmentError> {
    let resp = tokio::time::timeout(
        UV_DOWNLOAD_TIMEOUT,
        mgr.http_client()
            .get(url)
            .header("User-Agent", "campus-auth")
            .send(),
    )
    .await
    .map_err(|_| EnvironmentError::UvDownloadIoFailed {
        retries: 0,
        message: format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs()),
    })?
    .map_err(|e| EnvironmentError::UvDownloadFailed {
        retries: 0,
        source: e,
    })?;

    resp.text()
        .await
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })
}

/// 流式下载文件到指定路径（不含超时控制，由调用方包裹）
async fn download_file_streaming(
    mgr: &EnvironmentManager,
    url: &str,
    dest: &Path,
) -> Result<(), EnvironmentError> {
    crate::utils::io::download_streaming(mgr.http_client(), url, dest, 256 * 1024 * 1024)
        .await
        .map_err(|e| match e {
            crate::utils::io::DownloadError::Http(e) => EnvironmentError::UvDownloadFailed {
                retries: 0,
                source: e,
            },
            crate::utils::io::DownloadError::Io(e) => EnvironmentError::UvExtractFailed(e),
            crate::utils::io::DownloadError::TooLarge { limit } => {
                EnvironmentError::UvDownloadIoFailed {
                    retries: 0,
                    message: format!("下载内容超过大小上限 {limit} 字节"),
                }
            }
        })
}

/// 校验文件 SHA256 与期望值一致
pub async fn verify_sha256(path: &Path, expected: &str) -> Result<(), EnvironmentError> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let got = hex::encode(digest);
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(EnvironmentError::UvChecksumMismatch {
            expected: expected.to_string(),
            got,
        })
    }
}

/// 从 zip 中提取 uv 可执行文件到目标路径
fn extract_uv_from_zip(zip_path: &Path, dest: &Path) -> std::io::Result<()> {
    // uv 二进制位于 zip 内 `uv-{target}/uv.exe`（或 `uv`）。先按文件名过滤解压到
    // 临时目录，再把找到的可执行文件复制到目标路径（复用 extract_zip 模板）。
    let tmp_dir = tempfile::tempdir()?;
    let mut found: Option<PathBuf> = None;
    crate::utils::io::extract_zip(zip_path, tmp_dir.path(), |name| {
        if name
            .file_name()
            .is_some_and(|f| f == UV_EXE_NAME || f == "uv")
        {
            found = Some(tmp_dir.path().join(name));
            true
        } else {
            false
        }
    })?;
    let src = found.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "zip 中未找到 uv 可执行文件")
    })?;
    std::fs::copy(&src, dest)?;
    Ok(())
}

/// 执行 `uv sync` 安装 Python 虚拟环境与基础依赖（不含 OCR 可选依赖）。
///
/// OCR 依赖（ddddocr）不随 `uv sync` 默认安装；需要时经
/// [`install_ocr_dep`]（`uv add ddddocr`）单独添加，卸载经
/// [`remove_ocr_dep`]（`uv remove ddddocr`）。
pub async fn run_uv_sync(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    // 前置检查：worker 项目目录存在
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }

    let uv_exe = uv_exe_path(mgr);

    // 确保 environment/ 目录存在
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    // 构造 uv sync 命令，设置 UV_PROJECT_ENVIRONMENT 控制 venv 位置
    let cmd_future = uv_command(&uv_exe)
        .arg("sync")
        .arg("--project")
        .arg(&*mgr.worker_project_path().to_string_lossy())
        .env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path())
        .output();

    // 带超时执行
    let output = tokio::time::timeout(UV_SYNC_TIMEOUT, cmd_future)
        .await
        .map_err(|_| EnvironmentError::UvSyncTimeout {
            timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
        })?
        .map_err(EnvironmentError::UvExtractFailed)?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr,
        })
    }
}

/// 安装 OCR 依赖：`uv add ddddocr`
pub async fn install_ocr_dep(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    run_uv_dep(mgr, true).await
}

/// 卸载 OCR 依赖：`uv remove ddddocr`
pub async fn remove_ocr_dep(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    run_uv_dep(mgr, false).await
}

/// 执行 `uv add/remove ddddocr`（安装/卸载 OCR 依赖）
///
/// 在 worker 项目目录下执行并设置 UV_PROJECT_ENVIRONMENT 控制 venv 位置，
/// 与 `uv sync` 保持一致的运行环境。`uv add` 会同步更新 pyproject.toml 与
/// venv 内的 site-packages；`uv remove` 移除主依赖与已装入的包。
async fn run_uv_dep(mgr: &EnvironmentManager, add: bool) -> Result<(), EnvironmentError> {
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }
    let uv_exe = uv_exe_path(mgr);
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut cmd = uv_command(&uv_exe);
    cmd.arg(if add { "add" } else { "remove" })
        .arg("--project")
        .arg(&*mgr.worker_project_path().to_string_lossy())
        .arg("ddddocr")
        .env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path());

    let action = if add { "安装" } else { "卸载" };
    let output = tokio::time::timeout(Duration::from_secs(300), cmd.output())
        .await
        .map_err(|_| EnvironmentError::UvSyncTimeout {
            timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
        })?
        .map_err(EnvironmentError::UvExtractFailed)?;

    if output.status.success() {
        tracing::info!("OCR 依赖（ddddocr）{action}完成");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        tracing::error!("OCR 依赖（ddddocr）{action}失败: {stderr}");
        Err(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr,
        })
    }
}

/// 构造 uv 下载 URL（zip）— 主站
pub(crate) fn uv_zip_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.zip")
}

/// 构造 uv SHA256 文件 URL — 主站
pub(crate) fn uv_sha_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.zip.sha256")
}

/// 生成所有镜像的下载 URL 列表（主站 + 代理镜像）
fn uv_zip_urls(version: &str) -> Vec<String> {
    let base = uv_zip_url(version);
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_MIRRORS.len());
    // 先尝试直连
    urls.push(base.clone());
    // 再尝试代理镜像
    for mirror in crate::environment::GITHUB_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 生成所有镜像的 SHA256 URL 列表
fn uv_sha_urls(version: &str) -> Vec<String> {
    let base = uv_sha_url(version);
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_MIRRORS.len());
    urls.push(base.clone());
    for mirror in crate::environment::GITHUB_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 生成所有镜像的 GitHub API URL 列表
fn github_api_urls() -> Vec<String> {
    let base = "https://api.github.com/repos/astral-sh/uv/releases/latest";
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_API_MIRRORS.len());
    urls.push(base.to_string());
    for mirror in crate::environment::GITHUB_API_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 尝试从多个镜像下载文本，第一个成功即返回
async fn download_text_with_mirrors(
    mgr: &EnvironmentManager,
    urls: &[String],
) -> Result<String, EnvironmentError> {
    let mut last_err = String::new();
    tracing::info!("尝试 {} 个镜像下载", urls.len());
    for (i, url) in urls.iter().enumerate() {
        tracing::info!("镜像 {}/{}: {}", i + 1, urls.len(), url);
        match download_text(mgr, url).await {
            Ok(text) => {
                tracing::info!("镜像 {} 下载成功", url);
                return Ok(text);
            }
            Err(e) => {
                tracing::warn!("镜像 {} 失败: {}", url, e);
                last_err = e.to_string();
            }
        }
    }
    Err(EnvironmentError::GitHubApiError(format!(
        "所有镜像均失败: {last_err}"
    )))
}

/// 构造 uv 子进程 Command（Windows 上隐藏控制台窗口，避免环境引导弹黑窗）
pub(crate) fn uv_command(uv_exe: &std::path::Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(uv_exe);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusManager;
    use std::sync::Arc;

    /// URL 构造：zip 与 sha256 均指向主站对应文件
    #[test]
    fn test_uv_urls_format() {
        let expected = format!("/0.5.0/uv-{UV_TARGET}.zip");
        let zip = uv_zip_url("0.5.0");
        assert!(zip.ends_with(&expected), "zip: {zip}");
        let sha = uv_sha_url("0.5.0");
        assert!(sha.ends_with(".zip.sha256"), "sha: {sha}");
    }

    /// 镜像列表：直连在前，代理镜像在后，首项为直连
    #[test]
    fn test_uv_mirror_urls() {
        let zips = uv_zip_urls("0.5.0");
        assert_eq!(zips[0], uv_zip_url("0.5.0"));
        assert!(zips.len() > 1, "应包含代理镜像");
        let shas = uv_sha_urls("0.5.0");
        assert_eq!(shas[0], uv_sha_url("0.5.0"));
        assert_eq!(shas.len(), zips.len());
    }

    /// SHA256 校验：正确值通过，错误值被拒
    #[tokio::test]
    async fn test_verify_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, b"hello campus-auth").unwrap();
        let expected = hex::encode(Sha256::digest(b"hello campus-auth"));
        assert!(verify_sha256(&path, &expected).await.is_ok());
        assert!(verify_sha256(&path, "0000deadbeef").await.is_err());
    }

    /// zip 提取：从含 uv.exe 的 zip 中正确提取
    #[test]
    fn test_extract_uv_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("uv.zip");
        // 构造一个含 uv-{target}/uv.exe 的 zip
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("uv-0.5.0/uv.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write;
        zip.write_all(b"MZ fake-exe").unwrap();
        let cursor = zip.finish().unwrap();
        std::fs::write(&zip_path, cursor.into_inner()).unwrap();

        let dest = dir.path().join("uv.exe");
        extract_uv_from_zip(&zip_path, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ fake-exe");
    }

    /// 5.4：uv_exe_path 两分支——本地存在返回本地路径，否则回退到 PATH 的 `uv`
    #[test]
    fn test_uv_exe_path_two_branches() {
        let dir = tempfile::tempdir().unwrap();
        let status = Arc::new(StatusManager::new());
        let mgr = EnvironmentManager::new(dir.path().to_path_buf(), status, false);

        // 本地不存在 → 回退 PATH
        assert_eq!(uv_exe_path(&mgr), std::path::PathBuf::from("uv"));

        // 本地存在 → 返回本地路径
        let env = dir.path().join(crate::environment::ENV_DIR);
        std::fs::create_dir_all(&env).unwrap();
        std::fs::write(env.join(UV_EXE_NAME), b"fake").unwrap();
        assert_eq!(uv_exe_path(&mgr), env.join(UV_EXE_NAME));
    }

    /// 5.4：uv --version 输出解析（含 Windows 可能的括号后缀）
    #[test]
    fn test_parse_uv_version() {
        assert_eq!(
            parse_uv_version("uv 0.5.0 (9b1dd64fb 2024-11-26)"),
            Some(semver::Version::parse("0.5.0").unwrap())
        );
        assert_eq!(
            parse_uv_version("uv 0.6.1"),
            Some(semver::Version::parse("0.6.1").unwrap())
        );
        assert!(parse_uv_version("uv: unrecognized option").is_none());
    }

    /// F11：本地 uv 就绪判定加 --version 实启校验——
    /// 不存在的文件与不可执行的半成品内容都不得判为就绪
    #[tokio::test]
    async fn test_uv_executable_works_rejects_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        // 不存在的文件：文件级快速拒绝
        assert!(
            !uv_executable_works(&dir.path().join("missing-uv.exe")).await,
            "不存在的文件应判不可用"
        );
        // 半成品内容：文件存在但不是可执行映像，实际启动必然失败
        // （Windows 上 CreateProcess 拒绝非 PE 文件；Unix 上 execve 报 Exec 格式错误）
        let broken = dir.path().join("uv.exe");
        std::fs::write(&broken, b"half-written garbage").unwrap();
        assert!(
            !uv_executable_works(&broken).await,
            "半成品文件不得因 exists() 被判就绪"
        );
    }
}
