//! uv 下载与镜像：URL 构造、镜像测速排序、文本/流式下载与压缩包提取

use std::path::{Path, PathBuf};

use reqwest::Client;

use crate::environment::{
    EnvironmentError, EnvironmentManager, MirrorChoice, MirrorProbeEntry,
    UV_DOWNLOAD_STALL_TIMEOUT, UV_DOWNLOAD_TIMEOUT, UV_EXE_NAME, UV_FALLBACK_VERSION,
    UV_MIRROR_PROBE_BYTES, UV_MIRROR_PROBE_TIMEOUT, UV_RELEASES_BASE, UV_TARGET,
};
use crate::utils::io_human::io_error_reason;

/// uv 发布资产扩展名：Windows 为 zip；官方 Linux / macOS release 只提供 tar.gz
/// （zip 资产在 unix 目标上 404，是环境引导链的第一处平台断点）
#[cfg(target_os = "windows")]
pub(crate) const UV_ASSET_EXT: &str = "zip";
#[cfg(not(target_os = "windows"))]
pub(crate) const UV_ASSET_EXT: &str = "tar.gz";

/// 构造 uv 下载 URL — 主站
pub(crate) fn uv_archive_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.{UV_ASSET_EXT}")
}

/// 构造 uv SHA256 文件 URL — 主站
pub(crate) fn uv_sha_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.{UV_ASSET_EXT}.sha256")
}

