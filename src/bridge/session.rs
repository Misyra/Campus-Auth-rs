//! 会话互斥 + cancel_id 注册表

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::BridgeError;
use crate::utils::recover_lock;

/// pending 取消请求的保留时长
///
/// pending 的唯一用途：cancel 先于 register 到达时（同一 cancel_id 随后立刻注册，
/// 如登录重试循环先生成 id 再注册），保证取消不丢。超过此时长仍未被同 id 注册，
/// 即可断定是会话结束后迟到的取消（UUID 不会复用，永远等不到 register），
/// 及时丢弃防止无界累积。
const PENDING_TTL: Duration = Duration::from_secs(60);

/// 会话类型，用于互斥判断
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// execute_login_attempt / execute_browser_task
    Login,
    /// 可见浏览器重定向检测（独占，避免与登录/调试互相关闭窗口）
    RedirectTest,
    /// debug_start 保留的会话
    Debug,
}

/// cancel_id（UUID）到 CancellationToken 的注册表
///
/// **双区**：会话类命令（登录/浏览器任务/调试）与轻量旁路（OCR/反馈截图）
/// 分开存放。二者都能被 `trigger`/`remove` 精确命中，但 `trigger_all`
/// （Worker 回收时的兜底取消）**只**作用于会话区——轻量旁路与任何会话并发，
/// 无条件全量取消会把与本次回收毫不相干的 OCR 请求判为已取消（P2-6）。
pub struct CancelRegistry {
    map: Mutex<HashMap<String, CancellationToken>>,
    /// 轻量旁路（`ocr_recognize` / `feedback_capture`）注册的 token
    lightweight: Mutex<HashMap<String, CancellationToken>>,
    /// 尚未注册就被取消的 cancel_id -> 请求时间（带 TTL，防止迟到的取消请求无界累积）
    pending: Mutex<HashMap<String, Instant>>,
}

