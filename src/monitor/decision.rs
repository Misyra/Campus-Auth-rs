//! 探测证据到网络状态与自动恢复建议的纯函数判定

use crate::status::NetworkStatus;

use super::ProbeOutcome;
use super::model::{
    AssessmentConfidence, AssessmentReason, AuthEndpointState, ConnectivityAssessment,
    ProbeEvidence, RecoveryAdvice,
};

/// 综合公网探测证据。
///
/// HTTP 204 与 URL 内容探测属于强公网证据；TCP 仅证明某个端口可连接，
/// 因此只能作为补充。门户证据优先于公网成功，避免选择性劫持被漏判；
/// 单个失败不能覆盖另一类已经给出的强成功证据。
pub fn assess_connectivity(evidence: &ProbeEvidence) -> ConnectivityAssessment {
    let outcomes = [evidence.tcp, evidence.http, evidence.url];
    let active: Vec<ProbeOutcome> = outcomes
        .into_iter()
        .filter(|outcome| *outcome != ProbeOutcome::Disabled)
        .collect();

    if active.is_empty() {
        return assessment(
            NetworkStatus::Unknown,
            AssessmentConfidence::Low,
            AssessmentReason::NoProbesEnabled,
            RecoveryAdvice::NoProbeEvidence,
        );
    }

    let strong_captive = matches!(evidence.http, ProbeOutcome::Captive)
        || matches!(evidence.url, ProbeOutcome::Captive);
    if strong_captive {
        return assessment(
            NetworkStatus::CaptivePortal,
            AssessmentConfidence::High,
            AssessmentReason::CaptiveDetected,
            RecoveryAdvice::AttemptLogin,
        );
    }

    let strong_online =
        matches!(evidence.http, ProbeOutcome::Pass) || matches!(evidence.url, ProbeOutcome::Pass);
    if strong_online {
        return assessment(
            NetworkStatus::Online,
            AssessmentConfidence::High,
            AssessmentReason::InternetVerified,
            RecoveryAdvice::NoAction,
        );
    }

    if active.iter().all(|outcome| *outcome == ProbeOutcome::Fail) {
        return assessment(
            NetworkStatus::Offline,
            AssessmentConfidence::Medium,
            AssessmentReason::AllProbesFailed,
            RecoveryAdvice::WaitForNetwork,
        );
    }

    // Inconclusive（非预期状态码）优先于 tcp==Pass 判定：链路通但证据不足时，
    // 升级路径应走「谨慎单次」而非与普通失败相同的无差别 AttemptLogin
    let reason = if active.contains(&ProbeOutcome::Inconclusive) {
        AssessmentReason::InconclusiveEvidence
    } else if evidence.tcp == ProbeOutcome::Pass {
        AssessmentReason::WeakEvidenceOnly
    } else {
        AssessmentReason::ConflictingEvidence
    };
    assessment(
        NetworkStatus::Unknown,
        AssessmentConfidence::Low,
        reason,
        RecoveryAdvice::WaitForMoreEvidence,
    )
}

/// 把认证入口补充证据应用到基础连通性判断。
///
/// 明确门户证据不会因一次 TCP 预检失败而被抹掉；此时给出“谨慎尝试一次”建议。
/// 基础状态为 Offline/Unknown 时，只有认证入口可达才能升级为门户并建议登录。
pub fn apply_auth_endpoint(
    mut current: ConnectivityAssessment,
    auth_endpoint: AuthEndpointState,
) -> ConnectivityAssessment {
    current.auth_endpoint = auth_endpoint;
    match current.status {
        NetworkStatus::Online => {
            current.recovery_advice = RecoveryAdvice::NoAction;
        }
        NetworkStatus::CaptivePortal => {
            current.recovery_advice = match auth_endpoint {
                AuthEndpointState::Invalid | AuthEndpointState::Missing => {
                    RecoveryAdvice::FixConfiguration
                }
                AuthEndpointState::Unreachable => RecoveryAdvice::AttemptLoginOnce,
                AuthEndpointState::Reachable
                | AuthEndpointState::SkippedRedirectMode
                | AuthEndpointState::NotChecked => RecoveryAdvice::AttemptLogin,
            };
        }
        NetworkStatus::Offline | NetworkStatus::Unknown => match auth_endpoint {
            AuthEndpointState::Reachable => {
                // 证据不足与明确失败分级：探测目标自身 5xx 等异常会让 Unknown
                // 周期性出现，若与普通失败一样无差别 AttemptLogin，会在「探测
                // 目标故障 + 认证服务器在线」时反复触发自动登录；谨慎单次由
                // Engine 按配置版本去重（cautious_attempted_config_version）
                if current.reason == AssessmentReason::InconclusiveEvidence
                    && current.status == NetworkStatus::Unknown
                {
                    current.recovery_advice = RecoveryAdvice::AttemptLoginOnce;
                } else {
                    current.status = NetworkStatus::CaptivePortal;
                    current.confidence = AssessmentConfidence::Medium;
                    current.reason = AssessmentReason::ExternalFailedAuthReachable;
                    current.recovery_advice = RecoveryAdvice::AttemptLogin;
                }
            }
            AuthEndpointState::Invalid | AuthEndpointState::Missing => {
                current.recovery_advice = RecoveryAdvice::FixConfiguration;
            }
            AuthEndpointState::Unreachable => {
                current.recovery_advice = if current.status == NetworkStatus::Offline {
                    RecoveryAdvice::WaitForNetwork
                } else {
                    RecoveryAdvice::WaitForMoreEvidence
                };
            }
            AuthEndpointState::SkippedRedirectMode | AuthEndpointState::NotChecked => {}
        },
    }
    current
}

