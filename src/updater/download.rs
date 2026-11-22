//! 下载执行：流式下载 + SHA256 增量校验 + staging 管理
//!
//! 从下载包 URL 流式拉取压缩包到 staging 目录，下载过程中增量计算 SHA256，
//! 完成后与清单中的预期摘要比对；校验通过后按资产扩展名分派解压到 `extracted/`
//! （Windows `.zip` / unix `.tar.gz`）。

use std::path::{Path, PathBuf};

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::updater::UpdateInfo;
use crate::updater::apply::EXE_NAME;
use crate::updater::error::UpdaterError;
use crate::utils::io_human::{io_error, io_error_at};

/// 已暂存的更新（下载校验 + 解压后的产物）
///
/// 不携带版本号：网络下载路径的版本来自远程清单（[`UpdateInfo::latest_version`]），
/// 上传路径的版本在解压后从产物 exe 中提取（`version_info`），提取时机均在
/// 解压完成之后——解压本身不该依赖版本号。
#[derive(Clone, Debug)]
pub struct StagedUpdate {
    /// 解压后的 exe 路径
    pub extracted_exe: PathBuf,
}

/// 建立连接超时（TCP + TLS 握手，客户端级；见 [`crate::updater`] 的客户端构建）
pub(crate) const DOWNLOAD_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// 停滞判定：等待响应头 / 相邻 chunk 之间的最大空闲时间。
///
/// v5.0.0-alpha.7 前为"300s 总超时"——reqwest 的请求级 timeout 覆盖整个
/// 响应体读取，等于给下载设置了隐式速度下限（300s 内必须拉完整个包，
/// 大包/慢网必然失败且无断点续传），故改为连接超时 + 空闲超时模型：
/// 慢网只要还在出数据就允许继续，只有"彻底不出数据"才判失败。
pub(crate) const DOWNLOAD_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// 更新压缩包大小上限，避免异常响应占满磁盘。
pub(crate) const MAX_UPDATE_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

