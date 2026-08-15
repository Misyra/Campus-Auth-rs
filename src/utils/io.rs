//! 公共 I/O 工具：原子写入

use std::path::Path;

use serde::Serialize;

/// 原子写入 JSON：先写临时文件再 persist 覆盖，失败自动清理临时文件。
///
/// 使用 `tempfile` crate 保证临时文件与目标文件在同一文件系统，
/// `persist` 底层调用 `rename` 保证原子性。
///
/// 持久化保证与 `ConfigService::atomic_write_json` 对齐：`sync_all` 刷写文件 +
/// 父目录 fsync 确保目录项落盘 + macOS 上以 `F_FULLFSYNC` 刷写至物理介质。
/// （scheduler 与 tasks 的持久化路径在此实现，崩溃后不丢已提交数据。）
pub fn atomic_write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), std::io::Error> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp_")
        .suffix(".json")
        .tempfile_in(dir)?;
    {
        use std::io::Write;
        tmp.as_file_mut().write_all(json.as_bytes())?;
        tmp.as_file_mut().sync_all()?;
        // macOS：sync_all 仅等价于 fsync，需 F_FULLFSYNC 才能刷写至物理介质
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = tmp.as_file().as_raw_fd();
            let rc = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC, 0) };
            if rc != 0 {
                tracing::warn!("F_FULLFSYNC 失败（rc={rc}），已退化为 sync_all");
            }
        }
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
