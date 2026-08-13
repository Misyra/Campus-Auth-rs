//! uv 下载 + 调用封装

use std::path::Path;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::environment::{
    EnvironmentError, EnvironmentManager, UV_DOWNLOAD_MAX_RETRIES, UV_DOWNLOAD_RETRY_DELAY,
    UV_DOWNLOAD_TIMEOUT, UV_EXE_NAME, UV_RELEASES_BASE, UV_SYNC_TIMEOUT, UV_TARGET,
};

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
            Ok(text) => text.split_whitespace()
                .next()
                .unwrap_or("")
                .to_string(),
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

        // 5. 原子安装：rename 到目标位置
        let _ = tokio::fs::remove_file(&tmp_zip).await;

        if let Err(e) = tokio::fs::rename(&tmp_exe, &uv_dest).await {
            // Windows 跨卷 rename 可能失败，回退到 copy + delete
            tracing::warn!("rename 失败，尝试 copy: {}", e);
            if let Err(e2) = tokio::fs::copy(&tmp_exe, &uv_dest).await {
                let _ = tokio::fs::remove_file(&tmp_exe).await;
                return Err(EnvironmentError::UvExtractFailed(e2));
            }
            let _ = tokio::fs::remove_file(&tmp_exe).await;
        }

        // 6. 验证可执行
        let output = tokio::process::Command::new(&uv_dest)
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
        let resp = match mgr
            .http_client()
            .get(url)
            .header("User-Agent", "campus-auth")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("GitHub API 镜像失败 {}: {}", url, e);
                last_err = e.to_string();
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
    let resp = mgr
        .http_client()
        .get(url)
        .header("User-Agent", "campus-auth")
        .send()
        .await
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
    let resp = mgr
        .http_client()
        .get(url)
        .header("User-Agent", "campus-auth")
        .send()
        .await
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })?;

    let resp = resp
        .error_for_status()
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })?;
        file.write_all(chunk.as_ref())
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;
    }
    file.flush().await.map_err(EnvironmentError::UvExtractFailed)?;
    Ok(())
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

/// 从 zip 中提取 uv.exe 到目标路径
fn extract_uv_from_zip(zip_path: &Path, dest: &Path) -> std::io::Result<()> {
    use std::io::Read;

    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // 查找 uv.exe（zip 内路径通常为 uv-{target}/uv.exe）
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let name = match entry.enclosed_name() {
            Some(n) => n,
            None => continue,
        };

        // 匹配 uv.exe 或 uv（Unix）
        if name
            .file_name()
            .is_some_and(|f| f == UV_EXE_NAME || f == "uv")
        {
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            std::fs::write(dest, &contents)?;
            return Ok(());
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "zip 中未找到 uv 可执行文件",
    ))
}

/// 执行 `uv sync` 安装 Python 虚拟环境与依赖
pub async fn run_uv_sync(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    // 前置检查：worker 项目目录存在
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }

    let uv_exe = mgr.env_path().join(UV_EXE_NAME);
    if !uv_exe.exists() {
        return Err(EnvironmentError::UvExtractFailed(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "uv.exe 不存在，请先下载 uv",
        )));
    }

    // 确保 environment/ 目录存在
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    // 构造 uv sync 命令，设置 UV_PROJECT_ENVIRONMENT 控制 venv 位置
    let cmd_future = tokio::process::Command::new(&uv_exe)
        .args([
            "sync",
            "--project",
            &mgr.worker_project_path().to_string_lossy(),
        ])
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

/// 以 environment/uv.exe 执行一条 uv 子命令
pub async fn run_uv_command(
    mgr: &EnvironmentManager,
    args: &[&str],
) -> Result<(), EnvironmentError> {
    let uv_exe = mgr.env_path().join(UV_EXE_NAME);
    let output = tokio::process::Command::new(&uv_exe)
        .args(args)
        .output()
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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
async fn download_text_with_mirrors(mgr: &EnvironmentManager, urls: &[String]) -> Result<String, EnvironmentError> {
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
    Err(EnvironmentError::GitHubApiError(format!("所有镜像均失败: {last_err}")))
}


