//! 会话互斥 + cancel_id 注册表

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

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
    /// debug_start 保留的会话
    Debug,
}

/// cancel_id（UUID）到 CancellationToken 的注册表
pub struct CancelRegistry {
    map: Mutex<HashMap<String, CancellationToken>>,
    /// 尚未注册就被取消的 cancel_id -> 请求时间（带 TTL，防止迟到的取消请求无界累积）
    pending: Mutex<HashMap<String, Instant>>,
}

impl CancelRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
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
        self.map
            .lock()
            .unwrap_or_else(recover_lock)
            .insert(cancel_id, token);
    }

    /// 触发取消（调用 token.cancel()），若尚未注册则记为 pending（带 TTL）
    pub fn trigger(&self, cancel_id: &str) {
        if let Some(token) = self
            .map
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id)
        {
            token.cancel();
        } else {
            self.prune_pending();
            self.pending
                .lock()
                .unwrap_or_else(recover_lock)
                .insert(cancel_id.to_string(), Instant::now());
        }
    }

    /// 清理已完成的注册项（连同 pending 一并清除：会话已结束，
    /// 此后同 id 的迟到取消无意义，避免滞留到 TTL 过期）
    pub fn remove(&self, cancel_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id);
        self.pending
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id);
    }

    /// 全部取消（Worker 崩溃时）
    pub fn trigger_all(&self) {
        let mut guard = self.map.lock().unwrap_or_else(recover_lock);
        for (_, token) in guard.drain() {
            token.cancel();
        }
        self.pending.lock().unwrap_or_else(recover_lock).clear();
    }

    /// 清空全部注册项但**不触发取消**
    ///
    /// 用于 Worker 崩溃回收：在途请求已通过 pending 通道收到定性错误（WorkerCrashed /
    /// DebugSessionClosed），无需再 cancel 其 token。避免与 pending 送达形成 `select!` 竞态（
    /// 否则崩溃请求会非确定地报 `Cancelled` 而非 `WorkerCrashed`），同时防止 token 泄漏。
    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(recover_lock).clear();
        self.pending.lock().unwrap_or_else(recover_lock).clear();
    }

    /// 是否已注册指定 cancel_id（测试辅助）
    #[cfg(test)]
    pub fn contains(&self, cancel_id: &str) -> bool {
        self.map
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
