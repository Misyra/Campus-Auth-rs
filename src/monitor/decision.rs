//! 探测证据到网络状态与自动恢复建议的纯函数判定

use crate::status::NetworkStatus;

use super::ProbeOutcome;
use super::model::{
    AssessmentConfidence, AssessmentReason, AuthEndpointState, ConnectivityAssessment,
    LocalLinkState, ProbeEvidence, RecoveryAdvice,
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
    } else {
        // MON-1：到达此处必为 tcp==Pass——Captive/Pass(http/url) 已早退、
        // 全 Fail 已早退，无 Inconclusive 时 active 的非 Fail 成员只能是 tcp
        // 的 Pass。原 ConflictingEvidence 分支生产不可达已删，变体保留供
        // 序列化兼容；debug_assert 锁定不变量，未来扩展 TCP 探测产出时先改这里
        debug_assert_eq!(evidence.tcp, ProbeOutcome::Pass);
        AssessmentReason::WeakEvidenceOnly
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

/// 宽松触发是否可能生效（即：采集本地链路证据是否值得）。
///
/// 由调用方在「严格登录模式已关闭」的前提下使用（见 `MonitorConfig::strict_login_mode`）：
/// 本函数只判断判定本身是否还有升级空间，不含开关状态。
///
/// 与 [`apply_lenient_trigger`] 共用同一条件，避免「采集判据」与「升级判据」
/// 两处漂移——若分开写，改一处忘另一处会导致白采集或证据缺失。
///
/// `true` = 当前判定尚未确认在线、且严格口径**尚未给出登录建议**、也不属于
/// 宽松模式不该接管的两种终态（配置错误、无有效探测）。
pub fn lenient_trigger_candidate(current: &ConnectivityAssessment) -> bool {
    current.status != NetworkStatus::Online
        && !matches!(
            current.recovery_advice,
            RecoveryAdvice::FixConfiguration
                | RecoveryAdvice::NoProbeEvidence
                // 已是登录建议：宽松触发无事可做。若仍改写，会把严格口径更可信的
                // `High`/`CaptiveDetected` 降级为 `Low`/`LinkUpLoginAssumed`，
                // 抹掉「确实检测到劫持」与「仅按链路推断」的区别
                | RecoveryAdvice::AttemptLogin
                // 有意的「谨慎单次」节流（Engine 按配置版本去重）：升级为
                // `AttemptLogin` 会绕过该去重，退化为按失败节奏反复尝试
                | RecoveryAdvice::AttemptLoginOnce
        )
}

/// 应用「宽松登录触发」：本地网卡已连接且未确认在线时也建议自动登录。
///
/// 仅在用户**关闭**「严格登录模式」（`monitor.strict_login_mode = false`）时由
/// `MonitorService::check_once` 调用；默认（严格）模式下本函数不参与判定。
///
/// 面向「学校门户 → 校园网认证」两级认证场景：学校门户决定账号，进入校园网后
/// 再选运营商。这类网络的网关可能直接放行 204 探测域名（判 `Online`）、或返回
/// 探测目标自身的非预期状态码（判 `Unknown`），严格口径下都不会给出门户证据，
/// 于是 `WaitForNetwork`/`NoAction` 让自动登录永不触发。
///
/// 判定只依赖「本地链路可用 + 未确认在线」两个条件（前置条件见
/// [`lenient_trigger_candidate`]）：
/// - `status == Online` 时一律不动——网关已放行探测，说明确实能上网，不该被
///   宽松策略打扰（这也是本策略的唯一边界）；
/// - `local_link != Available` 时不动——网卡没连上（无 IP/链路本地）时登录
///   没有意义，等待链路本身就是正确动作；
/// - `FixConfiguration` 保持不动——配置错误是用户须先解决的问题，拉浏览器
///   只会产生误导性失败；
/// - `NoProbeEvidence` 保持不动——一个探测都没启用属于测量缺失而非证据不足，
///   此时升级会与「禁止自动恢复」的既定告警语义冲突，且每轮都会尝试登录；
/// - `AttemptLogin` / `AttemptLoginOnce` 保持不动——严格口径已经给出登录建议，
///   本函数是**兜底**（严格口径什么都没给出时才升级），此时改写只会有损：
///   前者会把 `High`/`CaptiveDetected` 降级为推断级 `Low`/`LinkUpLoginAssumed`，
///   后者会绕过 Engine 的「同一配置版本仅尝试一次」去重；
/// - 其余（Offline/Unknown/CaptivePortal）统一升级为 `CaptivePortal` +
///   `AttemptLogin`，并标注置信度 `Low` 与原因
///   [`AssessmentReason::LinkUpLoginAssumed`]，让用户在界面上能区分「明确检测到
///   劫持」与「按链路连接推断需要登录」。
///
/// 认证地址 TCP 预检结果不参与判定：预检失败（网关对新连接限速/丢首包等）不构成
/// 阻止尝试的理由，由用户显式选择承担该风险。
pub fn apply_lenient_trigger(
    mut current: ConnectivityAssessment,
    local_link: LocalLinkState,
) -> ConnectivityAssessment {
    if !lenient_trigger_candidate(&current) || local_link != LocalLinkState::Available {
        return current;
    }
    current.status = NetworkStatus::CaptivePortal;
    current.confidence = AssessmentConfidence::Low;
    current.reason = AssessmentReason::LinkUpLoginAssumed;
    current.recovery_advice = RecoveryAdvice::AttemptLogin;
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

    // ============ 宽松登录触发（两级认证场景） ============

    #[test]
    fn lenient_upgrades_offline_when_link_available() {
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        assert_eq!(base.recovery_advice, RecoveryAdvice::WaitForNetwork);
        let result = apply_lenient_trigger(base, LocalLinkState::Available);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.reason, AssessmentReason::LinkUpLoginAssumed);
        assert_eq!(result.confidence, AssessmentConfidence::Low);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
    }

    #[test]
    fn lenient_upgrades_unknown_and_inconclusive() {
        // 未知证据（目标服务异常）与 Inconclusive 同样升级：
        // 宽松模式下用户的意图是「只要网卡连着就试」，两种都不该漏
        let unknown = assess_connectivity(&evidence(
            ProbeOutcome::Pass,
            ProbeOutcome::Fail,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(unknown.status, NetworkStatus::Unknown);
        let result = apply_lenient_trigger(unknown, LocalLinkState::Available);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
        assert_eq!(result.reason, AssessmentReason::LinkUpLoginAssumed);

        let inconclusive = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Inconclusive,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(inconclusive.status, NetworkStatus::Unknown);
        let result = apply_lenient_trigger(inconclusive, LocalLinkState::Available);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
        assert_eq!(result.reason, AssessmentReason::LinkUpLoginAssumed);
    }

    #[test]
    fn lenient_preserves_no_probe_evidence() {
        // 一个探测都没启用属「测量缺失」，保持「禁止自动恢复」的既定语义，
        // 否则每轮都会拉起浏览器
        let disabled = assess_connectivity(&ProbeEvidence::default());
        assert_eq!(disabled.recovery_advice, RecoveryAdvice::NoProbeEvidence);
        let result = apply_lenient_trigger(disabled, LocalLinkState::Available);
        assert_eq!(result.recovery_advice, RecoveryAdvice::NoProbeEvidence);
        assert_eq!(result.status, NetworkStatus::Unknown);
    }

    #[test]
    fn lenient_never_touches_confirmed_online() {
        // 唯一边界：探测已确认在线时不打扰（网关放行 ≠ 需要登录）
        let online = assess_connectivity(&evidence(
            ProbeOutcome::Disabled,
            ProbeOutcome::Pass,
            ProbeOutcome::Disabled,
        ));
        assert_eq!(online.status, NetworkStatus::Online);
        let result = apply_lenient_trigger(online, LocalLinkState::Available);
        assert_eq!(result.status, NetworkStatus::Online);
        assert_eq!(result.reason, AssessmentReason::InternetVerified);
        assert_eq!(result.recovery_advice, RecoveryAdvice::NoAction);
    }

    #[test]
    fn lenient_requires_available_link() {
        // 网卡未连上（未检查/无接口/检查失败）时等待链路才是正确动作
        let offline = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        for state in [
            LocalLinkState::NotChecked,
            LocalLinkState::Unavailable,
            LocalLinkState::ProbeFailed,
        ] {
            let result = apply_lenient_trigger(offline.clone(), state);
            assert_eq!(
                result.recovery_advice,
                RecoveryAdvice::WaitForNetwork,
                "{state:?} 不应触发宽松登录"
            );
            assert_eq!(result.status, NetworkStatus::Offline);
        }
    }

    #[test]
    fn lenient_preserves_configuration_error() {
        // 配置错误须用户先修正：拉浏览器只会产生误导性失败
        let base = apply_auth_endpoint(
            assess_connectivity(&evidence(
                ProbeOutcome::Disabled,
                ProbeOutcome::Captive,
                ProbeOutcome::Disabled,
            )),
            AuthEndpointState::Invalid,
        );
        assert_eq!(base.recovery_advice, RecoveryAdvice::FixConfiguration);
        let result = apply_lenient_trigger(base, LocalLinkState::Available);
        assert_eq!(result.recovery_advice, RecoveryAdvice::FixConfiguration);
    }

    #[test]
    fn lenient_preserves_confirmed_captive_with_reachable_auth() {
        // 回归锚点：严格口径已给出明确门户证据 + 无差别登录建议时，宽松触发
        // 不得把它改写成推断级——否则 High/CaptiveDetected（确实检测到劫持）
        // 会被降级为 Low/LinkUpLoginAssumed（只是按链路猜），排障时无法区分
        let base = apply_auth_endpoint(
            assess_connectivity(&evidence(
                ProbeOutcome::Disabled,
                ProbeOutcome::Captive,
                ProbeOutcome::Disabled,
            )),
            AuthEndpointState::Reachable,
        );
        assert_eq!(base.recovery_advice, RecoveryAdvice::AttemptLogin);
        assert_eq!(base.confidence, AssessmentConfidence::High);
        assert_eq!(base.reason, AssessmentReason::CaptiveDetected);
        let result = apply_lenient_trigger(base, LocalLinkState::Available);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLogin);
        assert_eq!(result.confidence, AssessmentConfidence::High);
        assert_eq!(result.reason, AssessmentReason::CaptiveDetected);
        assert_eq!(result.status, NetworkStatus::CaptivePortal);
    }

    #[test]
    fn lenient_preserves_attempt_login_once_on_unreachable_auth() {
        // 明确门户证据但认证入口预检失败 → 「谨慎单次」（Engine 按配置版本去重，
        // 同一版本仅放行一次）。宽松触发若升级为无差别 AttemptLogin，会绕过该
        // 去重，退化为按失败节奏反复拉起浏览器
        let base = apply_auth_endpoint(
            assess_connectivity(&evidence(
                ProbeOutcome::Disabled,
                ProbeOutcome::Captive,
                ProbeOutcome::Fail,
            )),
            AuthEndpointState::Unreachable,
        );
        assert_eq!(base.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
        let result = apply_lenient_trigger(base, LocalLinkState::Available);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
        assert_eq!(result.confidence, AssessmentConfidence::High);
        assert_eq!(result.reason, AssessmentReason::CaptiveDetected);
    }

    #[test]
    fn lenient_preserves_attempt_once_on_inconclusive_with_reachable_auth() {
        // 证据不足（探测目标自身 5xx 等异常）+ 认证入口可达 → 谨慎单次。
        // 该输入本就意味着「探测目标异常」，升级为门户会误导用户判断
        let base = apply_auth_endpoint(
            assess_connectivity(&evidence(
                ProbeOutcome::Fail,
                ProbeOutcome::Inconclusive,
                ProbeOutcome::Disabled,
            )),
            AuthEndpointState::Reachable,
        );
        assert_eq!(base.status, NetworkStatus::Unknown);
        assert_eq!(base.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
        let result = apply_lenient_trigger(base, LocalLinkState::Available);
        assert_eq!(result.status, NetworkStatus::Unknown);
        assert_eq!(result.reason, AssessmentReason::InconclusiveEvidence);
        assert_eq!(result.recovery_advice, RecoveryAdvice::AttemptLoginOnce);
    }

    #[test]
    fn lenient_without_trigger_keeps_strict_semantics() {
        // 回归锚点：不调用 apply_lenient_trigger 时严格语义完全不变
        let base = assess_connectivity(&evidence(
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
            ProbeOutcome::Fail,
        ));
        let result = apply_auth_endpoint(base, AuthEndpointState::SkippedRedirectMode);
        assert_eq!(result.status, NetworkStatus::Offline);
        assert_eq!(result.recovery_advice, RecoveryAdvice::WaitForNetwork);
    }
}