/// 生成所有镜像的下载 URL 列表（主站 + 代理镜像）
pub(super) fn uv_archive_urls(version: &str) -> Vec<String> {
    let base = uv_archive_url(version);
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
pub(super) fn uv_sha_urls(version: &str) -> Vec<String> {
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

/// 并发探测镜像的实测下载吞吐，按吞吐量降序返回 URL 索引。
///
/// 用户在初始化弹窗显式选择下载源（[`MirrorChoice`]）时跳过测速：选中源最前、
/// 其余保持原序兜底；`Direct` 保持原序（直连本就在首位）；仅 `Auto` 走实测。
///
/// 自动路径：每个候选并发发起 `Range: bytes=0-{预算-1}` 的 GET，在总时限
/// （[`UV_MIRROR_PROBE_TIMEOUT`]）内最多读取 [`UV_MIRROR_PROBE_BYTES`] 字节，
/// 以"字节/秒"实测吞吐排序——直连 GitHub 常见"建连快但跑不动"，仅测建连
/// 延迟的旧口径识别不了这类源，会先撞 300s 总超时再换镜像。
///
/// 探测失败（超时且零字节 / 非 2xx / 连接错误）的候选不淘汰：追加到序列末尾
/// 保持原序兜底——探测路径不可用不代表下载路径不可用（部分代理对 Range 请求
/// 的处理与完整 GET 不一致，如 ghfast.top 曾对 Range 返回 403 而正常下载可用）。
pub(super) async fn rank_mirror_urls(mgr: &EnvironmentManager, urls: &[String]) -> Vec<usize> {
    if urls.len() <= 1 {
        return (0..urls.len()).collect();
    }
    // 显式选源优先于测速：偏好损坏/缺失回退 Auto，不会阻断下载
    match crate::environment::mirror::read_mirror_choice(mgr) {
        MirrorChoice::Mirror(prefix) => {
            tracing::debug!("镜像测速跳过：用户已固定下载源 {prefix}");
            let mut order: Vec<usize> = urls
                .iter()
                .enumerate()
                .filter(|(_, url)| url.starts_with(prefix.as_str()))
                .map(|(idx, _)| idx)
                .collect();
            let rest: Vec<usize> = (0..urls.len()).filter(|idx| !order.contains(idx)).collect();
            order.extend(rest);
            return order;
        }
        MirrorChoice::Direct => return (0..urls.len()).collect(),
        MirrorChoice::Auto => {}
    }
    let client = mgr.http_client().clone();
    let mut handles = Vec::with_capacity(urls.len());
    for url in urls {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            probe_mirror_throughput(&client, &url).await
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    // handles 与 urls 同序生成：enumerate 提供真实 idx，探测任务 panic
    //（JoinError）也能按「该候选失败」兜底保留——旧实现塞 usize::MAX 哨兵，
    // 会穿透排序进入返回值，调用方 urls[usize::MAX] 直接越界 panic
    for (idx, h) in handles.into_iter().enumerate() {
        match h.await {
            Ok(outcome) => results.push((idx, outcome)),
            Err(_) => results.push((idx, None)),
        }
    }
    let ok = results.iter().filter(|(_, o)| o.is_some()).count();
    tracing::debug!(
        "镜像测速：{ok} 个可用、{} 个零字节/失败，按实测吞吐排序（最快优先）",
        results.len() - ok
    );
    order_from_probe_results(results, urls.len())
}

/// 探测结果的纯排序：成功项按吞吐（字节/秒）降序，失败项按原序追加末尾兜底。
/// `url_count` 之外的越界 idx 一律丢弃（uv 哨兵修复的防御层）——返回值保证
/// 全部是可直接索引 urls 的合法下标，三个调用方（GitHub API 版本探测 /
/// SHA 文本下载 / uv 归档下载）据此永不越界 panic。
/// 抽成纯函数便于无网络单测。
fn order_from_probe_results(
    results: Vec<(usize, Option<(usize, std::time::Duration)>)>,
    url_count: usize,
) -> Vec<usize> {
    let mut ok: Vec<(usize, f64)> = Vec::new();
    let mut failed: Vec<usize> = Vec::new();
    for (idx, outcome) in results {
        // 哨兵防御：越界下标（历史 usize::MAX 哨兵形态）不得进入返回值
        if idx >= url_count {
            continue;
        }
        match outcome {
            Some((bytes, elapsed)) => {
                // 微小文件（SHA256 等）瞬时读完，耗时近零时钳到 1ms 防除零
                let secs = elapsed.as_secs_f64().max(0.001);
                ok.push((idx, bytes as f64 / secs));
            }
            None => failed.push(idx),
        }
    }
    ok.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ok.into_iter().map(|(idx, _)| idx).chain(failed).collect()
}

/// 单候选探测：Range GET 拉取至多 [`UV_MIRROR_PROBE_BYTES`] 字节，
/// 返回 (已读字节, 累计耗时)；零字节失败返回 None。
///
/// 预算到达 / 流提前结束（小文件）/ 读取错误 / 时限耗尽都以已读字节参与排序——
/// 半途拿到数据的候选仍是有用样本，只有零字节才视为失败。
async fn probe_mirror_throughput(
    client: &Client,
    url: &str,
) -> Option<(usize, std::time::Duration)> {
    let started = std::time::Instant::now();
    let deadline = started + UV_MIRROR_PROBE_TIMEOUT;
    let mut resp = client
        .get(url)
        .header("User-Agent", "campus-auth")
        .header("Range", format!("bytes=0-{}", UV_MIRROR_PROBE_BYTES - 1))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let mut bytes = 0usize;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                bytes += chunk.len();
                if bytes >= UV_MIRROR_PROBE_BYTES {
                    break;
                }
            }
            // EOF：SHA256 等小文件在预算内完整读完
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
    let elapsed = started.elapsed();
    (bytes > 0).then_some((bytes, elapsed))
}

/// 直连 + 各镜像前缀构造的 GitHub Release 资产探测 URL 列表（直连在前）。
///
/// 测速 URL 复用 uv 的发布资产（兜底版本真实存在）：python-build-standalone
/// 与 uv 同 host 同 CDN，对 GitHub 大文件链路的路由结论互通。
pub(crate) fn github_asset_probe_urls() -> Vec<String> {
    let base = uv_archive_url(UV_FALLBACK_VERSION);
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_MIRRORS.len());
    urls.push(base.clone());
    for mirror in crate::environment::GITHUB_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 并发测速直连与各镜像（初始化弹窗手动选源）：逐源返回
/// [`MirrorProbeEntry`]，与 [`rank_mirror_urls`] 共用同一探测实现与字节预算。
pub(crate) async fn probe_mirror_entries(mgr: &EnvironmentManager) -> Vec<MirrorProbeEntry> {
    let urls = github_asset_probe_urls();
    let client = mgr.http_client().clone();
    let mut handles = Vec::with_capacity(urls.len());
    for url in &urls {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            probe_mirror_throughput(&client, &url).await
        }));
    }
    let mut entries = Vec::with_capacity(urls.len());
    for (url, handle) in urls.iter().zip(handles) {
        // 探测任务 panic 按「不可用」呈现（与 rank 的兜底口径一致）
        let outcome = handle.await.ok().flatten();
        let (kind, prefix) = match crate::environment::mirror::mirror_prefix_of(url) {
            Some(m) => ("mirror", m),
            None => ("direct", ""),
        };
        entries.push(match outcome {
            Some((bytes, elapsed)) => MirrorProbeEntry {
                kind,
                prefix: prefix.to_string(),
                label: super::host_of_url(url).to_string(),
                ok: true,
                bytes,
                elapsed_ms: elapsed.as_millis() as u64,
                throughput: (bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64,
            },
            None => MirrorProbeEntry {
                kind,
                prefix: prefix.to_string(),
                label: super::host_of_url(url).to_string(),
                ok: false,
                bytes: 0,
                elapsed_ms: 0,
                throughput: 0,
            },
        });
    }
    entries
}

