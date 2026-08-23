//! 下载执行：流式下载 + SHA256 增量校验 + staging 管理
//!
//! 从下载包 URL 流式拉取 zip 到 staging 目录，下载过程中增量计算 SHA256，
//! 完成后与清单中的预期摘要比对；校验通过后解压到 `extracted/`。

use std::path::{Path, PathBuf};

use futures::StreamExt;
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::updater::apply::EXE_NAME;
use crate::updater::error::UpdaterError;
use crate::updater::UpdateInfo;

/// 已暂存的更新（下载校验 + 解压后的产物）
#[derive(Clone, Debug)]
pub struct StagedUpdate {
    /// 暂存的版本号
    pub version: Version,
    /// 解压后的 exe 路径
    pub extracted_exe: PathBuf,
}

/// 下载总超时（5 分钟）
pub(crate) const DOWNLOAD_TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// 更新压缩包大小上限，避免异常响应占满磁盘。
pub(crate) const MAX_UPDATE_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

/// 流式下载并执行 SHA256 校验
///
/// 下载前强制校验 URL 为 HTTPS；流式写入临时文件时增量更新 `Sha256`，
/// 完成后比对摘要，不匹配则删除临时文件并返回 [`UpdaterError::ChecksumMismatch`]。
/// 校验通过后将临时文件原子重命名为正式 zip 路径。
///
/// `on_progress` 在每收到一个 chunk 后被调用（传入 0~100 的进度百分比），可用于推送状态。
pub(crate) async fn download_and_verify(
    client: &reqwest::Client,
    info: &UpdateInfo,
    staging_dir: &Path,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<PathBuf, UpdaterError> {
    // 安全检查：仅允许 HTTPS
    if !info.url.starts_with("https://") {
        return Err(UpdaterError::HttpsRequired(info.url.clone()));
    }

    let response = client
        .get(&info.url)
        .timeout(DOWNLOAD_TOTAL_TIMEOUT)
        .header("User-Agent", "campus-auth-updater")
        .send()
        .await
        .map_err(UpdaterError::DownloadFailed)?
        .error_for_status()
        .map_err(UpdaterError::DownloadFailed)?;

    let content_length = response.content_length();
    if content_length.is_some_and(|size| size > MAX_UPDATE_ARCHIVE_BYTES) {
        return Err(UpdaterError::DownloadTooLarge {
            limit: MAX_UPDATE_ARCHIVE_BYTES,
        });
    }

    tokio::fs::create_dir_all(staging_dir)
        .await
        .map_err(UpdaterError::StagingDirCreateFailed)?;

    let tmp_path = staging_dir.join(format!("campus-auth-{}.zip.tmp", info.latest_version));
    // 简单策略：存在旧 tmp 则删除后重新下载（不实现断点续传）
    if tokio::fs::try_exists(&tmp_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }

    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(UpdaterError::PendingWriteFailed)?;

    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0u64;
    let mut stream = response.bytes_stream();
    let mut last_reported_pct: Option<u8> = None;
    // 进度回调节流：仅距上次上报 ≥500ms 或已到 100% 时才触发。
    // 回调会引发 WS 状态全量广播，逐 chunk 上报（每 1% 一次）在高速下载时
    // 会形成广播风暴。
    let mut last_report = std::time::Instant::now();
    const PROGRESS_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpdaterError::DownloadFailed)?;
        hasher.update(chunk.as_ref());
        file.write_all(chunk.as_ref())
            .await
            .map_err(UpdaterError::PendingWriteFailed)?;
        downloaded += chunk.len() as u64;
        if downloaded > MAX_UPDATE_ARCHIVE_BYTES {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(UpdaterError::DownloadTooLarge {
                limit: MAX_UPDATE_ARCHIVE_BYTES,
            });
        }

        if let (Some(total), Some(cb)) = (content_length, on_progress) {
            // 进度百分比恒钳制在 0~100：服务端实发字节超 Content-Length 时
            // (downloaded*100/total) 可能 >255，直接 as u8 会回绕（7.3）
            if let Some(pct) = (downloaded * 100).checked_div(total) {
                let percent = pct.min(100) as u8;
                // 节流放行条件：距上次上报 ≥500ms，或进度已到 100%（最终进度必须送达）；
                // 且仅对单调递增的百分比上报，避免重复回调
                if (percent >= 100 || last_report.elapsed() >= PROGRESS_REPORT_INTERVAL)
                    && last_reported_pct.is_none_or(|last| percent > last)
                {
                    cb(percent);
                    last_reported_pct = Some(percent);
                    last_report = std::time::Instant::now();
                }
            }
        }
    }
    file.flush().await.map_err(UpdaterError::PendingWriteFailed)?;
    drop(file);

    let actual = hex::encode(hasher.finalize());
    // 完整性校验针对下载的 ZIP 本身；解压后的 exe 不应拿 ZIP 摘要比较。
    if !info.sha256.is_empty() && actual.to_lowercase() != info.sha256.to_lowercase() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(UpdaterError::ChecksumMismatch {
            expected: info.sha256.clone(),
            actual,
        });
    } else if info.sha256.is_empty() {
        tracing::warn!("SHA256 校验值为空，跳过摘要校验，信任 HTTPS");
    }

    let zip_path = staging_dir.join(format!("campus-auth-{}.zip", info.latest_version));
    tokio::fs::rename(&tmp_path, &zip_path)
        .await
        .map_err(UpdaterError::PendingWriteFailed)?;
    Ok(zip_path)
}

