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
    /// 暂存包预期 SHA256（hex，空表示未取得校验值）
    ///
    /// G13：helper 替换前据此复核 staging exe 完整性；为空时 helper 侧跳过
    /// 复核（与 check.rs 的"信任 HTTPS"降级一致）。
    #[serde(default)]
    pub sha256: String,
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

/// 原子写入 pending.json（A6：统一走 utils::io::atomic_write_bytes）
///
/// 旧实现手写 tmp+rename 且无 fsync，崩溃窗口内目录项可能未落盘导致
/// pending.json 丢失或读到半成品。`atomic_write_bytes` 内含 fsync（文件 +
/// 父目录），与 scheduler / tasks 的持久化路径同级保证。
pub(crate) fn write_pending(pending: &PendingUpdate, base_path: &Path) -> Result<(), UpdaterError> {
    let path = pending_path(base_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(UpdaterError::PendingWriteFailed)?;
    }
    let json = serde_json::to_vec(pending).map_err(|e| {
        UpdaterError::PendingWriteFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e,
        ))
    })?;
    crate::utils::io::atomic_write_bytes(&path, &json).map_err(UpdaterError::PendingWriteFailed)
}

/// 读取并解析 pending.json
pub(crate) fn read_pending(base_path: &Path) -> Result<PendingUpdate, UpdaterError> {
    let path = pending_path(base_path);
    let data = std::fs::read(&path).map_err(UpdaterError::PendingReadFailed)?;
    serde_json::from_slice(&data).map_err(|e| {
        UpdaterError::PendingReadFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ))
    })
}

/// 替换成功后清理 pending 标记与 staging 目录（best-effort）
pub(crate) async fn cleanup_after_apply(base_path: &Path) {
    let path = pending_path(base_path);
    let _ = tokio::fs::remove_file(&path).await;
    let staging = base_path.join(STAGING_DIR_NAME);
    let _ = tokio::fs::remove_dir_all(&staging).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A6/G13：write_pending → read_pending 往返一致，含 sha256 字段；
    /// 旧版 pending.json（无 sha256 字段）反序列化时默认为空串（降级兼容）
    #[test]
    fn test_write_and_read_pending_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pending = PendingUpdate {
            version: "5.0.1".into(),
            staging_dir: dir.path().join("update/staging").to_string_lossy().into_owned(),
            target_exe: dir.path().join("campus-auth.exe").to_string_lossy().into_owned(),
            original_args: vec!["--port".into(), "8800".into()],
            sha256: "abc123".into(),
            created_at: "2026-08-24T00:00:00Z".into(),
        };
        write_pending(&pending, dir.path()).unwrap();
        let loaded = read_pending(dir.path()).unwrap();
        assert_eq!(loaded.version, "5.0.1");
        assert_eq!(loaded.sha256, "abc123");
        assert_eq!(loaded.original_args, vec!["--port", "8800"]);
        assert!(has_pending_update(dir.path()));

        // 旧格式（无 sha256 字段）兼容
        let legacy = r#"{
            "version": "5.0.0",
            "staging_dir": "/tmp/s",
            "target_exe": "/tmp/t",
            "original_args": [],
            "created_at": "2026-01-01T00:00:00Z"
        }"#;
        std::fs::write(pending_path(dir.path()), legacy).unwrap();
        let old = read_pending(dir.path()).unwrap();
        assert_eq!(old.sha256, "", "旧格式 sha256 应默认为空");
    }

    /// A6：写入后 update/ 目录内无 .tmp 残留（原子写不留半成品）
    #[test]
    fn test_write_pending_no_tmp_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let pending = PendingUpdate {
            version: "5.0.1".into(),
            staging_dir: "s".into(),
            target_exe: "t".into(),
            original_args: vec![],
            sha256: String::new(),
            created_at: "2026-08-24T00:00:00Z".into(),
        };
        write_pending(&pending, dir.path()).unwrap();
        let update_dir = dir.path().join("update");
        let leftovers: Vec<_> = std::fs::read_dir(&update_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件: {leftovers:?}");
    }
}
