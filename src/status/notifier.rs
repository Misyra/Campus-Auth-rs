//! 通知去重：Notifier + 去重逻辑
//!
//! 登录失败通知去重：按 Profile 去重，Profile 切换/登录成功后清除记录。
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
}

impl Notifier {
    /// 构造空 Notifier
    pub fn new() -> Self {
        Self {
            notified_failures: HashSet::new(),
        }
    }

    /// 是否应发送登录失败通知
    ///
    /// 同一 Profile 在连续失败期间仅通知一次。
    pub fn should_notify_login_failure(&mut self, profile_id: &str) -> bool {
        if self.notified_failures.contains(profile_id) {
            return false;
        }
        self.notified_failures.insert(profile_id.to_string());
        true
    }

    /// Profile 切换后清除去重记录，重新允许通知
    ///
    /// 切换方案后的失败是新的当前事实，下一次失败应重新通知。
    pub fn on_profile_switch(&mut self) {
        self.notified_failures.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 首次真实失败立即通知；连续失败去重；切换方案后重新允许通知
    #[test]
    fn test_profile_switch_keeps_first_scan_suppression_boundary() {
        let mut n = Notifier::new();
        assert!(n.should_notify_login_failure("p1"));
        assert!(!n.should_notify_login_failure("p1"));

        // 切换方案：清去重集，切换后的失败是新的当前事实
        n.on_profile_switch();
        assert!(
            n.should_notify_login_failure("p1"),
            "切换方案后首轮失败应通知"
        );
        assert!(
            n.should_notify_login_failure("p2"),
            "同轮内另一失败方案也应通知（去重集已清空）"
        );
    }
}
