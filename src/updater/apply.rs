//! 更新应用：pending.json 读写 + 启动检测 + 清理
//!
//! 负责把已暂存更新记录为 `update/pending.json`（供助手进程读取），
//! 以及启动时检测并清理残留 pending 标记。助手进程的 spawn 由 `mod.rs` 编排。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::updater::error::UpdaterError;

/// staging 目录相对路径（相对 base_path）
pub(crate) const STAGING_DIR_NAME: &str = "update/staging";
/// pending 标记文件名
pub(crate) const PENDING_FILE_NAME: &str = "update/pending.json";

/// 助手二进制文件名
#[cfg(windows)]
pub(crate) const HELPER_EXE_NAME: &str = "campus-auth-helper.exe";
#[cfg(not(windows))]
pub(crate) const HELPER_EXE_NAME: &str = "campus-auth-helper";

/// 解压出的可执行文件名
#[cfg(windows)]
pub(crate) const EXE_NAME: &str = "campus-auth.exe";
#[cfg(not(windows))]
pub(crate) const EXE_NAME: &str = "campus-auth";

/// 待应用更新记录（`pending.json` 数据模型）
///
/// 助手进程（`src/helper_main.rs`）读取本结构完成 exe 替换与重启。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingUpdate {
    /// 待应用版本号
    pub version: String,
    /// staging 目录绝对路径
    pub staging_dir: String,
    /// 目标 exe 绝对路径（将被替换的当前 exe）
    pub target_exe: String,
    /// 主进程原始启动参数（助手用于重启时恢复）
    pub original_args: Vec<String>,
    /// 创建时间（ISO 8601）
    pub created_at: String,
}

/// 计算 pending.json 的绝对路径
pub(crate) fn pending_path(base_path: &Path) -> std::path::PathBuf {
    base_path.join(PENDING_FILE_NAME)
}

/// 是否存在待应用更新标记
pub(crate) fn has_pending_update(base_path: &Path) -> bool {
    pending_path(base_path).exists()
}

/// 原子写入 pending.json（先写 `.tmp` 再 rename）
pub(crate) fn write_pending(
    pending: &PendingUpdate,
    base_path: &Path,
) -> Result<(), UpdaterError> {
    let path = pending_path(base_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(pending).map_err(|e| {
        UpdaterError::PendingReadFailed(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(UpdaterError::PendingWriteFailed)?;
    std::fs::rename(&tmp, &path).map_err(UpdaterError::PendingWriteFailed)?;
    Ok(())
}

/// 读取并解析 pending.json
pub(crate) fn read_pending(base_path: &Path) -> Result<PendingUpdate, UpdaterError> {
    let path = pending_path(base_path);
    let data = std::fs::read(&path).map_err(UpdaterError::PendingReadFailed)?;
    serde_json::from_slice(&data).map_err(|e| {
        UpdaterError::PendingReadFailed(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    })
}

/// 替换成功后清理 pending 标记与 staging 目录（best-effort）
pub(crate) async fn cleanup_after_apply(base_path: &Path) {
    let path = pending_path(base_path);
    let _ = tokio::fs::remove_file(&path).await;
    let staging = base_path.join(STAGING_DIR_NAME);
    let _ = tokio::fs::remove_dir_all(&staging).await;
}
