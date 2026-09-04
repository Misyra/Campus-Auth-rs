//! 下载执行：流式下载 + SHA256 增量校验 + staging 管理
//!
//! 从下载包 URL 流式拉取压缩包到 staging 目录，下载过程中增量计算 SHA256，
//! 完成后与清单中的预期摘要比对；校验通过后按资产扩展名分派解压到 `extracted/`
//! （Windows `.zip` / unix `.tar.gz`）。

use std::path::{Path, PathBuf};

use futures::StreamExt;
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::updater::UpdateInfo;
use crate::updater::apply::EXE_NAME;
use crate::updater::error::UpdaterError;

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
/// 校验通过后将临时文件原子重命名为正式压缩包路径。
///
/// `on_progress` 在每收到一个 chunk 后被调用（传入 0~100 的进度百分比），可用于推送状态。
pub(crate) async fn download_and_verify(
    client: &reqwest::Client,
    info: &UpdateInfo,
    staging_dir: &Path,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<PathBuf, UpdaterError> {
    // 严格校验：https 放行，http 仅精确回环（拒绝前缀绕过与 userinfo）
    if !crate::updater::check::is_allowed_update_url(&info.url) {
        return Err(UpdaterError::HttpsRequired(info.url.clone()));
    }
    // 缺失 SHA256 直接拒绝（不上签名，但不降级）
    if info.sha256.is_empty() {
        return Err(UpdaterError::MissingChecksum);
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
    // 重定向收敛：最终 URL 仍须通过白名单
    if !crate::updater::check::is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }
    let content_length = response.content_length();
    if content_length.is_some_and(|size| size > MAX_UPDATE_ARCHIVE_BYTES) {
        return Err(UpdaterError::DownloadTooLarge {
            limit: MAX_UPDATE_ARCHIVE_BYTES,
        });
    }
    let download_start = std::time::Instant::now();
    tracing::info!(
        url = %info.url,
        expected_size = ?content_length,
        "开始下载更新包"
    );

    tokio::fs::create_dir_all(staging_dir)
        .await
        .map_err(UpdaterError::StagingDirCreateFailed)?;

    // 资产文件名跟随下载 URL（Windows 发布 zip / unix 发布 tar.gz），
    // 落盘命名与后续解压分派都以此为据，不再硬编码 .zip
    let archive_name = archive_name_from_url(&info.url);
    let tmp_path = staging_dir.join(format!("{archive_name}.tmp"));
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
            if let Err(e) = tokio::fs::remove_file(&tmp_path).await {
                tracing::debug!("清理超限临时下载文件失败: {e}");
            }
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
    file.flush()
        .await
        .map_err(UpdaterError::PendingWriteFailed)?;
    drop(file);

    tracing::info!(
        bytes = downloaded,
        elapsed_ms = download_start.elapsed().as_millis() as u64,
        "更新包下载完成"
    );

    let actual = hex::encode(hasher.finalize());
    // 完整性校验针对下载的 ZIP 本身；缺失已在入口拒绝，此处仅比对
    if actual.to_lowercase() != info.sha256.to_lowercase() {
        if let Err(e) = tokio::fs::remove_file(&tmp_path).await {
            tracing::debug!("清理校验失败的临时下载文件失败: {e}");
        }
        return Err(UpdaterError::ChecksumMismatch {
            expected: info.sha256.clone(),
            actual,
        });
    }

    let archive_path = staging_dir.join(&archive_name);
    tokio::fs::rename(&tmp_path, &archive_path)
        .await
        .map_err(UpdaterError::PendingWriteFailed)?;
    Ok(archive_path)
}

/// 从下载 URL 提取资产文件名（截断 query / fragment）
///
/// 更新包落盘与解压分派都跟随官方资产名（Windows `.zip` / unix `.tar.gz`）；
/// URL 无可解析段时回退固定名（zip 兜底）。
fn archive_name_from_url(url: &str) -> String {
    let path_only = url.split(['?', '#']).next().unwrap_or("");
    path_only
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("campus-auth-update.zip")
        .to_string()
}

/// 将压缩包解压到 `staging_dir/extracted/`，并校验解压出的 exe 存在
///
/// 解压按资产扩展名分派（`.zip` / `.tar.gz`，见 `utils::io::extract_archive`）：
/// - 路径穿越由 `enclosed_name()` / tar slip 防护过滤，并额外做 `starts_with`
///   防御性检查（防止 zip/tar slip 攻击）；unix 上恢复条目权限位（含 +x）。
///
/// 解压为 CPU+I/O 密集操作，大更新包可能持续数秒~数十秒；通过
/// `tokio::task::spawn_blocking` 在阻塞线程池执行，避免长时间占用 tokio worker 线程。
pub(crate) async fn extract_to_staging(
    archive_path: &Path,
    staging_dir: &Path,
    version: &str,
) -> Result<StagedUpdate, UpdaterError> {
    // 参数为引用，clone 为 owned 后 move 进 spawn_blocking 闭包（闭包需 'static + Send）
    let archive_path = archive_path.to_path_buf();
    let staging_dir = staging_dir.to_path_buf();
    let version = version.to_string();
    tokio::task::spawn_blocking(move || {
        extract_to_staging_blocking(&archive_path, &staging_dir, &version)
    })
    .await
    .map_err(|e| UpdaterError::ExtractFailed(format!("解压任务执行失败: {e}")))?
}

/// 同步解压实现：实际执行压缩包解压与 exe 校验（由 `extract_to_staging` 在阻塞线程池调用）
fn extract_to_staging_blocking(
    archive_path: &Path,
    staging_dir: &Path,
    version: &str,
) -> Result<StagedUpdate, UpdaterError> {
    let extracted_dir = staging_dir.join("extracted");
    // 清理上一次残留的 extracted/
    if extracted_dir.exists() {
        let _ = std::fs::remove_dir_all(&extracted_dir);
    }
    std::fs::create_dir_all(&extracted_dir).map_err(UpdaterError::StagingDirCreateFailed)?;

    // 全量解压（压缩包打开/解析错误统一映射为 ExtractFailed；路径穿越由
    // extract_archive 兜底跳过）
    crate::utils::io::extract_archive(archive_path, &extracted_dir, |_| true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::routing::get;

    fn test_info(url: &str, sha256: &str) -> crate::updater::UpdateInfo {
        crate::updater::UpdateInfo {
            current_version: "5.0.0-alpha.6".to_string(),
            latest_version: "5.0.1-alpha.1".to_string(),
            update_available: true,
            url: url.to_string(),
            sha256: sha256.to_string(),
            size: None,
            notes: None,
            release_date: None,
        }
    }

    /// 起本地文件服务：固定字节 + Content-Length（回环 http 在更新白名单内）
    async fn serve_bytes(payload: Vec<u8>) -> SocketAddr {
        // Body::from(Vec<u8>) 自带精确 Content-Length，走进度上报分支
        let app = axum::Router::new().route(
            "/pkg.zip",
            get(move || {
                let payload = payload.clone();
                async move {
                    (
                        StatusCode::OK,
                        [("content-type", "application/zip")],
                        Body::from(payload),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    /// 回环直连：测试机代理（如 127.0.0.1:7890）会劫持 reqwest 回环请求导致 502
    fn loopback_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(data))
    }

    /// 成功路径：下载字节一致、落盘重命名完成、tmp 清理
    #[tokio::test]
    async fn download_and_verify_roundtrip() {
        let payload = b"fake-update-payload-12345".to_vec();
        let addr = serve_bytes(payload.clone()).await;
        let url = format!("http://{addr}/pkg.zip");
        let info = test_info(&url, &sha256_hex(&payload));
        let tmp = tempfile::tempdir().unwrap();
        let client = loopback_client();

        let path = download_and_verify(&client, &info, tmp.path(), None)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        // tmp 中间文件已重命名，无残留 .tmp
        let mut rd = tokio::fs::read_dir(tmp.path()).await.unwrap();
        let mut names = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            names.push(e.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["pkg.zip"]);
    }

    /// 摘要不符 → ChecksumMismatch 且不留文件
    #[tokio::test]
    async fn download_rejects_checksum_mismatch() {
        let payload = b"fake-update-payload-12345".to_vec();
        let addr = serve_bytes(payload).await;
        let url = format!("http://{addr}/pkg.zip");
        let info = test_info(&url, &"0".repeat(64));
        let tmp = tempfile::tempdir().unwrap();
        let client = loopback_client();

        let err = download_and_verify(&client, &info, tmp.path(), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, UpdaterError::ChecksumMismatch { .. }),
            "{err}"
        );
        let mut rd = tokio::fs::read_dir(tmp.path()).await.unwrap();
        assert!(rd.next_entry().await.unwrap().is_none());
    }

    /// 非白名单 URL 不出网直接拒绝；缺 SHA 不出网直接拒绝
    #[tokio::test]
    async fn download_rejects_url_and_missing_sha_without_network() {
        let client = loopback_client();
        let tmp = tempfile::tempdir().unwrap();
        let err = download_and_verify(
            &client,
            &test_info("http://example.com/pkg.zip", &"0".repeat(64)),
            tmp.path(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, UpdaterError::HttpsRequired(_)), "{err}");
        let err = download_and_verify(
            &client,
            &test_info("http://127.0.0.1:9/pkg.zip", ""),
            tmp.path(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, UpdaterError::MissingChecksum), "{err}");
    }
}
