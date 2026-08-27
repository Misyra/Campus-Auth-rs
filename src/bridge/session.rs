//! 会话互斥 + cancel_id 注册表

use std::collections::HashMap;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use crate::utils::recover_lock;

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
    pending: Mutex<std::collections::HashSet<String>>,
}

impl CancelRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            pending: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// 注册新的取消令牌（若已有 pending 取消则立即触发）
    pub fn register(&self, cancel_id: String, token: CancellationToken) {
        let was_pending = self
            .pending
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(&cancel_id);
        if was_pending {
            token.cancel();
        }
        self.map
            .lock()
            .unwrap_or_else(recover_lock)
            .insert(cancel_id, token);
    }

    /// 触发取消（调用 token.cancel()），若尚未注册则记为 pending
    pub fn trigger(&self, cancel_id: &str) {
        if let Some(token) = self
            .map
            .lock()
            .unwrap_or_else(recover_lock)
            .remove(cancel_id)
        {
            token.cancel();
        } else {
            self.pending
                .lock()
                .unwrap_or_else(recover_lock)
                .insert(cancel_id.to_string());
        }
    }

    /// 清理已完成的注册项
    pub fn remove(&self, cancel_id: &str) {
        self.map
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