/// 直连失败是否值得经镜像代理重试：前置校验类错误（URL 白名单 / 缺摘要）
/// 与下载源无关，重试必现同错；其余（网络失败 / 停滞 / 超限 / 摘要不符）
/// 都可能因换源而成功——摘要不符常对应镜像缓存损坏，换源重试正是解法。
pub(crate) fn is_mirror_retryable(err: &UpdaterError) -> bool {
    !matches!(
        err,
        UpdaterError::HttpsRequired(_) | UpdaterError::MissingChecksum
    )
}

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

    // send() 阶段（等待响应头）同样受停滞超时保护：连接握手完成后服务端
    // 拖着不发响应头会无限挂起，connect_timeout 管不到这一段
    let response = match tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, async {
        client
            .get(&info.url)
            .header("User-Agent", "campus-auth-updater")
            .send()
            .await
            .map_err(UpdaterError::DownloadFailed)?
            .error_for_status()
            .map_err(UpdaterError::DownloadFailed)
    })
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(UpdaterError::DownloadStalled {
                idle_secs: DOWNLOAD_STALL_TIMEOUT.as_secs(),
                received_bytes: 0,
            });
        }
    };
    // 重定向已由客户端策略逐跳前置校验（updater_redirect_policy），此处兜底：
    // 最终 URL 仍须通过白名单（custom policy 下恒通过，无行为漂移）
    if !crate::updater::check::is_allowed_update_url(response.url().as_str()) {
        return Err(UpdaterError::HttpsRequired(response.url().to_string()));
    }
    let content_length = response.content_length();
    if content_length.is_some_and(|size| size > MAX_UPDATE_ARCHIVE_BYTES) {
        return Err(UpdaterError::DownloadTooLarge {
            limit: MAX_UPDATE_ARCHIVE_BYTES,
        });
    }
    // UPD-7：清单声明的预期大小参与交叉核对——与 Content-Length 不一致仅告警
    // 不中断（最终完整性由 SHA256 把关），避免镜像误标长度导致正常更新失败
    if let (Some(expected), Some(actual)) = (info.size, content_length)
        && expected != actual
    {
        tracing::warn!(
            expected,
            actual,
            "更新包 Content-Length 与清单声明大小不一致（以 SHA256 校验为准）"
        );
    }
    let download_start = std::time::Instant::now();
    tracing::info!(
        url = %info.url,
        expected_size = ?content_length,
        "开始下载更新包"
    );

    tokio::fs::create_dir_all(staging_dir).await.map_err(|e| {
        UpdaterError::StagingDirCreateFailed(io_error_at("创建更新暂存目录", staging_dir, e))
    })?;

    // 资产文件名跟随下载 URL（Windows 发布 zip / unix 发布 tar.gz），
    // 落盘命名与后续解压分派都以此为据，不再硬编码 .zip
    let archive_name = archive_name_from_url(&info.url);
    let tmp_path = staging_dir.join(format!("{archive_name}.tmp"));
    // 简单策略：存在旧 tmp 则删除后重新下载（不实现断点续传）
    if tokio::fs::try_exists(&tmp_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }

    let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|e| {
        UpdaterError::PendingWriteFailed(io_error_at("创建下载临时文件", &tmp_path, e))
    })?;

    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0u64;
    let mut stream = response.bytes_stream();
    let mut last_reported_pct: Option<u8> = None;
    // 进度回调节流：仅距上次上报 ≥500ms 或已到 100% 时才触发。
    // 回调会引发 WS 状态全量广播，逐 chunk 上报（每 1% 一次）在高速下载时
    // 会形成广播风暴。
    let mut last_report = std::time::Instant::now();
    const PROGRESS_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

    loop {
        // chunk 间停滞判定：超时内未收到任何数据即判失败（正常 EOF 走 Ok(None)）
        let chunk = match tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, stream.next()).await {
            Ok(Some(c)) => c.map_err(|e| {
                // UPD-2：失败路径统一清理半写入的 .tmp（对齐下方超限/校验分支）
                cleanup_tmp(&tmp_path);
                UpdaterError::DownloadFailed(e)
            })?,
            Ok(None) => break,
            Err(_) => {
                // UPD-2：停滞超时不得残留半写入的 .tmp（下次下载虽会重建，
                // 但 3 天 stale 清理前一直占着 staging 空间）
                cleanup_tmp(&tmp_path);
                return Err(UpdaterError::DownloadStalled {
                    idle_secs: DOWNLOAD_STALL_TIMEOUT.as_secs(),
                    received_bytes: downloaded,
                });
            }
        };
        hasher.update(chunk.as_ref());
        if let Err(e) = file.write_all(chunk.as_ref()).await {
            cleanup_tmp(&tmp_path);
            return Err(UpdaterError::PendingWriteFailed(io_error(
                "写入下载临时文件",
                e,
            )));
        }
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
    file.flush().await.map_err(|e| {
        cleanup_tmp(&tmp_path);
        UpdaterError::PendingWriteFailed(io_error("写入下载临时文件", e))
    })?;
    drop(file);

    tracing::info!(
        bytes = downloaded,
        elapsed_ms = download_start.elapsed().as_millis() as u64,
        "更新包下载完成"
    );
    // UPD-7：实际字节数与清单声明不一致仅告警（完整性以 SHA256 校验为准）
    if let Some(expected) = info.size
        && downloaded != expected
    {
        tracing::warn!(
            expected,
            actual = downloaded,
            "更新包实际字节数与清单声明不一致（以 SHA256 校验为准）"
        );
    }

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
        .map_err(|e| {
            UpdaterError::PendingWriteFailed(io_error_at("保存下载完成的更新包", &archive_path, e))
        })?;
    Ok(archive_path)
}

