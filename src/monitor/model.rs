//! 网络探测证据、连通性判断与自动恢复建议的数据模型

use serde::Serialize;

use crate::status::NetworkStatus;

use super::ProbeOutcome;

/// 本地链路诊断结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalLinkState {
    /// 当前检测用途未执行本地链路诊断
    NotChecked,
    /// 至少发现一个具有有效地址的物理接口
    Available,
    /// 未发现有效物理接口
    Unavailable,
    /// 操作系统检测命令失败或超时
    ProbeFailed,
}

/// 认证入口探测状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthEndpointState {
    /// 当前检测用途无需检查认证入口
    NotChecked,
    /// TCP 直连认证入口成功
    Reachable,
    /// TCP 直连认证入口失败或超时
    Unreachable,
    /// 认证入口地址格式无效
    Invalid,
    /// 直接认证模式下未配置认证入口
    Missing,
    /// 重定向模式依赖 trigger_url，不执行直接认证入口预检
    SkippedRedirectMode,
}

/// 连通性判断置信度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentConfidence {
    /// 有明确公网成功或门户劫持证据
    High,
    /// 由公网失败与校内认证入口证据联合推断
    Medium,
    /// 证据不足、冲突或全部禁用
    Low,
}

/// 连通性主判断原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentReason {
    /// 尚未执行第一轮探测
    NotChecked,
    /// HTTP 204 或 URL 内容探测确认公网可用
    InternetVerified,
    /// HTTP 或 URL 探测发现明确门户劫持
    CaptiveDetected,
    /// 公网探测全部失败，但校内认证入口可达
    ExternalFailedAuthReachable,
    /// 所有已启用的公网探测均失败
    AllProbesFailed,
    /// 只有 TCP 等弱传输证据，不能确认公网状态
    WeakEvidenceOnly,
    /// 探测收到非预期状态码（目标服务异常或拦截式网关）：链路完整但
    /// 无法确认放行或劫持，证据不足
    InconclusiveEvidence,
    /// 强证据之间出现冲突
    ConflictingEvidence,
    /// 没有启用任何有效公网探测
    NoProbesEnabled,
}

/// 自动恢复建议
///
/// 这是监测层给 Engine 的建议，不代表最终一定执行；Engine 仍需检查暂停、
/// 冷却、配置版本与登录会话是否在途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAdvice {
    /// 网络已在线，无需恢复
    NoAction,
    /// 建议启动自动登录
    AttemptLogin,
    /// 认证入口预检失败但已有明确门户证据，按当前配置谨慎尝试一次
    AttemptLoginOnce,
    /// 更像物理断网，等待网络恢复
    WaitForNetwork,
    /// 当前证据不足，等待下一轮探测
    WaitForMoreEvidence,
    /// Profile 配置缺失或格式错误，需要用户修正
    FixConfiguration,
    /// 未启用有效探测，不能执行自动恢复
    NoProbeEvidence,
    /// 当前调用用途不评估自动恢复
    NotEvaluated,
}

/// 一轮探测收集到的原始证据
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeEvidence {
    /// TCP 弱连通探测结果
    pub tcp: ProbeOutcome,
    /// HTTP 204 门户探测结果
    pub http: ProbeOutcome,
    /// URL 内容探测结果
    pub url: ProbeOutcome,
    /// 本地链路诊断结果
    pub local_link: LocalLinkState,
}

impl ProbeEvidence {
    /// 构造不包含本地链路诊断的公网探测证据
    pub fn new(tcp: ProbeOutcome, http: ProbeOutcome, url: ProbeOutcome) -> Self {
        Self {
            tcp,
            http,
            url,
            local_link: LocalLinkState::NotChecked,
        }
    }
}

impl Default for ProbeEvidence {
    fn default() -> Self {
        Self::new(
            ProbeOutcome::Disabled,
            ProbeOutcome::Disabled,
            ProbeOutcome::Disabled,
        )
    }
}

/// 对一轮证据的统一解释
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectivityAssessment {
    /// 网络连通性状态
    pub status: NetworkStatus,
    /// 判断置信度
    pub confidence: AssessmentConfidence,
    /// 主判断原因
    pub reason: AssessmentReason,
    /// 认证入口补充证据
    pub auth_endpoint: AuthEndpointState,
    /// 自动恢复建议
    pub recovery_advice: RecoveryAdvice,
}

impl Default for ConnectivityAssessment {
    fn default() -> Self {
        Self {
            status: NetworkStatus::Unknown,
            confidence: AssessmentConfidence::Low,
            reason: AssessmentReason::NotChecked,
            auth_endpoint: AuthEndpointState::NotChecked,
            recovery_advice: RecoveryAdvice::NotEvaluated,
        }
    }
}

/// 一次完整检测的返回结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeReport {
    /// 原始探测证据
    pub evidence: ProbeEvidence,
    /// 对证据的统一解释
    pub assessment: ConnectivityAssessment,
    /// 整体检测耗时（毫秒）
    pub latency_ms: u64,
    /// 累计检测次数
    pub check_number: u64,
}
