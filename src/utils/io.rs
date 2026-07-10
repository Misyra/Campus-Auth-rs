//! 公共 I/O 工具：原子写入

use std::path::Path;

use serde::Serialize;

/// 原子写入 JSON：先写临时文件再 persist 覆盖，失败自动清理临时文件。
///
/// 使用 `tempfile` crate 保证临时文件与目标文件在同一文件系统，
/// `persist` 底层调用 `rename` 保证原子性。
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
    std::io::Write::write_all(tmp.as_file_mut(), json.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