/// 从下载 URL 提取资产文件名（截断 query / fragment，防御性净化）
///
/// 更新包落盘与解压分派都跟随官方资产名（Windows `.zip` / unix `.tar.gz`）；
/// URL 无可解析段、末段净化后为空（如 `..`）或超长时回退固定名（zip 兜底）。
/// 资产名来自 GitHub 且 URL 未解码，正常路径不会命中净化分支——此处是
/// 防御性约束，避免异常 URL 把路径片段变成落盘文件名。
///
/// 本地包暂存（`updater::local`）复用本函数：内容与远程资产一致时扩展名随之确定，
/// 按远程名分派解压才不会因用户改过本地文件名而选错解压器。
pub(crate) fn archive_name_from_url(url: &str) -> String {
    let path_only = url.split(['?', '#']).next().unwrap_or("");
    let raw = path_only.rsplit('/').next().unwrap_or("");
    let sanitized: String = raw
        .chars()
        .filter(|c| !matches!(c, '/' | '\\') && !c.is_control())
        .collect();
    let trimmed = sanitized.trim().trim_matches('.');
    if trimmed.is_empty() || trimmed.len() > 128 {
        "campus-auth-update.zip".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 将压缩包解压到 `staging_dir/extracted/`，并校验解压出的 exe 存在
///
/// 解压按资产扩展名分派（`.zip` / `.tar.gz`，见 `utils::io::extract_archive`）：
/// - 路径穿越由 `enclosed_name()` / tar slip 防护过滤，并额外做 `starts_with`
///   防御性检查（防止 zip/tar slip 攻击）；unix 上恢复条目权限位（含 +x）。
///
/// 解压为 CPU+I/O 密集操作，大更新包可能持续数秒~数十秒；通过
/// `tokio::task::spawn_blocking` 在阻塞线程池执行，避免长时间占用 tokio worker 线程。
/// 删除半写入/校验失败的 `.tmp` 下载残留（UPD-2）
///
/// 失败仅 debug 留痕（下次下载入口与 stale 清理仍会兜底），不掩盖原始错误。
fn cleanup_tmp(tmp_path: &Path) {
    if let Err(e) = std::fs::remove_file(tmp_path) {
        tracing::debug!("清理临时下载文件失败 {}: {e}", tmp_path.display());
    }
}

pub(crate) async fn extract_to_staging(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<StagedUpdate, UpdaterError> {
    // 参数为引用，clone 为 owned 后 move 进 spawn_blocking 闭包（闭包需 'static + Send）
    let archive_path = archive_path.to_path_buf();
    let staging_dir = staging_dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_to_staging_blocking(&archive_path, &staging_dir))
        .await
        .map_err(|e| UpdaterError::ExtractFailed(format!("解压任务执行失败: {e}")))?
}

/// 同步解压实现：实际执行压缩包解压与 exe 校验（由 `extract_to_staging` 在阻塞线程池调用）
fn extract_to_staging_blocking(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<StagedUpdate, UpdaterError> {
    let extracted_dir = staging_dir.join("extracted");
    // 清理上一次残留的 extracted/
    if extracted_dir.exists() {
        let _ = std::fs::remove_dir_all(&extracted_dir);
    }
    std::fs::create_dir_all(&extracted_dir).map_err(|e| {
        UpdaterError::StagingDirCreateFailed(io_error_at("创建解压目录", &extracted_dir, e))
    })?;

    // 全量解压（压缩包打开/解析错误统一映射为 ExtractFailed；路径穿越由
    // extract_archive 兜底跳过）
    crate::utils::io::extract_archive(archive_path, &extracted_dir, |_| true)
        .map_err(|e| UpdaterError::ExtractFailed(io_error("解压更新包", e).to_string()))?;

    let extracted_exe = extracted_dir.join(EXE_NAME);
    if !extracted_exe.exists() {
        return Err(UpdaterError::ExtractFailed("解压结果缺少可执行文件".into()));
    }

    Ok(StagedUpdate { extracted_exe })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::response::Redirect;
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
            platform_unavailable: false,
            local_package: None,
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

    /// 携带更新器重定向策略的回环直连客户端（生产 effective_client* 同款策略）
    fn policy_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(crate::updater::updater_redirect_policy())
            .build()
            .unwrap()
    }

    /// 起本地 302 跳转服务：`/a` 请求计数后 302 → `redirect_target`，`/b` 回固定字节。
    /// 返回 (监听地址, `/a` 命中计数器)——跟随与否以计数器观察
    async fn serve_redirect(
        redirect_target: &'static str,
        payload: Vec<u8>,
    ) -> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_route = hits.clone();
        let app = axum::Router::new()
            .route(
                "/a",
                get(move || {
                    let hits = hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Redirect::temporary(redirect_target)
                    }
                }),
            )
            .route(
                "/b",
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
        (addr, hits)
    }

    /// 汇总错误及其 source 链的全部文本（reqwest Display 是否内嵌 source 属上游
    /// 实现细节，走链收集断言更稳）
    fn error_chain_text(err: &UpdaterError) -> String {
        let mut text = err.to_string();
        let mut source = std::error::Error::source(err);
        while let Some(s) = source {
            text.push_str(" | ");
            text.push_str(&s.to_string());
            source = s.source();
        }
        text
    }

    /// 白名单内回环 302（/a → /b）正常跟随：下载成功、SHA256 匹配（策略放行路径）
    #[tokio::test]
    async fn download_follows_allowed_loopback_redirect() {
        let payload = b"redirected-payload-67890".to_vec();
        let (addr, _hits) = serve_redirect("/b", payload.clone()).await;
        let url = format!("http://{addr}/a");
        let info = test_info(&url, &sha256_hex(&payload));
        let tmp = tempfile::tempdir().unwrap();
        let client = policy_client();

        let path = download_and_verify(&client, &info, tmp.path(), None)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
    }

    /// 跳出白名单（回环 302 → http 非回环假域）被客户端策略逐跳前置拦截：
    /// 第二次带 UA 请求从未真实发出（源路由计数恒 1），错误链含策略静态文案
    /// 即证明拦在策略层而非目标域 DNS 失败，且错误文本不回显被拒目标。
    /// 目标用 http（非回环）：白名单对 https 任意主机放行，https 假域测不出拒绝路径
    #[tokio::test]
    async fn download_rejects_redirect_off_whitelist() {
        let payload = b"never-fetched".to_vec();
        let (addr, hits) = serve_redirect("http://example.invalid/x.zip", payload).await;
        let url = format!("http://{addr}/a");
        let info = test_info(&url, &"0".repeat(64));
        let tmp = tempfile::tempdir().unwrap();
        let client = policy_client();

        let err = download_and_verify(&client, &info, tmp.path(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, UpdaterError::DownloadFailed(_)), "{err}");
        // 源路由仅命中一次：302 后的带 UA 请求被策略前置拦下
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // 拦截发生在客户端策略层（含静态文案），不是目标域 DNS 失败碰巧通过
        let text = error_chain_text(&err);
        assert!(text.contains("重定向目标未通过更新源白名单"), "{text}");
        // 错误不回显被拒目标 URL（目标来自不可信 302 Location）
        assert!(!text.contains("example.invalid"), "{text}");
    }

    /// 白名单内连续 302 超过 5 跳被策略中止（上限 5 跳分支）：
    /// 第 6 个跳转目标在前置复核中被拒，链尾 200 永不抵达（若策略缺失，
    /// 走到链尾会因 SHA 不匹配以 ChecksumMismatch 失败，测试不空洞）
    #[tokio::test]
    async fn download_rejects_redirect_hop_limit() {
        use axum::extract::Path;
        use axum::response::IntoResponse;

        let app = axum::Router::new().route(
            "/hop/{n}",
            get(|Path(n): Path<usize>| async move {
                if n < 8 {
                    Redirect::temporary(&format!("/hop/{}", n + 1)).into_response()
                } else {
                    (StatusCode::OK, Body::from(b"never-reached".to_vec())).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/hop/1");
        let info = test_info(&url, &"0".repeat(64));
        let tmp = tempfile::tempdir().unwrap();
        let client = policy_client();

        let err = download_and_verify(&client, &info, tmp.path(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, UpdaterError::DownloadFailed(_)), "{err}");
        assert!(
            error_chain_text(&err).contains("更新源重定向超过 5 跳"),
            "{}",
            error_chain_text(&err)
        );
    }

    /// 文件名净化：正常资产名透传；`..` / 空段 / 超长 / 反斜杠回退或剥离
    #[test]
    fn test_archive_name_from_url_sanitized() {
        assert_eq!(
            archive_name_from_url("https://x.example/a/Campus-Auth-5.0.0-windows-x64.zip?sig=1"),
            "Campus-Auth-5.0.0-windows-x64.zip"
        );
        // 纯点段 / 空段 → 回退固定名
        assert_eq!(
            archive_name_from_url("https://x.example/.."),
            "campus-auth-update.zip"
        );
        assert_eq!(
            archive_name_from_url("https://x.example/"),
            "campus-auth-update.zip"
        );
        // 超长（>128 字符）→ 回退固定名
        let long = format!("https://x.example/{}.zip", "a".repeat(130));
        assert_eq!(archive_name_from_url(&long), "campus-auth-update.zip");
        // 反斜杠剥离（路径分隔符不得进入文件名）
        assert_eq!(
            archive_name_from_url("https://x.example/a\\pkg.zip"),
            "apkg.zip"
        );
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
