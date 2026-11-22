//! 去重/抢占矩阵：根据新来源与当前活跃会话来源计算决策
//!
//! 优先级数值（越高优先级越大）：`LoginOnce(4) > Browser(3) > Manual(2) > Auto(1)`。
//! 同一时刻最多只有一个活跃会话，新请求要么复用现有会话（去重），要么先取消再创建（抢占），
//! 要么在无活跃会话时直接创建。
//!
//! [`decide`] 是纯来源矩阵；[`decide_submit`] 是 submit 决策点的完整口径
//! （在矩阵之上叠加「已发射终态的活跃会话一律不复用」的 A1 防御），
//! 两者的组合空间由属性测试钉住。

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

/// 活跃会话在决策点的最小可见投影
///
/// 解耦 `preemption` 与 `login::mod` 私有的 `ActiveSession` 细节：submit 决策
/// 只关心来源、可复用句柄与「终态是否已发射」三个字段。
pub struct ActiveSessionView<'a> {
    /// 活跃会话来源
    pub source: LoginSource,
    /// 去重命中时返回的句柄；有活跃会话必有句柄，`None` 仅为保留 [`decide`]
    /// 的兜底语义（同 Auto / 更低优先级无句柄 → Create）
    pub handle: Option<&'a LoginHandle>,
    /// 终态是否已发射（`LoginHandle::is_resolved`）：已发射会话正在
    /// close_browser 收尾，复用只会立刻拿到旧结果并被重复计数
    pub resolved: bool,
}

