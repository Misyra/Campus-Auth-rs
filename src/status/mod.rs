//! 状态管理：StatusManager + watch channel 推送
//!
//! 持有 `watch::Sender<StatusSnapshot>` 唯一写入端，各服务通过 `merge()` 间接更新。
//! 使用保活接收端保证 channel 永远有接收方（无订阅者时 `send` 不会失败）。

pub mod notifier;
pub mod snapshot;

use std::sync::Mutex;

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
    /// 当前快照的可变副本（仅 merge 内部访问）
    snapshot: Mutex<StatusSnapshot>,
}

impl StatusManager {
    /// 构造 watch channel + 保活 rx + 默认快照
    pub fn new() -> Self {
        let snapshot = StatusSnapshot::default();
        let (watch_tx, keepalive_rx) = watch::channel(snapshot.clone());
        Self {
            watch_tx,
            _keepalive_rx: keepalive_rx,
            snapshot: Mutex::new(snapshot),
        }
    }

    /// 合并部分更新并推送
    pub fn merge(&self, partial: PartialSnapshot) {
        let mut guard = self.snapshot.lock().unwrap();
        apply_partial(&mut guard, &partial);
        let cloned = guard.clone();
        drop(guard);
        let _ = self.watch_tx.send(cloned);
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
pub use snapshot::{
    EngineState, InstallProgress, LoginSource, LoginStatus, NetworkStatus, PartialSnapshot,
    StatusSnapshot, WorkerStatus,
};
pub use notifier::{Notifier, StatusError};
