//! 通知去重：Notifier + 去重逻辑
//!
//! 登录失败通知去重：按 Profile 去重，首次扫描抑制，Profile 切换/登录成功后清除记录。
//! 系统错误（Worker 崩溃等）始终通知，不走去重逻辑。

use std::collections::HashSet;

use thiserror::Error;

/// 状态模块错误（极少产生，仅系统通知发送失败时）
#[derive(Debug, Error)]
pub enum StatusError {
    /// 系统通知发送失败
    #[error("系统通知发送失败: {0}")]
    NotifySendFailed(String),
}

/// 登录失败通知去重器
///
/// 由 Engine 持有，在调用 `StatusManager::merge(Login{...})` 后决定是否发送系统通知。
pub struct Notifier {
    /// 已通知过登录失败的 Profile ID 集合
    notified_failures: HashSet<String>,
    /// 扫描计数器（首轮 = 0，抑制首次扫描通知）
    scan_count: u8,
}

impl Notifier {
    /// 构造空 Notifier
    pub fn new() -> Self {
        Self {
            notified_failures: HashSet::new(),
            scan_count: 0,
        }
    }

    /// 是否应发送登录失败通知
    ///
    /// 首次扫描抑制；同一 Profile 仅通知一次。
    pub fn should_notify_login_failure(&mut self, profile_id: &str) -> bool {
        // 首次扫描（scan_count == 0）抑制，随后递增
        if self.scan_count == 0 {
            self.scan_count += 1;
            return false;
        }
        if self.notified_failures.contains(profile_id) {
            return false;
        }
        self.notified_failures.insert(profile_id.to_string());
        true
    }

    /// Profile 切换后清除去重记录，重新允许通知
    pub fn on_profile_switch(&mut self) {
        self.notified_failures.clear();
        self.scan_count = 0;
    }

    /// 登录成功后移除该 Profile 的去重记录
    pub fn on_login_success(&mut self, profile_id: &str) {
        self.notified_failures.remove(profile_id);
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}
