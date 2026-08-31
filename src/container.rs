//! ServiceContainer：15 服务拓扑排序构造 + DI 容器
//!
//! 所有长生命周期服务通过 `Arc<T>` 持有，构造顺序按依赖拓扑严格编排。
//! `startup()` 启动后台服务（Engine / Scheduler / Bridge），返回启动句柄供 Launcher 管理生命周期。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bridge::BridgeSupervisor;
use crate::config::{ConfigReloadSignal, ConfigService, ProfileService};
use crate::engine::{Engine, EngineDeps, EngineSlot};
use crate::environment::EnvironmentManager;
use crate::login::{LoginHistoryService, LoginOrchestrator};
use crate::monitor::MonitorService;
use crate::network::detect::create_detector;
use crate::scheduler::SchedulerService;
use crate::status::StatusManager;
use crate::tasks::{TaskExecutor, TaskManager};
use crate::updater::UpdaterService;
use crate::utils::metrics::Metrics;

/// 启动后台服务后返回的句柄集合（由 Launcher 持有并管理生命周期）
pub struct StartupHandles {
    /// 定时任务调度器句柄
    pub scheduler_handle: crate::scheduler::ServiceHandle,
    /// Bridge Supervisor 句柄
    pub bridge_handle: crate::bridge::ServiceHandle,
}

/// 服务容器：持有全部服务的 Arc 句柄
///
/// 构造顺序严格按拓扑排序（15 层），新增服务在此插入。
///
/// 字段设计：服务句柄均为 `pub` 的不可变 `Arc<T>`（DI 容器语义），
/// 外部只能克隆句柄、无法修改内部状态；唯一可变状态 `uptime_cancel` 保持私有。
pub struct ServiceContainer {
    // ---- Layer 1~2：配置 & 轻量服务 ----
    /// 配置服务（settings.json 读写 + ArcSwap 快照）
    pub config: Arc<ConfigService>,
    /// Profile 服务（多档案切换、检测）
    pub profiles: Arc<ProfileService>,
    /// 登录历史持久化
    pub history: Arc<LoginHistoryService>,

    // ---- Layer 3：任务 & 状态 ----
    /// 自定义任务管理器（JSON 驱动）
    pub tasks: Arc<TaskManager>,
    /// 状态管理器（watch 通道推送）
    pub status: Arc<StatusManager>,

    // ---- Layer 4~5：桥接 & 环境 ----
    /// 浏览器桥接器（Playwright + OCR NDJSON IPC）
    pub bridge: Arc<BridgeSupervisor>,
    /// 环境管理器（uv/python 按需安装）
    pub environment: Arc<EnvironmentManager>,

    // ---- Layer 6：执行器 ----
    /// 任务执行器（脚本/Shell 沙箱执行）
    pub executor: Arc<TaskExecutor>,

    // ---- Layer 7：登录编排 ----
    /// 登录编排器（状态机、去重、抢占、重试）
    pub login: Arc<LoginOrchestrator>,

    // ---- Layer 8~9：调度 & 监测 ----
    /// 定时任务调度器
    pub scheduler: Arc<SchedulerService>,
    /// 网络监测服务（TCP/HTTP/URL 探测）
    pub monitor: Arc<MonitorService>,

    // ---- Layer 10：更新器 ----
    /// 版本更新服务
    pub updater: Arc<UpdaterService>,

    // ---- Layer 11：Engine ----
    /// Engine 可替换句柄槽：崩溃重启后由 watch_engine 原子换入新句柄，
    /// Web/托盘/关闭流程经此取「当前活跃」Engine（引用收口）
    pub engine: EngineSlot,

    // ---- 横切关注点：运行指标 ----
    /// 共享运行指标（AtomicU64 计数器，通过 `/api/system/info` 暴露）
    pub metrics: Arc<Metrics>,
    /// uptime 定时器取消令牌（应用级关闭令牌的 child，父令牌取消时自动传播）
    uptime_cancel: CancellationToken,
}