/// 将 zip 解压到 `staging_dir/extracted/`，并校验解压出的 exe 存在
///
/// 使用 `zip::ZipFile::enclosed_name()` 过滤绝对/穿越路径，并额外做 `starts_with`
/// 防御性检查（防止 zip slip 攻击）。
///
/// 解压为 CPU+I/O 密集操作，大更新包可能持续数秒~数十秒；通过
/// `tokio::task::spawn_blocking` 在阻塞线程池执行，避免长时间占用 tokio worker 线程。
pub(crate) async fn extract_to_staging(
    zip_path: &Path,
    staging_dir: &Path,
    version: &str,
) -> Result<StagedUpdate, UpdaterError> {
    // 参数为引用，clone 为 owned 后 move 进 spawn_blocking 闭包（闭包需 'static + Send）
    let zip_path = zip_path.to_path_buf();
    let staging_dir = staging_dir.to_path_buf();
    let version = version.to_string();
    tokio::task::spawn_blocking(move || {
        extract_to_staging_blocking(&zip_path, &staging_dir, &version)
    })
    .await
    .map_err(|e| UpdaterError::ExtractFailed(format!("解压任务执行失败: {e}")))?
}

/// 同步解压实现：实际执行 zip 解压与 exe 校验（由 `extract_to_staging` 在阻塞线程池调用）
fn extract_to_staging_blocking(
    zip_path: &Path,
    staging_dir: &Path,
    version: &str,
) -> Result<StagedUpdate, UpdaterError> {
    let extracted_dir = staging_dir.join("extracted");
    // 清理上一次残留的 extracted/
    if extracted_dir.exists() {
        let _ = std::fs::remove_dir_all(&extracted_dir);
    }
    std::fs::create_dir_all(&extracted_dir).map_err(UpdaterError::StagingDirCreateFailed)?;

    // 全量解压（zip 打开/解析错误统一映射为 ExtractFailed；路径穿越由 extract_zip 兜底跳过）
    crate::utils::io::extract_zip(zip_path, &extracted_dir, |_| true)
        .map_err(|e| UpdaterError::ExtractFailed(e.to_string()))?;

    let extracted_exe = extracted_dir.join(EXE_NAME);
    if !extracted_exe.exists() {
        return Err(UpdaterError::ExtractFailed("解压结果缺少可执行文件".into()));
    }

    Ok(StagedUpdate {
        version: Version::parse(version).map_err(UpdaterError::VersionParseFailed)?,
        extracted_exe,
    })
}
