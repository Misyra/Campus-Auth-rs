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
/// - 任一 `Captive` → `CaptivePortal`（劫持证据优先：https 探测在劫持下必超时 Fail，真断网时则无 Captive，仍归 Offline）
/// - 任一 `Fail` → `Offline`
/// - 全部 `Pass` → `Online`
///
/// 注意：`Offline` 仅为第一阶段结论，上游 `check_once` 会在 Offline 时追加
/// `auth_url` 直连探测，可达则二次纠正为 `CaptivePortal`（真断网保持 Offline）。
pub fn evaluate(results: &[(ProbeKind, ProbeOutcome)]) -> NetworkStatus {
    let active: Vec<&ProbeOutcome> = results
        .iter()
        .map(|(_, o)| o)
        .filter(|o| !matches!(o, ProbeOutcome::Disabled))
        .collect();

    if active.is_empty() {
        return NetworkStatus::Offline;
    }
    if active.iter().any(|o| matches!(o, ProbeOutcome::Captive)) {
        return NetworkStatus::CaptivePortal;
    }
    if active.iter().any(|o| matches!(o, ProbeOutcome::Fail)) {
        return NetworkStatus::Offline;
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
    fn evaluate_captive_beats_fail_hijack() {
        // 劫持型门户：https 探测超时 Fail，但 http 明文探测被劫持 Captive → 必须判门户，否则重定向模式永不触发登录
        let r = [
            pair(ProbeKind::Http, ProbeOutcome::Fail),
            pair(ProbeKind::Url, ProbeOutcome::Captive),
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