impl CancelRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            lightweight: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 清理 pending 中超过 TTL 的过期项
    fn prune_pending(&self) {
        self.pending
            .lock()
            .unwrap_or_else(recover_lock)
            .retain(|_, t| t.elapsed() < PENDING_TTL);
    }

    /// 注册新的取消令牌（若已有 pending 取消则立即触发）
    pub fn register(&self, cancel_id: String, token: CancellationToken) {
        self.register_inner(cancel_id, token, false)
    }

    /// 注册轻量旁路令牌：与任意会话并发，不参与 `trigger_all` 的全量取消
    pub fn register_lightweight(&self, cancel_id: String, token: CancellationToken) {
        self.register_inner(cancel_id, token, true)
    }

    fn register_inner(&self, cancel_id: String, token: CancellationToken, lightweight: bool) {
        self.prune_pending();
        let was_pending = self
            .pending
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(&cancel_id)
            .is_some();
        if was_pending {
            token.cancel();
        }
        if lightweight {
            self.lightweight
                .lock()
                .unwrap_or_else(recover_lock)
                .insert(cancel_id, token);
        } else {
            self.map
                .lock()
                .unwrap_or_else(recover_lock)
                .insert(cancel_id, token);
        }
    }

    /// 触发取消（调用 token.cancel()），若尚未注册则记为 pending（带 TTL）
    pub fn trigger(&self, cancel_id: &str) {
        // 两个区都要查：取消请求只带 cancel_id，不区分请求类别
        if let Some(token) = self
            .map
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id)
        {
            token.cancel();
            return;
        }
        if let Some(token) = self
            .lightweight
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id)
        {
            token.cancel();
            return;
        }
        self.prune_pending();
        self.pending
            .lock()
            .unwrap_or_else(recover_lock)
            .insert(cancel_id.to_string(), Instant::now());
    }

    /// 清理已完成的注册项（连同 pending 一并清除：会话已结束，
    /// 此后同 id 的迟到取消无意义，避免滞留到 TTL 过期）
    pub fn remove(&self, cancel_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id);
        self.lightweight
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id);
        self.pending
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id);
    }

    /// 全部取消（Worker 崩溃时）
    ///
    /// **只取消会话区**：轻量旁路（OCR/反馈截图）与任意会话并发，其失败与否
    /// 与本次 Worker 回收无关；全量取消会让并发 OCR 请求被判 Cancelled（P2-6）。
    /// 轻量请求自身的收尾由其 guard drop 的 `lightweight_cleanup` 负责。
    pub fn trigger_all(&self) {
        let mut guard = self.map.lock().unwrap_or_else(recover_lock);
        for (_, token) in guard.drain() {
            token.cancel();
        }
        self.pending.lock().unwrap_or_else(recover_lock).clear();
    }

    /// 清空全部注册项但**不触发取消**（会话区 + 轻量区）
    ///
    /// 用于 Worker 崩溃回收：在途请求已通过 pending 通道收到定性错误（WorkerCrashed /
    /// DebugSessionClosed），无需再 cancel 其 token。避免与 pending 送达形成 `select!` 竞态（
    /// 否则崩溃请求会非确定地报 `Cancelled` 而非 `WorkerCrashed`），同时防止 token 泄漏。
    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(recover_lock).clear();
        self.lightweight.lock().unwrap_or_else(recover_lock).clear();
        self.pending.lock().unwrap_or_else(recover_lock).clear();
    }

    /// 是否已注册指定 cancel_id（测试辅助）
    #[cfg(test)]
    pub fn contains(&self, cancel_id: &str) -> bool {
        self.map
            .lock()
            .unwrap_or_else(recover_lock)
            .contains_key(cancel_id)
            || self
                .lightweight
                .lock()
                .unwrap_or_else(recover_lock)
                .contains_key(cancel_id)
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 会话 RAII 守卫：drop 时执行清理回调
///
/// 清理逻辑完全由构造时传入的闭包决定，从而支持两种语义：
/// - 普通会话（登录/调试）：复位会话槽位 + 启动空闲计时器（`reset_session`）；
/// - OCR 轻量请求：仅清理自身 pending 与 cancel 注册，不触碰会话槽位（避免摧毁
///   并发登录会话，见 5.1）。闭包捕获校验所需的一切状态（request_id / cancel_id）。
pub struct SessionGuard {
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl SessionGuard {
    /// 创建会话守卫，drop 时回调 `on_drop` 执行清理
    pub fn new(on_drop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            on_drop: Some(Box::new(on_drop)),
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

/// 不占用登录/调试单会话槽的轻量命令。
pub(super) fn is_lightweight_method(method: &str) -> bool {
    matches!(method, "ocr_recognize" | "feedback_capture")
}

/// 会话互斥矩阵：判断新请求 `method` 与当前活跃会话 `current` 是否兼容。
///
/// 兼容（允许继续）：
/// - 无活跃会话：任意方法
/// - InLogin + (execute_login_attempt/execute_browser_task/debug_start)：FIFO 排队
/// - InDebug + (debug_step/debug_stop/debug_run_all)：FIFO 排队
/// - OCR / 反馈捕获等轻量命令可与任意会话并发
///
/// 不兼容（快速失败 [`BridgeError::WorkerBusy`]）：
/// - InLogin + (debug_step/debug_stop/test_redirect)
/// - InLogin + profile_auto_login：持久自动登录**不与另一登录并发**——双方
///   都会做「登录后网络验证」兜底（探测与门户流量互相污染判定），排队
///   进 Worker 也会与前序登录的验证阶段真正并发；快速失败后由登录会话
///   转 NetworkError 退避重试，等对方完全收尾再进
/// - InDebug + (execute_login_attempt/execute_browser_task/profile_auto_login)：
///   登录类请求快速失败
/// - InDebug + debug_start：已有调试会话
/// - RedirectTest 与所有非轻量会话互斥：可见临时窗口不得被登录/调试关闭
///
/// `ocr_recognize` / `feedback_capture` 为轻量命令，允许与任意会话并发。
pub(super) fn check_session_compat(
    current: Option<SessionType>,
    method: &str,
) -> Result<(), BridgeError> {
    if method == "ocr_recognize" || method == "feedback_capture" {
        return Ok(());
    }
    match current {
        None => Ok(()),
        Some(SessionType::Login) => match method {
            "debug_step" | "debug_stop" | "test_redirect" => Err(BridgeError::WorkerBusy),
            // 持久自动登录拥有独立持久上下文，但不与另一登录并发（见函数头）；
            // 归类先例同 debug_start：自有运行时，不走 OCR 式轻量旁路
            "profile_auto_login" => Err(BridgeError::WorkerBusy),
            // execute_login_attempt / execute_browser_task / debug_start：FIFO 排队
            _ => Ok(()),
        },
        Some(SessionType::RedirectTest) => Err(BridgeError::WorkerBusy),
        Some(SessionType::Debug) => {
            if method.starts_with("debug_") {
                match method {
                    // debug_status / feedback_capture 为无副作用查询，允许在会话存续期随时调用
                    "debug_step" | "debug_stop" | "debug_run_all" | "debug_status"
                    | "feedback_capture" => Ok(()),
                    _ => Err(BridgeError::WorkerBusy),
                }
            } else {
                // 登录类命令（execute_login_attempt / execute_browser_task /
                // profile_auto_login）在调试会话存续期一律拒绝：与登录口径一致
                Err(BridgeError::WorkerBusy)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::test_support::{assert_busy, assert_ok};

    /// cancel 先于 register 到达（pending 命中）→ 注册时立即取消
    #[test]
    fn test_pending_cancel_fires_on_register() {
        let reg = CancelRegistry::new();
        reg.trigger("id-1");
        let token = CancellationToken::new();
        reg.register("id-1".into(), token.clone());
        assert!(token.is_cancelled());
    }

    /// 会话结束后 remove 清掉 pending：迟到的取消不再滞留（内存泄露回归）
    #[test]
    fn test_remove_clears_pending() {
        let reg = CancelRegistry::new();
        reg.trigger("id-1");
        reg.remove("id-1");
        let token = CancellationToken::new();
        reg.register("id-1".into(), token.clone());
        assert!(!token.is_cancelled());
    }

    /// 超过 TTL 的 pending 过期：迟到的取消不再影响之后同 id 的注册
    #[test]
    fn test_pending_ttl_expires() {
        let reg = CancelRegistry::new();
        reg.trigger("id-1");
        // 手动把时间戳拨回 TTL 之前（同一模块内可访问私有字段）
        let past = Instant::now() - PENDING_TTL - Duration::from_secs(1);
        reg.pending.lock().unwrap().insert("id-1".into(), past);
        let token = CancellationToken::new();
        reg.register("id-1".into(), token.clone());
        assert!(!token.is_cancelled());
        assert!(reg.pending.lock().unwrap().is_empty());
    }

    /// P2-6：`trigger_all` 不得波及轻量旁路（并发的 OCR/反馈截图）
    ///
    /// Worker 回收时全量取消会把与本次回收无关的 OCR 请求判为 Cancelled。
    #[test]
    fn test_trigger_all_spares_lightweight_registrations() {
        let reg = CancelRegistry::new();
        let session_token = CancellationToken::new();
        let ocr_token = CancellationToken::new();
        reg.register("session-1".into(), session_token.clone());
        reg.register_lightweight("ocr-1".into(), ocr_token.clone());

        reg.trigger_all();

        assert!(session_token.is_cancelled(), "会话类 token 应被全量取消");
        assert!(
            !ocr_token.is_cancelled(),
            "轻量旁路 token 不得被 trigger_all 波及（P2-6）"
        );
        // 轻量项仍在注册表中，其自身收尾仍可正常进行
        assert!(reg.contains("ocr-1"));
    }

    /// P2-6：轻量旁路的取消仍需能被精确命中（分区不影响 trigger/remove 语义）
    #[test]
    fn test_lightweight_still_precisely_cancellable() {
        let reg = CancelRegistry::new();
        let ocr_token = CancellationToken::new();
        reg.register_lightweight("ocr-2".into(), ocr_token.clone());

        reg.trigger("ocr-2");
        assert!(ocr_token.is_cancelled(), "轻量 token 应可被精准取消");

        // remove 同样覆盖两个分区
        let ocr_token2 = CancellationToken::new();
        reg.register_lightweight("ocr-3".into(), ocr_token2.clone());
        reg.remove("ocr-3");
        assert!(!reg.contains("ocr-3"));
    }

    /// P2-6：`clear`（Worker 崩溃回收）仍清空两个分区（防 token 泄漏）
    #[test]
    fn test_clear_empties_both_partitions() {
        let reg = CancelRegistry::new();
        reg.register("session-9".into(), CancellationToken::new());
        reg.register_lightweight("ocr-9".into(), CancellationToken::new());
        reg.clear();
        assert!(!reg.contains("session-9"));
        assert!(!reg.contains("ocr-9"));
    }

    // ============ 会话互斥矩阵 ============

    #[test]
    fn 无会话_任意方法均兼容() {
        for m in [
            "execute_login_attempt",
            "execute_browser_task",
            "test_redirect",
            "debug_start",
            "debug_step",
            "debug_stop",
            "profile_auto_login",
            "ocr_recognize",
            "feedback_capture",
        ] {
            assert_ok(None, m);
        }
    }

    #[test]
    fn 登录会话_允许登录与浏览器任务() {
        assert_ok(Some(SessionType::Login), "execute_login_attempt");
        assert_ok(Some(SessionType::Login), "execute_browser_task");
        assert_busy(Some(SessionType::Login), "test_redirect");
    }

    #[test]
    fn 登录会话_允许启动调试会话() {
        // 计划矩阵：InLogin + debug_start 走 FIFO 排队
        assert_ok(Some(SessionType::Login), "debug_start");
    }

    #[test]
    fn 登录会话_调试步进与停止应忙碌() {
        assert_busy(Some(SessionType::Login), "debug_step");
        assert_busy(Some(SessionType::Login), "debug_stop");
    }

    /// 持久自动登录的互斥归类：在途登录期间快速失败（不与另一登录并发，
    /// 双方的网络验证兜底会互相干扰），无会话时可进入
    #[test]
    fn 持久自动登录_在途登录期间应忙碌() {
        assert_busy(Some(SessionType::Login), "profile_auto_login");
        assert_ok(None, "profile_auto_login");
    }

    /// 持久自动登录与调试会话 / 可见重定向窗口互斥（登录类命令口径）
    #[test]
    fn 持久自动登录_调试与重定向会话期间应忙碌() {
        assert_busy(Some(SessionType::Debug), "profile_auto_login");
        assert_busy(Some(SessionType::RedirectTest), "profile_auto_login");
    }

    #[test]
    fn 调试会话_允许步进与停止() {
        assert_ok(Some(SessionType::Debug), "debug_step");
        assert_ok(Some(SessionType::Debug), "debug_stop");
        assert_ok(Some(SessionType::Debug), "debug_run_all");
    }

    #[test]
    fn 调试会话_再次启动调试应忙碌() {
        assert_busy(Some(SessionType::Debug), "debug_start");
    }

    #[test]
    fn 调试会话_登录与浏览器任务应忙碌() {
        assert_busy(Some(SessionType::Debug), "execute_login_attempt");
        assert_busy(Some(SessionType::Debug), "execute_browser_task");
        assert_busy(Some(SessionType::Debug), "test_redirect");
    }

    #[test]
    fn 重定向检测_独占浏览器会话() {
        for method in [
            "execute_login_attempt",
            "execute_browser_task",
            "test_redirect",
            "debug_start",
            "debug_step",
            "debug_stop",
        ] {
            assert_busy(Some(SessionType::RedirectTest), method);
        }
    }

    #[test]
    fn ocr识别_与任意会话并发兼容() {
        assert_ok(None, "ocr_recognize");
        assert_ok(Some(SessionType::Login), "ocr_recognize");
        assert_ok(Some(SessionType::RedirectTest), "ocr_recognize");
        assert_ok(Some(SessionType::Debug), "ocr_recognize");
    }

    #[test]
    fn 反馈捕获_走轻量路径且与任意会话兼容() {
        assert!(is_lightweight_method("feedback_capture"));
        assert_ok(None, "feedback_capture");
        assert_ok(Some(SessionType::Login), "feedback_capture");
        assert_ok(Some(SessionType::RedirectTest), "feedback_capture");
        assert_ok(Some(SessionType::Debug), "feedback_capture");
    }
}
