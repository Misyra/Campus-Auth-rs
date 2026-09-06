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
    ///
    /// 仅清去重集，不动 `scan_count`：首次扫描抑制只针对进程启动时的历史失败，
    /// 切换方案后的失败是当前事实，下一次扫描即应通知（否则首个失败提醒被吞）。
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

    /// 启动首轮扫描抑制历史失败；切换方案后的首轮失败应立即通知（F12）
    #[test]
    fn test_profile_switch_keeps_first_scan_suppression_boundary() {
        let mut n = Notifier::new();
        // 启动首轮：抑制（避免对历史失败误报）
        assert!(!n.should_notify_login_failure("p1"));
        // 第二轮：同 Profile 仅通知一次
        assert!(n.should_notify_login_failure("p1"));
        assert!(!n.should_notify_login_failure("p1"));

        // 切换方案：清去重集但不重置首轮抑制——切换后的失败是当前事实
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
