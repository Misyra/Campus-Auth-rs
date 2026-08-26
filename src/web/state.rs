//! AppState 类型定义 + 共享状态

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::watch;

use crate::bridge::BridgeApi;
use crate::config::{ConfigApi, ProfileApi};
use crate::container::ServiceContainer;
use crate::engine::EngineApi;
use crate::environment::EnvironmentApi;
use crate::login::{HistoryStore, LoginApi};
use crate::scheduler::SchedulerApi;
use crate::status::StatusManager;
use crate::tasks::{TaskApi, TaskRunApi};
use crate::updater::UpdaterApi;
use crate::utils::metrics::Metrics;

// WebSocket 日志条目与来源归一化已迁至 src/logging.rs（A-1）；
// 此处再导出保持既有 `state::LogEntry` 路径的消费方兼容。
pub use crate::logging::{LogEntry, normalize_source};

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
    /// 调度器（M1 第三域：`State<Arc<dyn SchedulerApi>>` 提取）
    pub scheduler: Arc<dyn SchedulerApi>,
    /// 任务管理（M1 第四域：`State<Arc<dyn TaskApi>>` 提取）
    pub tasks: Arc<dyn TaskApi>,
    /// 任务执行（M1：tasks 域伴生，`State<Arc<dyn TaskRunApi>>` 提取）
    pub task_runner: Arc<dyn TaskRunApi>,
    /// 配置服务（M1 第五域：`State<Arc<dyn ConfigApi>>` 提取；混合依赖
    /// handler 亦经 `state.config` 直字段触达）
    pub config: Arc<dyn ConfigApi>,
    /// Profile 业务（M1 第六域：`state.profiles` 直字段触达）
    pub profiles: Arc<dyn ProfileApi>,
    /// Bridge（M1 第七域：`State<Arc<dyn BridgeApi>>` 提取）
    pub bridge: Arc<dyn BridgeApi>,
    /// 环境能力（M1 第八域：`State<Arc<dyn EnvironmentApi>>` 提取）
    pub environment: Arc<dyn EnvironmentApi>,
    /// 更新器（M1 第九域：`State<Arc<dyn UpdaterApi>>` 提取）
    pub updater: Arc<dyn UpdaterApi>,
    /// 引擎（M1 第十域：`State<Arc<dyn EngineApi>>` 提取；实现为
    /// EngineSlot，崩溃重启后自动指向新实例，引用收口见 engine/slot.rs）
    pub engine: Arc<dyn EngineApi>,
    /// 运行指标（M1：直字段；Metrics 为内存 AtomicU64 集合，无需 trait 抽象）
    pub metrics: Arc<Metrics>,
    /// 状态管理器（M1：直字段替代 container.status 触达；StatusManager 本身
    /// 即内存实现，测试可直接构造，无需 trait 抽象）
    pub status: Arc<StatusManager>,
    /// 日志广播通道（WebSocket 订阅）
    pub log_tx: broadcast::Sender<LogEntry>,
    /// 通用事件广播通道（WebSocket 订阅，承载 screenshot/step_progress 等）
    pub ws_tx: broadcast::Sender<String>,
    /// 优雅关闭信号发送端（由 shutdown_app 等触发，通知 launcher 开始优雅关闭流程）
    pub shutdown_tx: watch::Sender<()>,
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
        // 细粒度依赖从容器抽出（trait object 化），handler 不再触达 container
        let history: Arc<dyn HistoryStore> = container.history.clone();
        let login: Arc<dyn LoginApi> = container.login.clone();
        let scheduler: Arc<dyn SchedulerApi> = container.scheduler.clone();
        let tasks: Arc<dyn TaskApi> = container.tasks.clone();
        let task_runner: Arc<dyn TaskRunApi> = container.executor.clone();
        let config: Arc<dyn ConfigApi> = container.config.clone();
        let profiles: Arc<dyn ProfileApi> = container.profiles.clone();
        let bridge: Arc<dyn BridgeApi> = container.bridge.clone();
        let environment: Arc<dyn EnvironmentApi> = container.environment.clone();
        let updater: Arc<dyn UpdaterApi> = container.updater.clone();
        let engine: Arc<dyn EngineApi> = Arc::new(container.engine.clone());
        let metrics = container.metrics.clone();
        let status = container.status.clone();
        Self {
            container,
            history,
            login,
            scheduler,
            tasks,
            task_runner,
            config,
            profiles,
            bridge,
            environment,
            updater,
            engine,
            metrics,
            status,
            log_tx,
            ws_tx,
            shutdown_tx,
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

/// 委派提取：`State<Arc<dyn SchedulerApi>>`（M1 第三域：scheduler）
impl axum::extract::FromRef<AppState> for Arc<dyn SchedulerApi> {
    fn from_ref(state: &AppState) -> Self {
        state.scheduler.clone()
    }
}

/// 委派提取：`State<Arc<dyn TaskApi>>`（M1 第四域：tasks）
impl axum::extract::FromRef<AppState> for Arc<dyn TaskApi> {
    fn from_ref(state: &AppState) -> Self {
        state.tasks.clone()
    }
}

/// 委派提取：`State<Arc<dyn TaskRunApi>>`（M1：tasks 域伴生执行器）
impl axum::extract::FromRef<AppState> for Arc<dyn TaskRunApi> {
    fn from_ref(state: &AppState) -> Self {
        state.task_runner.clone()
    }
}

/// 委派提取：`State<Arc<dyn ConfigApi>>`（M1 第五域：config）
impl axum::extract::FromRef<AppState> for Arc<dyn ConfigApi> {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}

/// 委派提取：`State<Arc<dyn ProfileApi>>`（M1 第六域：profiles）
impl axum::extract::FromRef<AppState> for Arc<dyn ProfileApi> {
    fn from_ref(state: &AppState) -> Self {
        state.profiles.clone()
    }
}

/// 委派提取：`State<Arc<dyn BridgeApi>>`（M1 第七域：bridge）
impl axum::extract::FromRef<AppState> for Arc<dyn BridgeApi> {
    fn from_ref(state: &AppState) -> Self {
        state.bridge.clone()
    }
}

/// 委派提取：`State<Arc<dyn EnvironmentApi>>`（M1 第八域：environment）
impl axum::extract::FromRef<AppState> for Arc<dyn EnvironmentApi> {
    fn from_ref(state: &AppState) -> Self {
        state.environment.clone()
    }
}

/// 委派提取：`State<Arc<dyn UpdaterApi>>`（M1 第九域：updater）
impl axum::extract::FromRef<AppState> for Arc<dyn UpdaterApi> {
    fn from_ref(state: &AppState) -> Self {
        state.updater.clone()
    }
}

/// 委派提取：`State<Arc<dyn EngineApi>>`（M1 第十域：engine）
impl axum::extract::FromRef<AppState> for Arc<dyn EngineApi> {
    fn from_ref(state: &AppState) -> Self {
        state.engine.clone()
    }
}

/// 委派提取：`State<Arc<Metrics>>`（M1：metrics 直字段）
impl axum::extract::FromRef<AppState> for Arc<Metrics> {
    fn from_ref(state: &AppState) -> Self {
        state.metrics.clone()
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
        assert_eq!(
            normalize_source("campus_auth::scheduler::cron_loop"),
            "scheduler"
        );
        assert_eq!(normalize_source("campus_auth::launcher"), "launcher");
        assert_eq!(normalize_source("campus_auth"), "app");
        assert_eq!(normalize_source("hyper_util::client::legacy"), "hyper_util");
        assert_eq!(normalize_source("  campus_auth::bridge::mod  "), "bridge");
        assert_eq!(normalize_source(""), "");
        assert_eq!(normalize_source("   "), "");
    }
}
