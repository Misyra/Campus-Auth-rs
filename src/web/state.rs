//! AppState 类型定义 + 共享状态

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::watch;

use crate::container::ServiceContainer;
use crate::login::{HistoryStore, LoginApi};
use crate::status::StatusManager;

/// WebSocket 日志条目（由内部事件推入广播通道，供 /ws/logs 订阅）
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    /// 全局单调递增序号（进程生命周期内唯一）
    ///
    /// 用途：前端 v-for 稳定 key（index key 在缓冲裁剪后导致整列表重建）、
    /// 实时日志去重（同毫秒同文案的两条日志不再被误判为重复）、
    /// 自动滚动触发依据（watch 长度在缓冲满员后不再变化）
    pub seq: u64,
    /// 日志级别（INFO/WARN/ERROR…）
    pub level: String,
    /// 日志消息
    pub message: String,
    /// ISO8601 时间戳
    pub timestamp: String,
    /// 日志来源（归一化后的短模块名，如 `launcher`/`scheduler`，由 tracing target 派生）
    #[serde(default)]
    pub source: String,
}

/// 日志序号发生器（全局单调递增）
static NEXT_LOG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LogEntry {
    /// 构造日志条目并分配单调序号（所有构造路径统一走此入口）
    pub fn new(level: String, message: String, timestamp: String, source: String) -> Self {
        Self {
            seq: NEXT_LOG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            level,
            message,
            timestamp,
            source,
        }
    }
}

/// 将 tracing target 归一化为短模块名，供前端来源过滤与展示
///
/// - `campus_auth::scheduler::cron_loop` → `scheduler`
/// - `campus_auth::launcher` → `launcher`
/// - `campus_auth`（crate 根） → `app`
/// - 外部 crate（如 `hyper_util::client`）→ 取首段 `hyper_util`
///
/// 归一化后前后端来源过滤（精确匹配短名）与徽章展示才能一致工作。
pub fn normalize_source(target: &str) -> String {
    let t = target.trim();
    if t.is_empty() {
        return String::new();
    }
    // 去掉 crate 前缀 `campus_auth::`
    let rest = t.strip_prefix("campus_auth::").unwrap_or(t);
    let first = rest.split("::").next().unwrap_or("").trim();
    if first.is_empty() || first == "campus_auth" {
        // crate 根模块（target 恰好为 `campus_auth`）
        return "app".to_string();
    }
    first.to_ascii_lowercase()
}

/// 应用共享状态：注入到所有 Axum handler 的 `State<AppState>`
#[derive(Clone)]
pub struct AppState {
    /// 服务容器（持有全部服务 Arc）
    pub container: Arc<ServiceContainer>,
    /// 登录历史存储（细粒度依赖，M1 试点：handler 经 FromRef 以
    /// `State<Arc<dyn HistoryStore>>` 提取，测试可注入内存实现）
    pub history: Arc<dyn HistoryStore>,
    /// 登录编排（M1 第二域：`State<Arc<dyn LoginApi>>` 提取）
    pub login: Arc<dyn LoginApi>,
    /// 状态管理器（M1：直字段替代 container.status 触达；StatusManager 本身
    /// 即内存实现，测试可直接构造，无需 trait 抽象）
    pub status: Arc<StatusManager>,
    /// 日志广播通道（WebSocket 订阅）
    pub log_tx: broadcast::Sender<LogEntry>,
    /// 通用事件广播通道（WebSocket 订阅，承载 screenshot/step_progress 等）
    pub ws_tx: broadcast::Sender<String>,
    /// 优雅关闭信号发送端（由 shutdown_app 等触发，通知 launcher 开始优雅关闭流程）
    pub shutdown_tx: watch::Sender<()>,
    /// WebSocket 单连接世代号：新连接接入时 +1，旧连接监测到世代号变化即断开。
    /// 实现「同一时刻只一个前端页面接收事件」，新标签页顶掉旧标签页。
    pub ws_epoch_tx: watch::Sender<u64>,
    /// 本地 API 鉴权 token（见 `web::auth` 模块说明）
    pub auth_token: Arc<str>,
}

impl AppState {
    /// 构造应用状态
    pub fn new(
        container: Arc<ServiceContainer>,
        log_tx: broadcast::Sender<LogEntry>,
        ws_tx: broadcast::Sender<String>,
        shutdown_tx: watch::Sender<()>,
        auth_token: Arc<str>,
    ) -> Self {
        // 初始世代号 0：首个连接接入后 +1 变为 1
        let (ws_epoch_tx, _) = watch::channel(0u64);
        // 细粒度依赖从容器抽出（trait object 化），handler 不再触达 container
        let history: Arc<dyn HistoryStore> = container.history.clone();
        let login: Arc<dyn LoginApi> = container.login.clone();
        let status = container.status.clone();
        Self {
            container,
            history,
            login,
            status,
            log_tx,
            ws_tx,
            shutdown_tx,
            ws_epoch_tx,
            auth_token,
        }
    }
}

/// 委派提取：handler 声明 `State<Arc<dyn HistoryStore>>` 即可从 AppState 取得
/// 细粒度依赖（M1）。axum 对 `State<S>` 本身的恒等 FromRef 已内建，此 impl
/// 仅服务于「Router 级 state 为 AppState」的主路由装配。
impl axum::extract::FromRef<AppState> for Arc<dyn HistoryStore> {
    fn from_ref(state: &AppState) -> Self {
        state.history.clone()
    }
}

/// 委派提取：`State<Arc<dyn LoginApi>>`（M1 第二域）
impl axum::extract::FromRef<AppState> for Arc<dyn LoginApi> {
    fn from_ref(state: &AppState) -> Self {
        state.login.clone()
    }
}

/// 委派提取：`State<Arc<StatusManager>>`（M1：status 直字段）
impl axum::extract::FromRef<AppState> for Arc<StatusManager> {
    fn from_ref(state: &AppState) -> Self {
        state.status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// normalize_source：模块路径归一化为短名
    #[test]
    fn test_normalize_source_various_targets() {
        assert_eq!(normalize_source("campus_auth::scheduler::cron_loop"), "scheduler");
        assert_eq!(normalize_source("campus_auth::launcher"), "launcher");
        assert_eq!(normalize_source("campus_auth"), "app");
        assert_eq!(normalize_source("hyper_util::client::legacy"), "hyper_util");
        assert_eq!(normalize_source("  campus_auth::bridge::mod  "), "bridge");
        assert_eq!(normalize_source(""), "");
        assert_eq!(normalize_source("   "), "");
    }
}