/// 通过 GitHub API 获取 uv 最新版本号（先并发测速选最快镜像，再按序尝试）
pub(super) async fn fetch_latest_uv_version(
    mgr: &EnvironmentManager,
) -> Result<String, EnvironmentError> {
    let urls = github_api_urls();
    let order = rank_mirror_urls(mgr, &urls).await;
    let mut last_err = String::new();

    for idx in order {
        let url = &urls[idx];
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
            // ENV-7：tag 未经清洗即拼进下载 URL（uv_archive_url 等 format!），
            // url crate 会规范化 `..`，同 host 路径操纵面收敛于此——semver 白名单
            // 校验失败视为镜像响应异常，继续尝试下一镜像
            if semver::Version::parse(version).is_err() {
                tracing::warn!("镜像返回非法版本号 {tag:?}，跳过该镜像");
                last_err = format!("非法 tag_name: {tag}");
                continue;
            }
            return Ok(version.to_string());
        }
        last_err = "tag_name 字段缺失".to_string();
    }

    tracing::warn!("所有 GitHub API 镜像均失败({last_err})，回退到兜底版本 {UV_FALLBACK_VERSION}");
    Ok(UV_FALLBACK_VERSION.to_string())
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

    // 非 2xx（404/403/502 等）直接判失败，避免把错误页当作 sha 内容吃进校验
    let mut resp = resp
        .error_for_status()
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })?;

    // ENV-5：SHA256 校验文件实际只有 64 hex + 换行，限制响应体大小防止
    // 异常/被劫持镜像的超大 body 全量读入内存（二进制下载走 256MB 上限的
    // 流式路径，文本此前无任何上限）
    const SHA_TEXT_MAX_BYTES: usize = 1024 * 1024;
    if let Some(len) = resp.content_length()
        && len as usize > SHA_TEXT_MAX_BYTES
    {
        return Err(EnvironmentError::UvDownloadIoFailed {
            retries: 0,
            message: format!("SHA256 文件大小 {len} 字节超过上限 {SHA_TEXT_MAX_BYTES}"),
        });
    }
    let mut body: Vec<u8> = Vec::with_capacity(256);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })?
    {
        if body.len() + chunk.len() > SHA_TEXT_MAX_BYTES {
            return Err(EnvironmentError::UvDownloadIoFailed {
                retries: 0,
                message: format!("SHA256 文件超过大小上限 {SHA_TEXT_MAX_BYTES} 字节"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| EnvironmentError::UvDownloadIoFailed {
        retries: 0,
        message: "SHA256 校验文件不是有效的 UTF-8 文本".into(),
    })
}

/// 尝试从多个镜像下载文本，优先使用最快的镜像。
///
/// 先并发探测全部 URL 的首字节延迟并按延迟排序，再按测速结果依次尝试下载；
/// 测速超时/失败的镜像不会被优先，但仍会在排序末尾保留作为兜底。
pub(super) async fn download_text_with_mirrors(
    mgr: &EnvironmentManager,
    urls: &[String],
) -> Result<String, EnvironmentError> {
    let order = rank_mirror_urls(mgr, urls).await;
    let mut last_err = String::new();
    tracing::debug!("尝试 {} 个镜像下载（已按测速排序）", urls.len());
    for (rank, idx) in order.iter().enumerate() {
        let url = &urls[*idx];
        tracing::debug!("镜像 {}/{}: {}", rank + 1, urls.len(), url);
        match download_text(mgr, url).await {
            Ok(text) => {
                tracing::debug!("镜像 {} 下载成功", url);
                return Ok(text);
            }
            Err(e) => {
                tracing::debug!("镜像 {} 失败: {}", url, e);
                last_err = e.to_string();
            }
        }
    }
    Err(EnvironmentError::GitHubApiError(format!(
        "所有镜像均失败: {last_err}"
    )))
}

/// 流式下载文件到指定路径（带停滞检测：慢网只要出数据就不判失败，仅彻底停滞才超时）
///
/// `cancel`（ENV-3）透传至 chunk 循环：传输中触发即清理临时文件返回取消，
/// 不再等 300s 总超时。
pub(super) async fn download_file_streaming(
    mgr: &EnvironmentManager,
    url: &str,
    dest: &Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), EnvironmentError> {
    crate::utils::io::download_streaming_with_stall(
        mgr.http_client(),
        url,
        dest,
        256 * 1024 * 1024,
        Some(UV_DOWNLOAD_STALL_TIMEOUT),
        Some(cancel),
    )
    .await
    .map_err(|e| match e {
        crate::utils::io::DownloadError::Cancelled => EnvironmentError::Cancelled,
        crate::utils::io::DownloadError::Http(e) => EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        },
        crate::utils::io::DownloadError::Io(e) => {
            EnvironmentError::UvExtractFailed(io_error_reason(e))
        }
        crate::utils::io::DownloadError::Stalled {
            idle_secs,
            received_bytes,
        } => EnvironmentError::UvDownloadIoFailed {
            retries: 0,
            message: format!("下载停滞（{idle_secs}s 内无数据，已接收 {received_bytes} 字节）"),
        },
        crate::utils::io::DownloadError::TooLarge { limit } => {
            EnvironmentError::UvDownloadIoFailed {
                retries: 0,
                message: format!("下载内容超过大小上限 {limit} 字节"),
            }
        }
    })
}

/// 从压缩包（zip / tar.gz，按扩展名分派）中提取 uv 可执行文件到目标路径
pub(super) fn extract_uv_from_archive(archive_path: &Path, dest: &Path) -> std::io::Result<()> {
    // uv 二进制位于压缩包内 `uv-{target}/uv.exe`（或 `uv`）。先按文件名过滤解压到
    // 临时目录，再把找到的可执行文件复制到目标路径（复用 extract_archive 模板）。
    let tmp_dir = tempfile::tempdir()?;
    let mut found: Option<PathBuf> = None;
    crate::utils::io::extract_archive(archive_path, tmp_dir.path(), |name| {
        // 同时接受 uv / uv.exe：官方 unix 资产为 uv，Windows zip 为 uv.exe；
        // 宽松匹配让解压逻辑与当前平台的 UV_EXE_NAME 解耦
        if name
            .file_name()
            .is_some_and(|f| f == UV_EXE_NAME || f == "uv" || f == "uv.exe")
        {
            found = Some(tmp_dir.path().join(name));
            true
        } else {
            false
        }
    })?;
    let src = found.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "压缩包中未找到 uv 可执行文件")
    })?;
    std::fs::copy(&src, dest)?;
    // unix：显式补 0755 兜底——uv 不可执行 = 环境引导整体失败，不依赖
    // 解压/复制链路上任何一环的权限位传递是否完整
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL 构造：压缩包与 sha256 均指向主站对应文件（资产扩展名按平台：
    /// Windows zip / unix tar.gz）
    #[test]
    fn test_uv_urls_format() {
        let expected = format!("/0.5.0/uv-{UV_TARGET}.{UV_ASSET_EXT}");
        let archive = uv_archive_url("0.5.0");
        assert!(archive.ends_with(&expected), "archive: {archive}");
        let sha = uv_sha_url("0.5.0");
        assert!(
            sha.ends_with(&format!(".{UV_ASSET_EXT}.sha256")),
            "sha: {sha}"
        );
    }

    /// 镜像列表：直连在前，代理镜像在后，首项为直连
    #[test]
    fn test_uv_mirror_urls() {
        let archives = uv_archive_urls("0.5.0");
        assert_eq!(archives[0], uv_archive_url("0.5.0"));
        assert!(archives.len() > 1, "应包含代理镜像");
        let shas = uv_sha_urls("0.5.0");
        assert_eq!(shas[0], uv_sha_url("0.5.0"));
        assert_eq!(shas.len(), archives.len());
    }

    /// 探测排序：成功项按吞吐（字节/秒）降序——同耗时高字节与同字节低耗时都应靠前
    #[test]
    fn test_order_from_probe_results_by_throughput() {
        let dur = |ms: u64| std::time::Duration::from_millis(ms);
        let results = vec![
            (0, Some((64, dur(100)))),       // 640 B/s：SHA 小文件秒回
            (1, Some((262_144, dur(500)))),  // ≈524 KB/s
            (2, Some((100_000, dur(400)))),  // 250 KB/s
            (3, Some((262_144, dur(1000)))), // 262 KB/s
        ];
        assert_eq!(order_from_probe_results(results, 4), vec![1, 3, 2, 0]);
    }

    /// 探测排序：失败候选不淘汰，按原序追加到成功项之后兜底
    #[test]
    fn test_order_from_probe_results_failed_appended() {
        let dur = std::time::Duration::from_millis(100);
        let results = vec![
            (0, None),
            (1, Some((262_144, dur))),
            (2, None),
            (3, Some((1024, dur))),
        ];
        assert_eq!(order_from_probe_results(results, 4), vec![1, 3, 0, 2]);
        // 全部失败：回退原序
        let results = vec![(0, None), (1, None), (2, None)];
        assert_eq!(order_from_probe_results(results, 3), vec![0, 1, 2]);
    }

    /// uv 哨兵修复：越界 idx（usize::MAX 哨兵的历史形态 / 任何越界值）在纯排序
    /// 处丢弃，返回值全部是可直接索引 urls 的合法下标——三个调用方据此不会
    /// 在探测任务 panic 后越界 panic
    #[test]
    fn test_order_from_probe_results_drops_out_of_range_idx() {
        let dur = std::time::Duration::from_millis(100);
        // JoinError 哨兵历史形态 + 越界但「成功」的样本：均不得穿透进返回值
        let results = vec![
            (usize::MAX, None),
            (0, Some((64, dur))),
            (7, Some((262_144, dur))),
            (1, None),
        ];
        assert_eq!(
            order_from_probe_results(results, 2),
            vec![0, 1],
            "越界样本丢弃、合法样本按吞吐与失败兜底原序保留"
        );
    }

    /// zip 提取：从含 uv.exe 的 zip 中正确提取（Windows 资产格式）
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
        extract_uv_from_archive(&zip_path, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ fake-exe");
    }

    /// tar.gz 提取：unix 官方资产格式（uv-{target}/uv）跨平台可解，且
    /// 解出的可执行文件在 unix 上具备 0755 权限
    #[test]
    fn test_extract_uv_from_tar_gz() {
        use flate2::write::GzEncoder;
        use tar::{Builder, Header};

        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("uv.tar.gz");
        let gz = GzEncoder::new(
            std::fs::File::create(&tar_path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = Builder::new(gz);
        let uv_content: &[u8] = b"ELF fake-uv";
        let mut hdr = Header::new_gnu();
        // tar header 的 size 必须与数据长度一致（tar 按大小寻址）
        hdr.set_size(uv_content.len() as u64);
        hdr.set_mode(0o755);
        hdr.set_cksum();
        builder
            .append_data(&mut hdr, "uv-x86_64-unknown-linux-gnu/uv", uv_content)
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let dest = dir.path().join("uv");
        extract_uv_from_archive(&tar_path, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ELF fake-uv");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "解压出的 uv 必须可执行");
        }
    }
}
