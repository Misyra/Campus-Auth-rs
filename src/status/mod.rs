//! 状态管理：StatusManager + watch channel 推送
//!
//! 持有 `watch::Sender<StatusSnapshot>` 唯一写入端，各服务通过 `merge()` 间接更新。
//! 使用保活接收端保证 channel 永远有接收方（无订阅者时 `send` 不会失败）。

pub mod notifier;
pub mod snapshot;

use snapshot::apply_partial;
use tokio::sync::watch;

/// 默认活跃 Profile ID
pub const DEFAULT_ACTIVE_PROFILE: &str = "default";

/// 状态管理器：状态快照的写入与订阅中心
pub struct StatusManager {
    /// 状态快照写入端（唯一写入方）
    watch_tx: watch::Sender<StatusSnapshot>,
    /// 保活接收端，保证 channel 永远有接收方
    _keepalive_rx: watch::Receiver<StatusSnapshot>,
}

impl StatusManager {
    /// 构造 watch channel + 保活 rx + 默认快照
    pub fn new() -> Self {
        let snapshot = StatusSnapshot::default();
        let (watch_tx, keepalive_rx) = watch::channel(snapshot);
        Self {
            watch_tx,
            _keepalive_rx: keepalive_rx,
        }
    }

    /// 合并部分更新并推送
    ///
    /// 修改与发布必须在同一临界区内完成：历史实现「锁内修改 + 复制 + 释放锁后
    /// send」，线程 A 可在锁释放后、send 前被抢占，线程 B 完整发布新快照后 A
    /// 才 send 旧快照，订阅端观察到状态回退（并发复现已观测 uptime 回退）。
    /// `watch::send_modify` 在 watch 内部锁内原子完成「修改 + 唤醒」，天然消除
    /// 该窗口；同时递增 `snapshot_version` 供前端做单调新鲜度比较（同一秒内
    /// 的多次状态变化也能区分先后）。
    pub fn merge(&self, partial: PartialSnapshot) {
        self.watch_tx.send_modify(|snap| {
            apply_partial(snap, &partial);
            snap.snapshot_version = snap.snapshot_version.saturating_add(1);
        });
    }

    /// 订阅状态快照（WebSocket / 托盘使用）
    pub fn subscribe(&self) -> watch::Receiver<StatusSnapshot> {
        self.watch_tx.subscribe()
    }

    /// 同步读取最新快照
    pub fn borrow(&self) -> StatusSnapshot {
        self.watch_tx.borrow().clone()
    }
}

impl Default for StatusManager {
    fn default() -> Self {
        Self::new()
    }
}

// 重新导出公共类型，供其他模块直接 `use crate::status::Xxx`
pub use notifier::{Notifier, StatusError};
pub use snapshot::{
    EngineState, InstallProgress, LoginSource, LoginStatus, NetworkStatus, PartialSnapshot,
    StatusSnapshot, WorkerStatus,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// merge 每次发布递增 snapshot_version，订阅端可据此区分先后
    #[tokio::test]
    async fn merge_increments_snapshot_version_monotonically() {
        let mgr = StatusManager::new();
        assert_eq!(mgr.borrow().snapshot_version, 0);
        let mut rx = mgr.subscribe();

        mgr.merge(PartialSnapshot::Uptime(1));
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().snapshot_version, 1);
        assert_eq!(rx.borrow().uptime_seconds, 1);

        mgr.merge(PartialSnapshot::ActiveProfile { id: "dorm".into() });
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().snapshot_version, 2);
        assert_eq!(rx.borrow().active_profile, "dorm");
    }

    /// 并发 merge 下订阅端观察到的版本号严格递增（无回退）
    ///
    /// 历史实现「锁内修改、锁外 send」在并发下会发布乱序快照（旧覆盖新）；
    /// send_modify 后版本号单调性是回退的直接检测器。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_merge_never_publishes_stale_version() {
        let mgr = Arc::new(StatusManager::new());
        let mut rx = mgr.subscribe();

        let workers: Vec<_> = (0..4)
            .map(|i| {
                let m = mgr.clone();
                tokio::spawn(async move {
                    for n in 0..500u64 {
                        m.merge(PartialSnapshot::Uptime(i * 1000 + n));
                    }
                })
            })
            .collect();
        for w in workers {
            w.await.unwrap();
        }

        // 排空订阅端：每次观察到的版本号必须严格大于上一次
        let mut last: u64 = 0;
        loop {
            // changed() 在无新值时挂起；用 try_changed 语义（非阻塞轮询）
            let progressed = rx.has_changed().unwrap();
            if !progressed {
                break;
            }
            rx.mark_unchanged();
            let v = rx.borrow().snapshot_version;
            assert!(v > last, "版本号回退: {v} (上一观察值 {last})");
            last = v;
        }
        // 终态：2000 次 merge 全部发布（version 从 0 → 2000）
        assert_eq!(mgr.borrow().snapshot_version, 2000);
        assert_eq!(last, 2000);
    }
}
