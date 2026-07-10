//! 去重/抢占矩阵：根据新来源与当前活跃会话来源计算决策
//!
//! 优先级数值（越高优先级越大）：`LoginOnce(4) > Browser(3) > Manual(2) > Auto(1)`。
//! 同一时刻最多只有一个活跃会话，新请求要么复用现有会话（去重），要么先取消再创建（抢占），
//! 要么在无活跃会话时直接创建。

use crate::login::LoginHandle;
use crate::status::LoginSource;

/// 去重/抢占决策
#[derive(Debug, Clone)]
pub enum PreemptionDecision {
    /// 复用现有会话（去重），返回现有句柄
    Reuse(LoginHandle),
    /// 抢占：取消旧会话后由调用方创建新会话
    Preempt,
    /// 无活跃会话，直接创建新会话
    Create,
}

impl LoginSource {
    /// 优先级数值，越高越优先
    ///
    /// `LoginOnce=4 > Browser=3 > Manual=2 > Auto=1`
    pub fn priority(self) -> u8 {
        match self {
            LoginSource::LoginOnce => 4,
            LoginSource::Browser => 3,
            LoginSource::Manual => 2,
            LoginSource::Auto => 1,
        }
    }
}

/// 根据新来源与当前活跃会话来源，纯函数计算去重/抢占决策
///
/// - 无活跃会话 → [`PreemptionDecision::Create`]
/// - 新来源优先级更高 → [`PreemptionDecision::Preempt`]
/// - 同来源：
///   - `Auto` → [`PreemptionDecision::Reuse`]（去重，返回现有句柄）
///   - `Manual` / `LoginOnce` / `Browser` → [`PreemptionDecision::Preempt`]
///     （同类请求不应去重复用，应重新执行）
/// - 新来源优先级更低或相等（跨来源）→ [`PreemptionDecision::Reuse`]
pub fn decide(
    new_source: LoginSource,
    current_source: Option<LoginSource>,
    current_handle: Option<LoginHandle>,
) -> PreemptionDecision {
    match current_source {
        None => PreemptionDecision::Create,
        Some(current) => {
            if new_source == current {
                // 同来源：Auto 去重复用，其余同类请求走抢占重新执行
                match current {
                    LoginSource::Auto => match current_handle {
                        Some(handle) => PreemptionDecision::Reuse(handle),
                        // 理论上不会发生：有活跃来源必有句柄；兜底为创建
                        None => PreemptionDecision::Create,
                    },
                    _ => PreemptionDecision::Preempt,
                }
            } else if new_source.priority() > current.priority() {
                PreemptionDecision::Preempt
            } else {
                match current_handle {
                    Some(handle) => PreemptionDecision::Reuse(handle),
                    None => PreemptionDecision::Create,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::LoginSource;

    // ============ LoginSource::priority 测试 ============

    #[test]
    fn test_priority_ordering() {
        // 文档约定：LoginOnce(4) > Browser(3) > Manual(2) > Auto(1)
        assert_eq!(LoginSource::LoginOnce.priority(), 4);
        assert_eq!(LoginSource::Browser.priority(), 3);
        assert_eq!(LoginSource::Manual.priority(), 2);
        assert_eq!(LoginSource::Auto.priority(), 1);
    }

    #[test]
    fn test_priority_login_once_beats_browser() {
        assert!(LoginSource::LoginOnce.priority() > LoginSource::Browser.priority());
    }

    // ============ decide 纯函数测试（current_handle = None 分支） ============
    // 注：Reuse 分支需要 LoginHandle 实例，其字段为 mod.rs 私有，无法在 preemption.rs
    // 构造；该分支在 login/mod.rs 的测试模块中覆盖。

    #[test]
    fn test_decide_no_active_session_creates() {
        // 无活跃会话 → 直接创建
        let d = decide(LoginSource::Auto, None, None);
        assert!(matches!(d, PreemptionDecision::Create));
    }

    #[test]
    fn test_decide_higher_priority_preempts() {
        // 新来源优先级更高 → 抢占（无论是否同来源）
        // LoginOnce(4) vs Manual(2)
        assert!(matches!(
            decide(LoginSource::LoginOnce, Some(LoginSource::Manual), None),
            PreemptionDecision::Preempt
        ));
        // Browser(3) vs Auto(1)
        assert!(matches!(
            decide(LoginSource::Browser, Some(LoginSource::Auto), None),
            PreemptionDecision::Preempt
        ));
        // Manual(2) vs Auto(1)
        assert!(matches!(
            decide(LoginSource::Manual, Some(LoginSource::Auto), None),
            PreemptionDecision::Preempt
        ));
        // Browser(3) vs Manual(2)
        assert!(matches!(
            decide(LoginSource::Browser, Some(LoginSource::Manual), None),
            PreemptionDecision::Preempt
        ));
    }

    #[test]
    fn test_decide_same_source_non_auto_preempts() {
        // 同来源且非 Auto → 抢占（同类请求不应去重，应重新执行）
        assert!(matches!(
            decide(LoginSource::Manual, Some(LoginSource::Manual), None),
            PreemptionDecision::Preempt
        ));
        assert!(matches!(
            decide(LoginSource::Browser, Some(LoginSource::Browser), None),
            PreemptionDecision::Preempt
        ));
        assert!(matches!(
            decide(LoginSource::LoginOnce, Some(LoginSource::LoginOnce), None),
            PreemptionDecision::Preempt
        ));
    }

    #[test]
    fn test_decide_same_auto_with_none_handle_falls_back_to_create() {
        // 同来源 Auto + 无句柄（理论上不应发生）→ 兜底创建
        let d = decide(LoginSource::Auto, Some(LoginSource::Auto), None);
        assert!(matches!(d, PreemptionDecision::Create));
    }

    #[test]
    fn test_decide_lower_priority_with_none_handle_creates() {
        // 新来源优先级更低 + 无句柄 → 兜底创建（有句柄时才 Reuse）
        // Auto(1) vs Manual(2)
        assert!(matches!(
            decide(LoginSource::Auto, Some(LoginSource::Manual), None),
            PreemptionDecision::Create
        ));
        // Manual(2) vs Browser(3)
        assert!(matches!(
            decide(LoginSource::Manual, Some(LoginSource::Browser), None),
            PreemptionDecision::Create
        ));
    }

    // 注：Reuse 分支需要 LoginHandle 实例，其字段为 mod.rs 私有，无法在 preemption.rs 构造；
    // 该分支在 login/mod.rs 的测试模块中覆盖。
}
