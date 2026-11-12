//! 实例互斥：实例锁获取（含 `--force` 强杀重试）与重启场景的锁释放等待

use std::path::Path;

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use crate::utils::lock::InstanceLock;

/// 获取实例锁
///
/// async fn：`--force` 抢锁重试期间用 `tokio::time::sleep` 让出执行权——
/// 本函数在 tokio worker 线程上被 await，改用 `std::thread::sleep` 会阻塞
/// executor 线程最多 3s（5 次退避累加），同 [`wait_for_lock_release`] 口径。
pub(super) async fn acquire_lock(base_path: &Path, force: bool) -> Result<InstanceLock> {
    match InstanceLock::try_acquire(base_path) {
        Ok(lock) => Ok(lock),
        Err(_) if force => {
            warn!("已有实例运行中，--force 终止...");
            if let Some(info) = crate::utils::lock::query_instance(base_path)
                && info.running
            {
                // PID 复用防护：非本程序进程拒绝误杀，仅清理残留后抢锁
                if !crate::utils::lock::is_own_process(info.pid) {
                    warn!(
                        pid = info.pid,
                        "目标 PID 非本程序实例（疑似 PID 复用/残留），跳过 kill，仅清理后抢锁"
                    );
                } else {
                    crate::utils::lock::force_kill(info.pid);
                    for retry in 0..5 {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (retry + 1)))
                            .await;
                        if let Ok(lock) = InstanceLock::try_acquire(base_path) {
                            return Ok(lock);
                        }
                    }
                    anyhow::bail!("强制终止后无法获取锁");
                }
            }
            if let Err(e) = std::fs::remove_file(crate::utils::paths::instance_info_path(base_path))
            {
                tracing::debug!(error = %e, "清理残留的 .instance 锁文件失败");
            }
            InstanceLock::try_acquire(base_path).context("强制终止后仍无法获取锁")
        }
        Err(e) => Err(e).context("无法获取实例锁（已有实例在运行，使用 --force 强制终止）"),
    }
}

/// 重启场景：等待旧进程释放锁
///
/// 超时上限与错误文案共用 [`LOCK_RELEASE_TIMEOUT_SECS`]（UPD-6 提常量）。
pub(super) async fn wait_for_lock_release(base_path: &Path) -> Result<InstanceLock> {
    const LOCK_RELEASE_TIMEOUT_SECS: u64 = 30;
    let timeout = std::time::Duration::from_secs(LOCK_RELEASE_TIMEOUT_SECS);
    let interval = std::time::Duration::from_millis(200);
    let start = std::time::Instant::now();

    info!("重启场景：等待旧进程释放实例锁...");
    loop {
        match InstanceLock::try_acquire(base_path) {
            Ok(lock) => {
                return Ok(lock);
            }
            Err(_) if start.elapsed() < timeout => {
                tokio::time::sleep(interval).await;
            }
            Err(_) => {
                error!("等待旧进程释放锁超时（{LOCK_RELEASE_TIMEOUT_SECS} 秒），本次启动中止");
                anyhow::bail!("等待旧进程释放锁超时（{LOCK_RELEASE_TIMEOUT_SECS} 秒）");
            }
        }
    }
}
