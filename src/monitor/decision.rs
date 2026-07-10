//! 探测结果 → Online/CaptivePortal/Offline 判定
//!
//! 本文件提供纯函数 `evaluate`（多类探测结果汇总），无 IO，便于单元测试。
//!
//! 暂停时段检测由 Engine 在循环入口统一负责，Monitor 不再自行判断。

use crate::status::NetworkStatus;

use super::{ProbeKind, ProbeOutcome};

/// 综合判定：将多类探测结果汇总为最终网络状态。
///
/// 规则（任一类别禁用则忽略）：
/// - 列表为空（全部禁用）→ `Offline`（保守处理）
/// - 任一 `Fail` → `Offline`（物理断网优先于门户劫持）
/// - 任一 `Captive` → `CaptivePortal`
/// - 全部 `Pass` → `Online`
pub fn evaluate(results: &[(ProbeKind, ProbeOutcome)]) -> NetworkStatus {
    let active: Vec<&ProbeOutcome> = results
        .iter()
        .map(|(_, o)| o)
        .filter(|o| !matches!(o, ProbeOutcome::Disabled))
        .collect();

    if active.is_empty() {
        return NetworkStatus::Offline;
    }
    if active
        .iter()
        .any(|o| matches!(o, ProbeOutcome::Fail))
    {
        return NetworkStatus::Offline;
    }
    if active
        .iter()
        .any(|o| matches!(o, ProbeOutcome::Captive))
    {
        return NetworkStatus::CaptivePortal;
    }
    NetworkStatus::Online
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(kind: ProbeKind, outcome: ProbeOutcome) -> (ProbeKind, ProbeOutcome) {
        (kind, outcome)
    }

    #[test]
    fn evaluate_all_pass() {
        let r = [
            pair(ProbeKind::Tcp, ProbeOutcome::Pass),
            pair(ProbeKind::Http, ProbeOutcome::Pass),
        ];
        assert_eq!(evaluate(&r), NetworkStatus::Online);
    }

    #[test]
    fn evaluate_any_fail_offline() {
        let r = [
            pair(ProbeKind::Tcp, ProbeOutcome::Pass),
            pair(ProbeKind::Http, ProbeOutcome::Fail),
        ];
        assert_eq!(evaluate(&r), NetworkStatus::Offline);
    }

    #[test]
    fn evaluate_captive() {
        let r = [
            pair(ProbeKind::Tcp, ProbeOutcome::Pass),
            pair(ProbeKind::Http, ProbeOutcome::Captive),
        ];
        assert_eq!(evaluate(&r), NetworkStatus::CaptivePortal);
    }

    #[test]
    fn evaluate_disabled_ignored() {
        let r = [
            pair(ProbeKind::Tcp, ProbeOutcome::Disabled),
            pair(ProbeKind::Http, ProbeOutcome::Pass),
        ];
        assert_eq!(evaluate(&r), NetworkStatus::Online);
    }

    #[test]
    fn evaluate_all_disabled_offline() {
        let r = [pair(ProbeKind::Tcp, ProbeOutcome::Disabled)];
        assert_eq!(evaluate(&r), NetworkStatus::Offline);
    }

}
