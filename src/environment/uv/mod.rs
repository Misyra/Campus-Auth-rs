//! uv 下载 + 调用封装（编排主流程与聚合重导出）
//!
//! [`download_uv`] 为 uv 引导的编排主流程：版本决策 → SHA256 校验文件下载 →
//! 压缩包多镜像流式下载 → 校验 → 解压提取 → 原子安装 → 实启校验。
//!
//! 子模块按职责划分：
//! - [`download`]：URL/镜像构造、镜像测速排序、文本/流式下载与压缩包提取；
//! - [`verify`]：uv 就绪校验（实启/PATH 探测）与 SHA256 校验；
//! - [`sync`]：`uv sync` 与 OCR 依赖交接（`uv add/remove` + 项目文件备份回滚）；
//! - [`process`]：取消/超时受控的子进程执行封装与流式输出。

mod download;
mod process;
mod sync;
mod verify;

pub(crate) use process::{CommandOutputError, command_output_streaming, uv_command};
pub use sync::{install_ocr_dep, reconcile_ocr_preference, remove_ocr_dep, run_uv_sync};
pub(crate) use verify::uv_executable_works;
pub use verify::{check_uv_on_path, uv_exe_path, verify_sha256};

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::environment::{
    EnvironmentError, EnvironmentManager, UV_DOWNLOAD_MAX_RETRIES, UV_DOWNLOAD_RETRY_DELAY,
    UV_DOWNLOAD_TIMEOUT, UV_EXE_NAME,
};

use download::{
    download_file_streaming, download_text_with_mirrors, extract_uv_from_archive,
    fetch_latest_uv_version, rank_mirror_urls, uv_archive_urls, uv_sha_urls,
};
use process::command_output_with_cancel;

