//! 公共工具模块
//!
//! 提供跨切面关注点：实例互斥文件锁、原子指标计数器、平台特定代码（自启动、网络检测、Shell 检测）。

pub mod io;
pub mod lock;
pub mod metrics;
pub mod paths;
pub mod platform;

// 重新导出公共类型，供其他模块直接 `use crate::utils::Xxx`
pub use io::{atomic_write_bytes, atomic_write_json, extract_zip, fsync_full};
pub use lock::{InstanceInfo, InstanceLock};
pub use metrics::Metrics;

/// 恢复中毒的 `std::sync::Mutex` 锁守卫。
///
/// 持锁期间发生 panic 会标记锁为「中毒」（poisoned）。对于仅保护简单数据的锁，
/// 内部数据仍是一致的，可直接取出继续使用，避免因一次无关 panic 拖垮后续所有读写。
/// 用法：`guard.lock().unwrap_or_else(recover_lock)`。
pub fn recover_lock<T>(poisoned: std::sync::PoisonError<T>) -> T {
    poisoned.into_inner()
}
