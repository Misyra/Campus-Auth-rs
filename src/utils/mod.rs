//! 公共工具模块
//!
//! 提供跨切面关注点：实例互斥文件锁、原子指标计数器、平台特定代码（自启动、网络检测、Shell 检测）。

pub mod io;
pub mod lock;
pub mod metrics;
pub mod platform;

// 重新导出公共类型，供其他模块直接 `use crate::utils::Xxx`
pub use io::atomic_write_json;
pub use lock::{InstanceInfo, InstanceLock};
pub use metrics::Metrics;
