//! 公共 I/O 工具：原子写入

use std::path::Path;

use serde::Serialize;

/// 原子写入 JSON：先写临时文件再 persist 覆盖，失败自动清理临时文件。
///
/// 使用 `tempfile` crate 保证临时文件与目标文件在同一文件系统，
/// `persist` 底层调用 `rename` 保证原子性。
///
/// 持久化保证与 [`atomic_write_bytes`] 一致：`fsync_full` 刷写文件 +
/// 父目录 fsync 确保目录项落盘（scheduler 与 tasks 的持久化路径在此实现，崩溃后不丢已提交数据。）
pub fn atomic_write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), std::io::Error> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write_bytes(path, json.as_bytes())
}

/// 原子写入原始字节：先写临时文件再 persist 覆盖，失败自动清理临时文件。
///
/// 持久化保证与 [`atomic_write_json`] 对齐：`fsync_full` 刷写文件 +
/// 父目录 fsync 确保目录项落盘（崩溃后不丢已提交数据）。
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp_")
        .suffix(".json")
        .tempfile_in(dir)?;
    {
        use std::io::Write;
        tmp.as_file_mut().write_all(bytes)?;
        fsync_full(tmp.as_file())?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    // 父目录 fsync（确保重命名后的目录项持久化）
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// 刷写文件至物理介质：`sync_all` + macOS `F_FULLFSYNC`。
///
/// macOS 下 `sync_all` 仅等价于 `fsync`，需 `F_FULLFSYNC` 才能保证数据已落盘；
/// 合并 config/service.rs 与 utils/io.rs 两处重复块（C3）。
pub fn fsync_full(file: &std::fs::File) -> std::io::Result<()> {
    file.sync_all()?;
    // macOS：sync_all 仅等价于 fsync，需 F_FULLFSYNC 才能刷写至物理介质
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC, 0) };
        if rc != 0 {
            tracing::warn!("F_FULLFSYNC 失败（rc={rc}），已退化为 sync_all");
        }
    }
    Ok(())
}

/// 通用 zip 解压：按 `accept` 过滤条目后解压到 `dest` 目录（保留相对路径）。
///
/// 统一环境引导（MinGit / uv）与更新包 staging 三处解压模板（C1）：
/// - 使用 `entry.enclosed_name()` 过滤绝对路径与 `..` 穿越（zip slip）；
/// - 额外以 `outpath.starts_with(dest)` 做防御性兜底；
/// - 目录条目创建目录，文件条目写入内容。
///
/// `accept` 接收相对路径（`enclosed_name` 结果），返回 true 才解压该条目，
/// 调用方借此保留各自的过滤逻辑（如仅提取 uv 可执行文件）。
pub fn extract_zip(
    zip_path: &Path,
    dest: &Path,
    mut accept: impl FnMut(&Path) -> bool,
) -> Result<(), std::io::Error> {
    use std::io::{Read, Write};

    const MAX_ZIP_ENTRIES: usize = 8_192;
    const MAX_ZIP_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
    const MAX_ZIP_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(std::io::Error::other("zip 条目数量超过上限"));
    }
    let mut total_bytes = 0u64;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let name = match entry.enclosed_name() {
            Some(n) => n.to_path_buf(),
            None => continue,
        };
        // 防御性兜底：解压结果必须落在目标目录之内
        let outpath = dest.join(&name);
        if !outpath.starts_with(dest) {
            continue;
        }
        if !accept(&name) {
            continue;
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if entry.size() > MAX_ZIP_ENTRY_BYTES
                || total_bytes.saturating_add(entry.size()) > MAX_ZIP_TOTAL_BYTES
            {
                return Err(std::io::Error::other("zip 解压大小超过上限"));
            }
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut output = std::fs::File::create(&outpath)?;
            let copied = std::io::copy(
                &mut entry.by_ref().take(MAX_ZIP_ENTRY_BYTES.saturating_add(1)),
                &mut output,
            )?;
            if copied > MAX_ZIP_ENTRY_BYTES {
                let _ = std::fs::remove_file(&outpath);
                return Err(std::io::Error::other("zip 文件条目超过大小上限"));
            }
            output.flush()?;
            total_bytes = total_bytes.saturating_add(copied);
        }
    }
    Ok(())
}

/// 流式下载错误：网络层（reqwest）或落盘（IO）
#[derive(Debug)]
pub enum DownloadError {
    /// 网络层错误（请求发送 / 非 2xx 状态 / 分块读取）
    Http(reqwest::Error),
    /// 落盘错误（创建 / 写入 / flush）
    Io(std::io::Error),
    /// 响应体超过调用方给定上限。
    TooLarge { limit: u64 },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Http(e) => write!(f, "网络请求失败: {e}"),
            DownloadError::Io(e) => write!(f, "文件写入失败: {e}"),
            DownloadError::TooLarge { limit } => write!(f, "下载内容超过大小上限 {limit} 字节"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// 流式下载文件到指定路径（异步）。
///
/// 统一 git.rs / uv.rs 两处流式下载模板（C2）：GET + error_for_status +
/// 分块写入 + flush。调用方传入已配置的 `reqwest::Client` 并按各自错误类型映射
/// （`DownloadError` 保留 Http / Io 之分，便于 uv 映射到不同 EnvironmentError 变体）。
pub async fn download_streaming(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    max_bytes: u64,
) -> Result<(), DownloadError> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let resp = client
        .get(url)
        .header("User-Agent", "campus-auth")
        .send()
        .await
        .map_err(DownloadError::Http)?;

    let resp = resp.error_for_status().map_err(DownloadError::Http)?;
    if resp.content_length().is_some_and(|size| size > max_bytes) {
        return Err(DownloadError::TooLarge { limit: max_bytes });
    }

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(DownloadError::Io)?;

    let mut stream = resp.bytes_stream();
    let mut downloaded = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(DownloadError::Http)?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > max_bytes {
            let _ = tokio::fs::remove_file(dest).await;
            return Err(DownloadError::TooLarge { limit: max_bytes });
        }
        file.write_all(chunk.as_ref())
            .await
            .map_err(DownloadError::Io)?;
    }
    file.flush().await.map_err(DownloadError::Io)?;
    Ok(())
}
