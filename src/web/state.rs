//! AppState 类型定义 + 共享状态

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::watch;

use crate::container::ServiceContainer;

/// WebSocket 日志条目（由内部事件推入广播通道，供 /ws/logs 订阅）
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
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
    /// 日志广播通道（WebSocket 订阅）
    pub log_tx: broadcast::Sender<LogEntry>,
    /// 通用事件广播通道（WebSocket 订阅，承载 screenshot/step_progress 等）
    pub ws_tx: broadcast::Sender<String>,
    /// 优雅关闭信号发送端（由 shutdown_app 等触发，通知 launcher 开始优雅关闭流程）
    pub shutdown_tx: watch::Sender<()>,
}

impl AppState {
    /// 构造应用状态
    pub fn new(
        container: Arc<ServiceContainer>,
        log_tx: broadcast::Sender<LogEntry>,
        ws_tx: broadcast::Sender<String>,
        shutdown_tx: watch::Sender<()>,
    ) -> Self {
        Self {
            container,
            log_tx,
            ws_tx,
            shutdown_tx,
        }
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