impl ServiceContainer {
    /// 构造服务容器（拓扑排序）
    ///
    /// 按依赖顺序创建 15 个服务，确保被依赖的服务先于依赖方初始化。
    /// `shutdown_token` 为应用级关闭令牌（由 Launcher 持有），容器内据此派生
    /// 登录 shutdown 与 uptime 各自的 child，父令牌取消时自动传播（A3）。
    pub async fn new(
        base_path: &Path,
        shutdown_token: CancellationToken,
    ) -> Result<(Arc<Self>, StartupHandles)> {
        // ---- Layer 0：配置重载信号通道 ----
        let (reload_tx, reload_rx) = mpsc::channel::<ConfigReloadSignal>(32);

        // ---- 横切关注点：运行指标 ----
        let metrics = Metrics::new();

        // ---- Layer 1：ConfigService（唯一 async 构造，直接返回 Arc<Self>）----
        let config = ConfigService::new(base_path.to_path_buf(), reload_tx)
            .await
            .context("初始化 ConfigService 失败")?;

        // ---- Layer 2：无依赖轻量服务 ----
        let profiles = Arc::new(ProfileService::new(config.clone()));
        let history = Arc::new(LoginHistoryService::new(base_path));
        let status = Arc::new(StatusManager::new());

        // ---- Layer 3：持久化 & 状态 ----
        let tasks = TaskManager::new(base_path, config.clone());

        // ---- Layer 4：桥接 & 环境（自返 Arc）----
        let bridge = BridgeSupervisor::new(
            base_path.to_path_buf(),
            config.clone(),
            status.clone(),
            Some(metrics.clone()),
        );
        let git_download_enabled = config.runtime().load().app.developer_mode;
        let environment = EnvironmentManager::new(
            base_path.to_path_buf(),
            status.clone(),
            git_download_enabled,
        );
        // 环境重建成功后复位 Bridge 连续 spawn 失败熔断（B3），解除熔断允许重新 spawn
        {
            let bridge_for_cb = bridge.clone();
            environment
                .set_on_bootstrap_done(Arc::new(move || bridge_for_cb.reset_spawn_failures()));
        }

        // 启动即后台探测环境真实状态：EnvironmentStatus 初始全 false（未就绪），
        // 只有 ensure_capability（登录/任务/调试/手动初始化）才会刷新。此前磁盘上
        // 环境完好时重启程序，/api/init-status 仍报"未就绪"，直到首次使用才纠正。
        // check_environment 仅做只读探测（uv/python --version + 浏览器缓存目录），
        // 不触发下载；失败仅记日志，不阻断启动。
        {
            let env_bg = environment.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::environment::check_environment(&env_bg).await {
                    tracing::warn!("启动环境探测失败: {e}");
                }
            });
        }

        // ---- Layer 5：TaskExecutor（依赖 Bridge + Environment）----
        let executor = TaskExecutor::new(
            base_path,
            status.clone(),
            bridge.clone(),
            environment.clone(),
            config.clone(),
        );

        // ---- Layer 6：网络探测 & 监测（login 前置，供构造注入）----
        let detector = create_detector();
        let monitor = Arc::new(
            MonitorService::new(
                config.clone(),
                detector.clone(),
                None,
                Some(metrics.clone()),
            )
            .context("初始化 MonitorService 失败")?,
        );

        // ---- Layer 7：登录编排器（构造注入全部依赖，无 setter）----
        // 从应用级关闭令牌派生登录 shutdown 的 child：父令牌取消时自动传播，
        // 无需在优雅关闭中单独二次取消（A3）。
        let login_shutdown_token = shutdown_token.child_token();
        let login = Arc::new(LoginOrchestrator::new(
            config.clone(),
            history.clone(),
            status.clone(),
            bridge.clone(),
            environment.clone(),
            tasks.clone(),
            monitor.clone(),
            login_shutdown_token,
            Some(metrics.clone()),
        ));

        // ---- Layer 8：定时任务调度器 ----
        // new() 经 Arc::new_cyclic 直接返回 Arc<Self>（自引用弱句柄，M1）
        let scheduler = SchedulerService::new(
            config.clone(),
            tasks.clone(),
            executor.clone(),
            status.clone(),
            reload_rx,
        )
        .context("初始化 SchedulerService 失败")?;

        // ---- Layer 9：更新器 ----
        let updater = UpdaterService::new(config.clone(), status.clone(), base_path.to_path_buf());

        // 启动时应用待处理更新：改为后台 spawn，不在容器构造内 await——
        // 其内部含 fetch_manifest 网络请求（staging 产物二次校验），网络慢时
        // 会阻塞启动数十秒。self_replace 替换运行中的 exe 后新版本在下次启动生效，
        // 因此运行期后台应用不影响本次进程。
        // 并发说明（F9）：apply_pending_on_startup 与手动 apply_update 统一走
        // update_in_progress 原子标记互斥——后台路径抢不到标记即跳过并记日志，
        // 不会与手动更新并发双写 pending.json / 重复 spawn helper。此处保留
        // 2s 延迟仅用于错开启动初期的资源争抢（Web 服务与配置加载），不再是
        // 并发正确性的依赖。
        // 无论成功失败都继续运行：替换失败时已自动回滚，以当前版本运行，不阻断用户。
        {
            let updater_bg = updater.clone();
            let shutdown_for_update = shutdown_token.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_for_update.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
                tracing::info!("检查待定更新（后台）");
                match updater_bg.apply_pending_on_startup().await {
                    Ok(true) => tracing::info!("待定更新已应用，新版本将在下次启动生效"),
                    Ok(false) => tracing::info!("无待定更新，跳过"),
                    Err(e) => tracing::warn!("待定更新应用失败，继续以当前版本运行: {e}"),
                }
            });
        }

        // ---- Layer 10：自启动服务（检查平台支持）----
        // AutoStartService / DebugSessionManager / TaskRegistry / WebSocketManager
        // 在当前版本中尚未独立实现，相关功能由 TrayManager / Scheduler / Bridge 内聚处理。

        // ---- Layer 11：Engine（聚合全部服务）----
        let engine_deps = EngineDeps {
            config_service: config.clone(),
            profile_service: profiles.clone(),
            orchestrator: login.clone(),
            status_manager: status.clone(),
            monitor_service: monitor.clone(),
            network_detect: detector,
        };
        let engine_handle = Engine::spawn(engine_deps);

        // ---- 组装容器 ----
        let container = Arc::new(Self {
            config,
            profiles,
            history,
            tasks,
            status,
            bridge,
            environment,
            executor,
            login,
            scheduler,
            monitor,
            updater,
            engine: EngineSlot::new(engine_handle),
            metrics,
            // uptime 定时器取消信号：应用级关闭令牌的 child
            uptime_cancel: shutdown_token.child_token(),
        });

        // ---- 启动运行时长更新任务 ----
        // 每秒周期更新：既写入 Metrics（/api/system 数据源），也通过 PartialSnapshot::Uptime
        // 推送状态快照，保证 WebSocket 状态里的 uptime_seconds 与 /api/system 保持一致。
        let start_time = std::time::Instant::now();
        let cancel_for_task = container.uptime_cancel.clone();
        let metrics_for_uptime = container.metrics.clone();
        let status_for_uptime = container.status.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_for_task.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                        let secs = start_time.elapsed().as_secs();
                        metrics_for_uptime.set_uptime(secs);
                        status_for_uptime.merge(crate::status::PartialSnapshot::Uptime(secs));
                    }
                }
            }
        });

        // ---- 启动后台服务 ----
        let scheduler_handle = container.startup().await?;

        Ok((container, scheduler_handle))
    }

    /// 启动后台服务：Scheduler + Bridge Supervisor
    ///
    /// Engine 已在 `new()` 中通过 `Engine::spawn` 启动。
    /// 返回句柄由 Launcher 持有，优雅关闭时调用。
    async fn startup(self: &Arc<Self>) -> Result<StartupHandles> {
        // 启动定时任务调度器
        let scheduler_handle = self.scheduler.clone().start().await;

        // 启动 Bridge Supervisor（懒加载 Worker，仅启动 supervisor 主循环）
        let bridge_handle = self.bridge.spawn();

        tracing::info!("后台服务已启动（定时调度器 + Bridge）");
        Ok(StartupHandles {
            scheduler_handle,
            bridge_handle,
        })
    }
}
