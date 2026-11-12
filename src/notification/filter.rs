//! 事件过滤：类型集合 + 免打扰时段（纯函数，可测）
//!
//! 两个判定互相独立：先按事件类型集合过滤（配置里勾了才推），再看免打扰
//! 时段（时段内只记日志不发送）。均无状态、无时钟依赖——时钟由调用方传入
//! 本地时间，跨零点语义集中在这里一处。

use chrono::NaiveTime;

/// 事件类型是否在推送集合内（空集合 = 不推送任何事件）
pub fn event_allowed(enabled_types: &[String], kind: &str) -> bool {
    enabled_types.iter().any(|t| t == kind)
}

/// 免打扰时段判定（支持跨零点）
///
/// - `start < end`：普通窗口，`start <= now < end` 命中；
/// - `start > end`：跨零点窗口（如 22:00–08:00），`now >= start || now < end` 命中；
/// - `start == end`：视为全天免打扰（想表达"无窗口"应关闭 quiet_enabled）。
pub fn in_quiet_window(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start == end {
        return true;
    }
    if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    /// 未启用免打扰时恒放行
    #[test]
    fn test_event_allowed_empty_set_blocks_all() {
        let enabled: Vec<String> = vec!["login_failed".into(), "network_down".into()];
        assert!(event_allowed(&enabled, "login_failed"));
        assert!(event_allowed(&enabled, "network_down"));
        assert!(!event_allowed(&enabled, "network_up"));
        assert!(!event_allowed(&[], "login_failed"), "空集合不推送任何事件");
    }

    /// 普通窗口：22:00-23:59 含头不含尾
    #[test]
    fn test_quiet_window_same_day() {
        let (start, end) = (time(22, 0), time(23, 59));
        assert!(in_quiet_window(time(22, 0), start, end), "窗口起点命中");
        assert!(in_quiet_window(time(23, 0), start, end));
        assert!(!in_quiet_window(time(23, 59), start, end), "窗口终点不含");
        assert!(!in_quiet_window(time(21, 59), start, end));
    }

    /// 跨零点窗口：22:00–08:00，午夜两侧都命中
    #[test]
    fn test_quiet_window_crosses_midnight() {
        let (start, end) = (time(22, 0), time(8, 0));
        assert!(in_quiet_window(time(23, 30), start, end));
        assert!(in_quiet_window(time(0, 0), start, end), "零点整在窗口内");
        assert!(in_quiet_window(time(7, 59), start, end));
        assert!(!in_quiet_window(time(8, 0), start, end), "结束时刻已放行");
        assert!(!in_quiet_window(time(12, 0), start, end));
        assert!(!in_quiet_window(time(21, 59), start, end));
    }

    /// start == end 视为全天免打扰（语义集中定义在此一处）
    #[test]
    fn test_quiet_window_equal_bounds_is_full_day() {
        let t = time(9, 0);
        assert!(in_quiet_window(t, t, t));
        assert!(in_quiet_window(time(3, 0), t, t));
    }

    /// 秒级精度的时间（配置只精确到分，秒位为 0）不影响窗口判定
    #[test]
    fn test_quiet_window_ignores_seconds_component() {
        let (start, end) = (time(22, 0), time(8, 0));
        let now = NaiveTime::from_hms_opt(22, 0, 30).unwrap();
        assert!(in_quiet_window(now, start, end));
    }
}
