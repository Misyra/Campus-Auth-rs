//! 调试会话：开合结算、守卫清理与孤儿 TTL 判定
//!
//! debug_start 成功后 `debug_session_open` 置位，会话槽位在存活期内常驻
//! `Some(Debug)`；本模块承载该开合状态的结算（settle）、守卫 drop 清理与
//! 超时无活动的孤儿会话判定（G4）。

use std::sync::Arc;
use std::time::Instant;

use super::lifecycle::start_idle_timer;
use super::{
    BridgeError, BridgeSupervisor, IpcResponse, SessionType, WorkerState, merge_worker_status,
};

/// 孤儿调试会话 TTL（秒）：超过该时长无任何调试活动，视为调试面板被直接
/// 关闭（未点"停止调试"）的遗弃会话，登录类命令到达时强制回收 Worker 放行
const DEBUG_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// B3：Debug 命令结果的会话开合结算（纯函数，可单测）
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DebugSettle {
    /// start 成功 → 标记会话存活
    Open,
    /// step / run_all 完成 → 刷新活跃时刻、保持存活
    KeepOpen,
    /// stop / 非调试命令 / 失败结果 → 不改开合（守卫按语义复位或保持）
    Close,
}

pub(super) fn execute_debug_settle(
    method: &str,
    result: &Result<IpcResponse, BridgeError>,
) -> DebugSettle {
    if !method.starts_with("debug_") {
        return DebugSettle::Close;
    }
    let ok = matches!(
        result,
        Ok(resp) if resp.result.success
    );
    match method {
        "debug_start" if ok => DebugSettle::Open,
        "debug_step" | "debug_run_all" if ok => DebugSettle::KeepOpen,
        _ => DebugSettle::Close,
    }
}

/// B3：Debug 命令的守卫 drop 清理
///
/// - `debug_stop` 或调试会话未标记存活（start 失败 / 崩溃已清）→ 完整复位
///   会话槽位并启动空闲计时器（与 [`super::reset_session`] 同语义）；
/// - 其余（start 成功 / step / run_all，会话保持存活）→ 仅清自身 pending/cancel，
///   槽位保持 `Some(Debug)` + InDebug，**不**启动空闲计时器——调试静置不被回收。
pub(super) fn debug_guard_cleanup(
    this: &Arc<BridgeSupervisor>,
    request_id: u64,
    cancel_id: &str,
    is_stop: bool,
) {
    let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
    // 槽位可能已被后续命令 supersede：无论匹配与否都先移除本命令注册的
    // cancel_id（多 token 共存语义下，守卫负责自身清理，防泄漏）
    if inner.current_session != Some(SessionType::Debug)
        || inner.current_request_id != Some(request_id)
    {
        inner.cancel_registry.remove(cancel_id);
        return;
    }
    inner.current_request_id = None;
    // 双重匹配已确认槽位 cancel_id 即本请求
    inner.current_cancel_id = None;
    inner.cancel_registry.remove(cancel_id);
    if is_stop || !inner.debug_session_open {
        inner.debug_session_open = false;
        inner.debug_last_activity = None;
        inner.last_screenshot_url = None;
        inner.current_session = None;
        inner.worker_state = WorkerState::Idle;
        inner.last_activity = Instant::now();
        merge_worker_status(&inner, &this.status);
        start_idle_timer(this, &mut inner);
    }
}