/// submit 决策点（纯函数）：活跃会话状态 × 新来源 → 去重/抢占决策
///
/// 在 [`decide`] 的来源矩阵之上叠加 A1 的终态发射防御：
/// - 无活跃会话 → [`PreemptionDecision::Create`]
/// - 活跃会话已发射终态（`resolved`）→ [`PreemptionDecision::Preempt`]：
///   已 resolve 的活跃会话一律**不复用**（Reuse 只会立刻拿到旧结果，让
///   调用方把同一次登录重复计入失败预算），降级为抢占并等待其收尾
/// - 其余按 [`decide`] 的去重/抢占矩阵
pub fn decide_submit(
    new_source: LoginSource,
    active: Option<ActiveSessionView<'_>>,
) -> PreemptionDecision {
    match active {
        None => PreemptionDecision::Create,
        Some(view) if view.resolved => PreemptionDecision::Preempt,
        Some(view) => decide(new_source, Some(view.source), view.handle.cloned()),
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
    // 注：decide 的 Reuse 分支与叠加终态防御的 decide_submit 组合空间
    // 由下方属性测试（D1-D5）与全组合矩阵测试（D4）覆盖。

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

    // ============ decide_submit 属性测试（proptest） ============
    //
    // 句柄构造说明：LoginHandle 的私有字段对本文件的子模块测试可见
    // （同 crate::login 模块树内），无需再外溢到 login/mod.rs 覆盖 Reuse 分支。

    use std::sync::Arc;
    use std::time::Duration;

    use proptest::prelude::*;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use crate::login::{LoginHandleInner, LoginResult, LoginTerminal};

    /// 属性测试统一口径：512 cases（CI 亲和，无秒级超时；随机失败必可 shrink 复现）
    fn prop_config() -> proptest::test_runner::Config {
        proptest::test_runner::Config::with_cases(512)
    }

    /// 四个来源的均匀策略
    fn arb_source() -> impl proptest::strategy::Strategy<Value = LoginSource> {
        prop_oneof![
            Just(LoginSource::Auto),
            Just(LoginSource::Manual),
            Just(LoginSource::LoginOnce),
            Just(LoginSource::Browser),
        ]
    }

    /// 构造未发射终态的测试句柄（复用 watch channel，结果槽初始为 None）
    fn make_unresolved_handle(source: LoginSource) -> LoginHandle {
        let (result_tx, _rx) = watch::channel(None);
        LoginHandle {
            source,
            cancel_token: CancellationToken::new(),
            inner: Arc::new(LoginHandleInner { result_tx }),
        }
    }

    /// 构造最小终态结果（写入结果槽用）
    fn make_login_result(message: &str) -> LoginResult {
        LoginResult {
            network_retryable: false,
            terminal: LoginTerminal::Success,
            message: message.to_string(),
            source: LoginSource::Auto,
            duration: Duration::ZERO,
            attempts: 0,
        }
    }

    /// 决策变体标签（Reuse 内含句柄无 PartialEq，按形状比较）
    fn decision_tag(d: &PreemptionDecision) -> &'static str {
        match d {
            PreemptionDecision::Reuse(_) => "reuse",
            PreemptionDecision::Preempt => "preempt",
            PreemptionDecision::Create => "create",
        }
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【不变量 D1】无活跃会话必须新建：任何来源都不得 Reuse（无会话可复用）
        /// 也不得 Preempt（无人可抢占）
        #[test]
        fn prop_decide_submit_no_active_session_always_creates(new in arb_source()) {
            prop_assert!(
                matches!(decide_submit(new, None), PreemptionDecision::Create),
                "无活跃会话时 {new:?} 必须新建"
            );
        }

        /// 【不变量 D2】已发射终态（resolved）的活跃会话一律不复用：任意
        /// (新来源 × 活跃来源) 组合下降级为 Preempt，绝不 Reuse——这是
        /// 「终态已发射 + close_browser 收尾中的会话不被二次计入失败预算」
        /// 的决策防线（A1）
        #[test]
        fn prop_decide_submit_resolved_active_never_reused(new in arb_source(), active in arb_source()) {
            let handle = make_unresolved_handle(active);
            let view = ActiveSessionView { source: active, handle: Some(&handle), resolved: true };
            prop_assert!(
                matches!(decide_submit(new, Some(view)), PreemptionDecision::Preempt),
                "resolved 会话必须降级 Preempt（new={new:?}, active={active:?}）"
            );
        }

        /// 【不变量 D3】未发射终态 + 无句柄兜底时与 decide 矩阵完全一致：
        /// Preempt ⟺ 新来源优先级更高，或同来源非 Auto；否则 Create
        #[test]
        fn prop_decide_submit_unresolved_matches_decide_matrix(new in arb_source(), active in arb_source()) {
            let got = decide_submit(
                new,
                Some(ActiveSessionView { source: active, handle: None, resolved: false }),
            );
            // 与既有 decide 纯函数逐例一致（decide_submit 只是叠加 resolved 防御）
            prop_assert_eq!(
                decision_tag(&got),
                decision_tag(&decide(new, Some(active), None)),
                "decide_submit 未发射分支必须与 decide 一致（new={:?}, active={:?}）",
                new,
                active
            );
            // 决策语义独立钉死，不依赖 decide 的实现
            let expect_preempt =
                new.priority() > active.priority() || (new == active && active != LoginSource::Auto);
            prop_assert_eq!(
                matches!(got, PreemptionDecision::Preempt),
                expect_preempt,
                "new={:?}, active={:?}",
                new,
                active
            );
        }

        /// 【不变量 D5】随机组合下 Reuse 必须交还活跃会话**自身的句柄**：
        /// 来源一致且结果槽共享（向活跃槽写入标记后 is_resolved 同步可见），
        /// 绝不新造句柄——否则去重命中的调用方拿不到在途会话的终态。
        /// 非 Reuse 分支的矩阵由 D3/D4 钉住。
        #[test]
        fn prop_decide_submit_reuse_returns_active_handle(new in arb_source(), active in arb_source()) {
            let handle = make_unresolved_handle(active);
            let view = ActiveSessionView { source: active, handle: Some(&handle), resolved: false };
            if let PreemptionDecision::Reuse(h) = decide_submit(new, Some(view)) {
                prop_assert_eq!(h.source(), active, "复用句柄来源必须等于活跃会话来源");
                handle.inner.set_result(make_login_result("标记"));
                prop_assert!(h.is_resolved(), "复用句柄必须共享活跃会话结果槽");
            }
        }
    }

    /// 【不变量 D4】全部组合（4 新来源 × 4 活跃来源 × 有无句柄 × 是否已发射终态）
    /// 的决策矩阵钉死；同时验证复用句柄与活跃句柄共享结果槽（诱饵句柄不受影响）
    #[test]
    fn test_decide_submit_full_matrix() {
        let sources = [
            LoginSource::Auto,
            LoginSource::Manual,
            LoginSource::LoginOnce,
            LoginSource::Browser,
        ];
        for &new in &sources {
            assert!(
                matches!(decide_submit(new, None), PreemptionDecision::Create),
                "无活跃会话时 {new:?} 必须新建"
            );
            for &active_src in &sources {
                for resolved in [false, true] {
                    for handle_present in [false, true] {
                        let handle = make_unresolved_handle(active_src);
                        // 诱饵句柄：与活跃句柄同来源但结果槽独立
                        let decoy = make_unresolved_handle(active_src);
                        let view = ActiveSessionView {
                            source: active_src,
                            handle: handle_present.then_some(&handle),
                            resolved,
                        };
                        let d = decide_submit(new, Some(view));
                        let expect_tag = if resolved {
                            "preempt"
                        } else if new == active_src {
                            match (active_src, handle_present) {
                                (LoginSource::Auto, true) => "reuse",
                                (LoginSource::Auto, false) => "create",
                                _ => "preempt",
                            }
                        } else if new.priority() > active_src.priority() {
                            "preempt"
                        } else if handle_present {
                            "reuse"
                        } else {
                            "create"
                        };
                        assert_eq!(
                            decision_tag(&d),
                            expect_tag,
                            "new={new:?} active={active_src:?} resolved={resolved} handle={handle_present}"
                        );
                        // 复用必须交还活跃会话自身句柄（结果槽共享），而非诱饵
                        if let PreemptionDecision::Reuse(h) = d {
                            assert_eq!(h.source(), active_src);
                            assert!(!h.is_resolved(), "复用未发射会话时句柄应为未发射态");
                            handle.inner.set_result(make_login_result("标记"));
                            assert!(h.is_resolved(), "复用句柄必须共享活跃会话结果槽");
                            assert!(!decoy.is_resolved(), "诱饵句柄独立槽位不受影响");
                        }
                    }
                }
            }
        }
    }
}
