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
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
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

/// rename 失败（跨卷等）时的回退安装：copy 到目标同目录临时名再原子 rename（A6/F11）
///
/// 保证目标位置永远不出现半成品文件：
/// - copy 写入目标同目录的临时文件（同目录必然同卷，后续 rename 原子生效）；
/// - copy / rename 任一失败均清理临时文件并返回错误，`dst` 保持原样；
/// - 成功后删除 `src`（移动语义，与 rename 一致；删除失败仅告警不回滚）。
async fn copy_via_temp(src: &Path, dst: &Path) -> std::io::Result<()> {
    let file_name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rename_or_copy".to_string());
    // 临时名带进程 ID 前缀，避免同目录并发安装互踩
    let tmp = dst.with_file_name(format!(".{}.{}.tmp", std::process::id(), file_name));
    if let Err(e) = tokio::fs::copy(src, &tmp).await {
        // copy 失败清理半成品临时文件，dst 未被触碰
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&tmp, dst).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    // 移动语义：成功后移除源文件（失败仅告警，目标已完整生效）
    if let Err(e) = tokio::fs::remove_file(src).await {
        tracing::warn!("rename_or_copy 清理源文件失败 {}: {e}", src.display());
    }
    Ok(())
}

/// 原子化移动文件：优先 rename，失败（跨卷/文件系统不支持）时回退
/// copy 到目标同目录临时名再 rename（A6）。
///
/// 此前的裸 copy 回退会把数据直接写进 `dst`，中途失败会残留部分写入的
/// 半成品文件，被 `exists()` 类就绪检查永久误判（F11）。统一收敛到本函数后，
/// `dst` 只会在最终 rename 时被原子替换。
pub async fn rename_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    match tokio::fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // 跨卷或目标被占用等场景：回退 copy 路径（保持移动语义）
            tracing::warn!("rename 失败，回退 copy 安装: {e}");
            copy_via_temp(src, dst).await
        }
    }
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
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| std::io::Error::other(e.to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// rename_or_copy 快速路径：同目录 rename 原子生效，src 消失、内容完整迁移
    #[tokio::test]
    async fn test_rename_or_copy_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"campus-auth").unwrap();

        rename_or_copy(&src, &dst).await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"campus-auth");
        assert!(!src.exists(), "移动语义：src 应已被 rename 消费");
    }

    /// copy 回退失败路径：src 不存在时返回错误，且目标目录无临时文件残留、
    /// dst 保持原样（不出现半成品，F11）
    #[tokio::test]
    async fn test_copy_via_temp_failure_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("missing.bin");
        let dst = dir.path().join("dst.bin");

        let err = copy_via_temp(&src, &dst).await;
        assert!(err.is_err(), "src 不存在时 copy 必须失败");
        assert!(!dst.exists(), "失败路径不得触碰 dst");
        // 目录内除 missing.bin 外无任何临时残留
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件应被清理: {leftovers:?}");
    }

    /// copy 回退成功路径：内容完整迁移且 src 按移动语义删除
    #[tokio::test]
    async fn test_copy_via_temp_success_moves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"moved-content").unwrap();

        copy_via_temp(&src, &dst).await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"moved-content");
        assert!(!src.exists());
    }
}