fn assessment(
    status: NetworkStatus,
    confidence: AssessmentConfidence,
    reason: AssessmentReason,
    recovery_advice: RecoveryAdvice,
) -> ConnectivityAssessment {
    ConnectivityAssessment {
        status,
        confidence,
        reason,
        auth_endpoint: AuthEndpointState::NotChecked,
        recovery_advice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(tcp: ProbeOutcome, http: ProbeOutcome, url: ProbeOutcome) -> ProbeEvidence {
        ProbeEvidence::new(tcp, http, url)
    }

    #[test]
    fn http_pass_beats_supplementary_tcp_failure() {
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Pass,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(result.status, NetworkStatus::Online);
        assert_eq!(result.reason, AssessmentReason::InternetVerified);
    }

    #[test]
    fn captive_evidence_beats_other_results() {
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Pass,
            ProbeOutcome::Fail,
            ProbeOutcome::Captive,
        ));
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
    }

    #[test]
    fn all_fail_is_offline_candidate() {
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        assert_eq!(result.status, NetworkStatus::Offline);
        assert_eq!(result.reason, AssessmentReason::AllProbesFailed);
    }

    #[test]
    fn tcp_pass_without_strong_evidence_is_unknown() {
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Pass,
            ProbeOutcome::Fail,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.reason, AssessmentReason::WeakEvidenceOnly);
    }

    #[test]
    fn all_disabled_is_unknown_without_login_evidence() {
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Disabled,
            ProbeOutcome::Disabled,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.recovery_advice, RecoveryAdvice::NoProbeEvidence);
    }

    #[test]
    fn auth_reachable_upgrades_offline_to_captive() {
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::Reachable);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.reason, AssessmentReason::ExternalFailedAuthReachable);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
    }

    #[test]
    fn explicit_captive_with_unreachable_auth_attempts_once() {
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Disabled,
            ProbeOutcome::Captive,
            ProbeOutcome::Fail,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::Unreachable);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
    }

    #[test]
    fn invalid_auth_requires_configuration_fix() {
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Disabled,
            ProbeOutcome::Captive,
            ProbeOutcome::Disabled,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::Invalid);
        assert_eq!(result.recovery_advice, RecoveryAdvice::FixConfiguration);
    }

    // ============ Inconclusive（MON-2）：非预期状态码 = 证据不足 ============

    #[test]
    fn inconclusive_without_strong_evidence_is_unknown() {
        // 403/5xx 等非预期状态码不再判 Pass：不得落 Online，也不得与全 Fail
        // 混同落 Offline，而是证据不足的 Unknown
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Inconclusive,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.reason, AssessmentReason::InconclusiveEvidence);
        assert_eq!(result.recovery_advice, RecoveryAdvice::WaitForMoreEvidence);
    }

    #[test]
    fn inconclusive_with_tcp_pass_stays_inconclusive() {
        // tcp==Pass + http==Inconclusive：链路通但放行证据不足，
        // 谨慎语义优先于 WeakEvidenceOnly 的无限升级路径
        let result = assess_connectivity(&evidence(
            ProbeOutcome::Pass,
            ProbeOutcome::Inconclusive,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.reason, AssessmentReason::InconclusiveEvidence);
    }

    #[test]
    fn inconclusive_with_auth_reachable_attempts_once() {
        // 证据不足 + auth_url 可达：谨慎单次，不升级 CaptivePortal、
        // 不给无差别 AttemptLogin（防探测目标短暂 5xx 周期性误触发登录）
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Inconclusive,
            ProbeOutcome::Disabled,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::Reachable);
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.reason, AssessmentReason::InconclusiveEvidence);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
    }

    #[test]
    fn plain_offline_with_auth_reachable_keeps_full_attempt() {
        // 回归锚点：明确 Offline（全 Fail）+ auth_url 可达仍走原升级路径，
        // 不受 Inconclusive 分流影响
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::Reachable);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
    }
}