/// 从 GitHub Releases 下载 uv 二进制、SHA256 校验、解压到 environment/uv.exe
pub async fn download_uv(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<std::path::PathBuf, EnvironmentError> {
    if cfg!(feature = "openwrt") {
        return Err(EnvironmentError::UnsupportedPlatform);
    }
    let env_path = mgr.env_path();
    let uv_dest = env_path.join(UV_EXE_NAME);

    // 获取版本号：优先使用锁定版本，否则查询 GitHub API
    let version = match crate::environment::UV_PINNED_VERSION {
        Some(v) => {
            tracing::debug!(version = %v, "uv 版本决策：使用锁定版本");
            v.to_string()
        }
        None => {
            let v = fetch_latest_uv_version(mgr).await?;
            tracing::debug!(version = %v, "uv 版本决策：未锁定，使用 latest 最新版");
            v
        }
    };

    let mut last_err_msg = String::new();

    for attempt in 0..UV_DOWNLOAD_MAX_RETRIES {
        // 检查取消
        if cancel.is_cancelled() {
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
        let archive_urls = uv_archive_urls(&ver);

        // 1. 下载 SHA256 校验文件（多镜像）；结果为空直接视为失败，避免后续校验永远不通过
        let expected_hash = match download_text_with_mirrors(mgr, &sha_urls).await {
            Ok(text) => {
                let hash = text.split_whitespace().next().unwrap_or("").to_string();
                if hash.is_empty() {
                    let msg = "SHA256 校验文件内容为空";
                    tracing::warn!(
                        "下载 uv SHA256 文件为空 (尝试 {}/{}): {}",
                        attempt + 1,
                        UV_DOWNLOAD_MAX_RETRIES,
                        msg
                    );
                    last_err_msg = msg.to_string();
                    let delay = UV_DOWNLOAD_RETRY_DELAY * (1u32 << attempt.min(3));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                hash
            }
            Err(e) => {
                tracing::warn!(
                    "下载 uv SHA256 文件失败 (尝试 {}/{}): {}",
                    attempt + 1,
                    UV_DOWNLOAD_MAX_RETRIES,
                    e
                );
                last_err_msg = e.to_string();
                let delay = UV_DOWNLOAD_RETRY_DELAY * (1u32 << attempt.min(3));
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        // 2. 流式下载压缩包到临时文件（先并发测速选最快镜像，再按序尝试）
        let tmp_archive = env_path.join("uv-archive.tmp");
        // 每轮下载前并发测速排序：最快的镜像优先，避免固定顺序下慢镜像拖满 300s 总超时
        let archive_order = rank_mirror_urls(mgr, &archive_urls).await;
        let mut archive_downloaded = false;
        for idx in archive_order {
            let archive_url = &archive_urls[idx];
            if cancel.is_cancelled() {
                return Err(EnvironmentError::Cancelled);
            }
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            // download_file_streaming 内部已带停滞检测，此处仅包一层总超时兜底
            let dl_result = tokio::time::timeout(
                UV_DOWNLOAD_TIMEOUT,
                download_file_streaming(mgr, archive_url, &tmp_archive, cancel),
            )
            .await;
            match dl_result {
                Ok(Ok(())) => {
                    archive_downloaded = true;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::debug!("压缩包下载失败 {}: {}", archive_url, e);
                    last_err_msg = e.to_string();
                }
                Err(_) => {
                    tracing::debug!("压缩包下载总超时: {}", archive_url);
                    last_err_msg = format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs());
                }
            }
            if cancel.is_cancelled() {
                return Err(EnvironmentError::Cancelled);
            }
        }
        if !archive_downloaded {
            tracing::warn!(
                "下载 uv 压缩包全部镜像失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                last_err_msg
            );
            let delay = UV_DOWNLOAD_RETRY_DELAY * (1u32 << attempt.min(3));
            tokio::time::sleep(delay).await;
            continue;
        }

        // 3. SHA256 校验
        if let Err(e) = verify_sha256(&tmp_archive, &expected_hash).await {
            tracing::warn!(
                "uv SHA256 校验失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            last_err_msg = e.to_string();
            let delay = UV_DOWNLOAD_RETRY_DELAY * (1u32 << attempt.min(3));
            tokio::time::sleep(delay).await;
            continue;
        }

        // 4. 解压提取 uv 可执行文件
        let tmp_exe = env_path.join("uv.tmp");
        if let Err(e) = extract_uv_from_archive(&tmp_archive, &tmp_exe) {
            tracing::warn!(
                "uv 解压失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 5. 原子安装：rename 到目标位置（跨卷回退走 copy→临时名→rename）
        let _ = tokio::fs::remove_file(&tmp_archive).await;

        // F11/A6：统一走 utils::io::rename_or_copy——rename 失败（跨卷）时
        // copy 到目标同目录临时名再原子 rename，目标位置永远不会出现半成品；
        // copy 失败自动清理临时文件，残留的 tmp_exe 一并清除。
        if let Err(e) = crate::utils::io::rename_or_copy(&tmp_exe, &uv_dest).await {
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 6. 验证可执行（ENV-2：带超时 + 响应取消 + kill_on_drop）——该步骤
        // 在持有 BootstrapGate 期间执行，裸 .output() 挂起会永久占住引导
        // 互斥门，整个环境子系统假死；对齐 uv_executable_works 的 5s 口径
        let cmd = {
            let mut c = uv_command(&uv_dest);
            c.arg("--version");
            c
        };
        let output = command_output_with_cancel(cmd, Duration::from_secs(5), cancel)
            .await
            .map_err(|e| match e {
                CommandOutputError::Cancelled => EnvironmentError::Cancelled,
                CommandOutputError::Timeout => EnvironmentError::UvExtractFailed(
                    std::io::Error::other("uv --version 校验超时（5s），疑似环境异常"),
                ),
                CommandOutputError::Io(e) => EnvironmentError::UvExtractFailed(e),
            })?;

        if !output.status.success() {
            return Err(EnvironmentError::UvExtractFailed(std::io::Error::other(
                "uv --version 执行失败",
            )));
        }

        tracing::info!("uv 下载安装成功: {}", uv_dest.display());
        return Ok(uv_dest);
    }

    // 所有重试均失败：归类原因并给出可操作建议（与 Playwright 安装失败同风格），
    // 重试次数由 UvDownloadIoFailed 的 Display 前缀（"重试 N 次"）携带，不再重复
    let (cause, hint) = classify_uv_download_failure(&last_err_msg);
    Err(EnvironmentError::UvDownloadIoFailed {
        retries: UV_DOWNLOAD_MAX_RETRIES,
        message: format!("原因：{cause}；建议：{hint}；详情：{last_err_msg}"),
    })
}

/// uv 下载失败归类：从聚合错误文本识别超时/连接/HTTP 状态/磁盘等模式，
/// 返回（原因，建议）随错误消息直接展示，帮助对照日志自查
/// （与 `python::classify_playwright_failure` 同风格）。
fn classify_uv_download_failure(detail: &str) -> (&'static str, &'static str) {
    let lower = detail.to_ascii_lowercase();
    if detail.contains("下载停滞") || detail.contains("下载超时") || lower.contains("timed out")
    {
        (
            "下载超时或停滞",
            "网络过慢或镜像限速；可稍后重试，或检查代理/防火墙设置",
        )
    } else if lower.contains("dns error")
        || lower.contains("dns failure")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("connection aborted")
        || lower.contains("unreachable")
        || lower.contains("error trying to connect")
        || lower.contains("certificate")
        || lower.contains("tls")
    {
        (
            "无法连接下载源",
            "检查网络是否已通过校园网认证、代理是否可用、防火墙是否放行 GitHub 及镜像站",
        )
    } else if detail.contains("HTTP status") || detail.contains("HTTP ") {
        // reqwest 状态错误形如 "HTTP status client error (404 ...)"，
        // GitHub API 镜像路径形如 "HTTP 403"
        (
            "下载源返回错误",
            "资源不存在或被限流（404/403/429/5xx）；可稍后重试",
        )
    } else if lower.contains("no space left") || lower.contains("enospc") {
        ("磁盘空间不足", "清理磁盘空间后重试")
    } else if detail.contains("大小上限") {
        ("下载内容异常", "网络劫持或镜像返回了异常内容；可稍后重试")
    } else {
        ("下载失败", "检查网络与代理设置后重试“初始化 Python 环境”")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 失败归类：超时/连接/HTTP/磁盘/兜底各走对应原因与建议。
    #[test]
    fn test_classify_uv_download_failure_branches() {
        let (cause, hint) =
            classify_uv_download_failure("下载停滞（60s 内无数据，已接收 1024 字节）");
        assert_eq!(cause, "下载超时或停滞");
        assert!(hint.contains("重试"));

        let (cause, _) = classify_uv_download_failure("下载超时 (超过 300s)");
        assert_eq!(cause, "下载超时或停滞");

        let (cause, hint) = classify_uv_download_failure(
            "uv 下载失败 (重试 0 次): error sending request for url (https://github.com): error trying to connect: dns error: failed to lookup",
        );
        assert_eq!(cause, "无法连接下载源");
        assert!(hint.contains("防火墙"));

        let (cause, _) = classify_uv_download_failure(
            "HTTP status client error (404 Not Found) for url (https://example.com/x)",
        );
        assert_eq!(cause, "下载源返回错误");

        let (cause, _) = classify_uv_download_failure("所有镜像均失败: HTTP 403");
        assert_eq!(cause, "下载源返回错误");

        let (cause, _) = classify_uv_download_failure("ENOSPC: no space left on device");
        assert_eq!(cause, "磁盘空间不足");

        let (cause, hint) = classify_uv_download_failure("boom");
        assert_eq!(cause, "下载失败");
        assert!(hint.contains("重试"));
    }
}