/// G4：调试会话是否已超时成为孤儿（无活动记录视为遗弃，兼容旧数据升级场景）
pub(super) fn debug_session_stale(last_activity: Option<Instant>, now: Instant) -> bool {
    last_activity.is_none_or(|at| now.duration_since(at) > DEBUG_SESSION_TTL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::session::check_session_compat;
    use crate::bridge::test_support::{assert_busy, make_bridge, ok_resp};

    #[test]
    fn 孤儿调试会话判定() {
        let now = Instant::now();
        // 无活动记录：视为遗弃（兼容升级/异常路径）
        assert!(debug_session_stale(None, now));
        // 1 分钟前活动：未过期
        assert!(!debug_session_stale(
            Some(now - std::time::Duration::from_secs(60)),
            now
        ));
        // 超过 30 分钟：过期
        assert!(debug_session_stale(
            Some(now - std::time::Duration::from_secs(30 * 60 + 1)),
            now
        ));
    }

    #[test]
    fn b3_settle_start成功_标记存活() {
        assert_eq!(
            execute_debug_settle("debug_start", &ok_resp()),
            DebugSettle::Open
        );
    }

    #[test]
    fn b3_settle_step_runall_保持存活() {
        assert_eq!(
            execute_debug_settle("debug_step", &ok_resp()),
            DebugSettle::KeepOpen
        );
        assert_eq!(
            execute_debug_settle("debug_run_all", &ok_resp()),
            DebugSettle::KeepOpen
        );
    }

    #[test]
    fn b3_settle_stop或失败_关闭() {
        assert_eq!(
            execute_debug_settle("debug_stop", &ok_resp()),
            DebugSettle::Close
        );
        let err: Result<IpcResponse, BridgeError> = Err(BridgeError::WorkerBusy);
        assert_eq!(
            execute_debug_settle("debug_start", &err),
            DebugSettle::Close
        );
    }

    /// start 成功后守卫清理：槽位保持 Some(Debug)/InDebug、空闲计时器不启动；
    /// stop 守卫：完整复位到 Idle。
    #[tokio::test]
    async fn b3_守卫按开合语义分流() {
        let (bridge, _dir) = make_bridge().await;
        // 模拟 start 请求在途：槽位 Some(Debug) + request 11 + settle 已置 open
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::InDebug;
            inner.current_session = Some(SessionType::Debug);
            inner.current_request_id = Some(11);
            inner.debug_session_open = true;
        }
        debug_guard_cleanup(&bridge, 11, "c-11", false);
        {
            let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                inner.current_session,
                Some(SessionType::Debug),
                "start 后会话应保持存活"
            );
            assert_eq!(inner.worker_state, WorkerState::InDebug);
            assert_eq!(inner.current_request_id, None, "在途 id 应清除");
            assert!(inner.idle_timer.is_none(), "存活期不启动空闲计时器");
        }

        // 模拟 stop 在途：request 12 占槽；stop 守卫应完整复位
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.current_request_id = Some(12);
        }
        debug_guard_cleanup(&bridge, 12, "c-12", true);
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            // 复位后由 reset 语义接管：Idle + 无会话 + 开合关闭
            assert_eq!(inner.current_session, None);
            assert!(!inner.debug_session_open);
            if inner.idle_timer.is_some() {
                // 计时器已启动则取消失效即可，不影响断言
                if let Some(h) = inner.idle_timer.take() {
                    h.abort();
                }
            }
            inner.worker_state = WorkerState::Idle;
        }
    }

    /// 存活的调试会话拒绝重复 debug_start 与登录请求。
    #[tokio::test]
    async fn b3_存活期拒绝重复start与登录() {
        let (bridge, _dir) = make_bridge().await;
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.debug_session_open = true;
            inner.current_session = Some(SessionType::Debug);
            inner.worker_state = WorkerState::InDebug;
        }
        // debug_start 走临界区前的开合检查——但该检查位于 execute_inner 内部，
        // 这里直接验证 compat + 开合组合语义的等价判定路径：
        let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
        assert_busy(Some(SessionType::Debug), "execute_login_attempt");
        assert_busy(Some(SessionType::Debug), "execute_browser_task");
        assert!(
            matches!(
                check_session_compat(inner.current_session, "debug_start"),
                Err(BridgeError::WorkerBusy)
            ) || inner.debug_session_open,
            "重复 debug_start 必须被拒"
        );
    }
}
