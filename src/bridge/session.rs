//! 会话互斥 + cancel_id 注册表

use std::collections::HashMap;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

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
}

impl CancelRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// 注册新的取消令牌
    pub fn register(&self, cancel_id: String, token: CancellationToken) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(cancel_id, token);
    }

    /// 触发取消（调用 token.cancel()）
    pub fn trigger(&self, cancel_id: &str) {
        if let Some(token) = self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(cancel_id) {
            token.cancel();
        }
    }

    /// 清理已完成的注册项
    pub fn remove(&self, cancel_id: &str) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(cancel_id);
    }

    /// 全部取消（Worker 崩溃时）
    pub fn trigger_all(&self) {
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        for (_, token) in guard.drain() {
            token.cancel();
        }
    }

    /// 清空全部注册项但**不触发取消**
    ///
    /// 用于 Worker 崩溃回收：在途请求已通过 pending 通道收到定性错误（WorkerCrashed /
    /// DebugSessionClosed），无需再 cancel 其 token。避免与 pending 送达形成 `select!` 竞态（
    /// 否则崩溃请求会非确定地报 `Cancelled` 而非 `WorkerCrashed`），同时防止 token 泄漏。
    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 会话 RAII 守卫：drop 时复位会话状态
pub struct SessionGuard {
    session_type: SessionType,
    /// 本会话的请求 id（唯一），用于在复位时校验会话归属，避免
    /// 已结束会话的延迟 drop 误伤刚启动的同类型新会话。
    request_id: u64,
    cancelled: bool,
    on_drop: Option<Box<dyn FnOnce(SessionType, u64) + Send>>,
}

impl SessionGuard {
    /// 创建会话守卫，drop 时回调 `on_drop`（携带会话类型与 request_id）复位状态
    pub fn new(
        session_type: SessionType,
        request_id: u64,
        on_drop: impl FnOnce(SessionType, u64) + Send + 'static,
    ) -> Self {
        Self {
            session_type,
            request_id,
            cancelled: false,
            on_drop: Some(Box::new(on_drop)),
        }
    }

    /// 会话类型
    pub fn session_type(&self) -> SessionType {
        self.session_type
    }

    /// 标记已取消
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// 是否已取消
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// 显式强制关闭（调试会话回收）
    pub fn force_close(&mut self) {
        self.cancelled = true;
        if let Some(f) = self.on_drop.take() {
            f(self.session_type, self.request_id);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f(self.session_type, self.request_id);
        }
    }
}
