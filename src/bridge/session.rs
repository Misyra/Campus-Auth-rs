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
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 会话 RAII 守卫：drop 时复位会话状态
pub struct SessionGuard {
    session_type: SessionType,
    cancelled: bool,
    on_drop: Option<Box<dyn FnOnce(SessionType) + Send>>,
}

impl SessionGuard {
    /// 创建会话守卫，drop 时回调 `on_drop` 复位状态
    pub fn new(
        session_type: SessionType,
        on_drop: impl FnOnce(SessionType) + Send + 'static,
    ) -> Self {
        Self {
            session_type,
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
            f(self.session_type);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f(self.session_type);
        }
    }
}
