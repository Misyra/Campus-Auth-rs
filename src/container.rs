//! ServiceContainer：16 字段拓扑排序构造 + DI 容器（13 服务 + EventBus/Metrics/uptime 3 横切）
//!
//! 所有长生命周期服务通过 `Arc<T>` 持有，构造顺序按依赖拓扑严格编排。
//! `startup()` 启动后台服务（EventSink / Scheduler / Bridge），返回启动句柄供 Launcher 管理生命周期。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bridge::BridgeSupervisor;
use crate::config::{ConfigReloadSignal, ConfigService, ProfileService};
use crate::engine::{Engine, EngineDeps, EngineSlot};
use crate::environment::EnvironmentManager;
use crate::events::EventBus;
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
    /// 事件流落盘任务句柄（同时受应用关闭令牌驱动：令牌取消即排空退出，
    /// Launcher 无需显式 stop；句柄保留供测试与后续源头接入方显式停用）
    pub event_sink: crate::ServiceHandle,
    /// 推送通知服务句柄（订阅 EventBus 按配置过滤投递；发送失败静默，
    /// 绝不影响登录主链路；句柄交 Launcher 随应用关闭统一 stop）
    pub notification: crate::ServiceHandle,
}

/// 服务容器：持有全部服务的 Arc 句柄
///
/// 构造顺序严格按拓扑排序（16 字段），新增服务在此插入。
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
    /// 任务执行器（脚本沙箱执行）
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

    // ---- 横切关注点：事件总线 & 运行指标 ----
    /// 事件总线（类型化发布/订阅；源头接入与消费方见 `src/events/` 模块 doc，
    /// JSONL 落盘订阅者由 `startup()` 启动的 EventSink 承担）
    pub events: Arc<EventBus>,
    /// 共享运行指标（AtomicU64 计数器，通过 `/api/system/info` 暴露）
    pub metrics: Arc<Metrics>,
    /// uptime 定时器取消令牌（应用级关闭令牌的 child，父令牌取消时自动传播）
    uptime_cancel: CancellationToken,
}

impl ServiceContainer {
    /// 构造服务容器（拓扑排序）
    ///
    /// 按依赖顺序创建 16 个组件（13 服务 + 3 横切），确保被依赖的服务先于依赖方初始化。
    /// `shutdown_token` 为应用级关闭令牌（由 Launcher 持有），容器内据此派生
    /// 登录 shutdown 与 uptime 各自的 child，父令牌取消时自动传播（A3）。
    pub async fn new(
        base_path: &Path,
        shutdown_token: CancellationToken,
    ) -> Result<(Arc<Self>, StartupHandles)> {
        // ---- Layer 0：横切通道 & 信号 ----
        // 事件总线最先构造：登录 / 监测 / 任务 / 更新等所有潜在发布方都排在它之后，
        // 消费方（通知 / 诊断 / Dashboard）与发布方互不感知，仅经总线解耦
        let events = Arc::new(EventBus::new());
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
        let tasks = TaskManager::new(base_path);

        // ---- Layer 4：桥接 & 环境（自返 Arc）----
        let bridge = BridgeSupervisor::new(
            base_path.to_path_buf(),
            config.clone(),
            status.clone(),
            Some(metrics.clone()),
        );
        let environment = EnvironmentManager::new_with_events(
            base_path.to_path_buf(),
            status.clone(),
            // 引导完成（fire_bootstrap_done）发布 EnvironmentReady 的事件源
            events.clone(),
        );
        // 环境重建成功后复位 Bridge 连续 spawn 失败熔断（B3），解除熔断允许重新 spawn。
        Self::wire_environment_bridge(&bridge, &environment);

        // 启动即后台探测环境真实状态（只读，不触发下载，不阻断启动）。
        Self::spawn_environment_probe(&environment, &shutdown_token);

        // ---- Layer 5：TaskExecutor（依赖 Bridge + Environment + EventBus）----
        // 任务终态（TaskFinished / TaskFailed）经 execute / execute_with_timeout_override
        // 统一入口发布，手动执行与定时调度两条触发共用同一收敛点
        let executor = TaskExecutor::new_with_events(
            base_path,
            status.clone(),
            bridge.clone(),
            environment.clone(),
            config.clone(),
            events.clone(),
        );

        // ---- Layer 6：网络探测 & 监测（login 前置，供构造注入）----
        let detector = create_detector();
        let monitor = Arc::new(
            MonitorService::new(config.clone(), detector.clone(), Some(metrics.clone()))
                .context("初始化 MonitorService 失败")?,
        );

        // ---- Layer 7：登录编排器（构造注入全部依赖，无 setter）----
        // 从应用级关闭令牌派生登录 shutdown 的 child：父令牌取消时自动传播，
        // 无需在优雅关闭中单独二次取消（A3）。
        let login_shutdown_token = shutdown_token.child_token();
        let login = Arc::new(LoginOrchestrator::new_with_events(
            config.clone(),
            history.clone(),
            status.clone(),
            bridge.clone(),
            environment.clone(),
            tasks.clone(),
            // 脚本登录渠道复用同一个执行器实例（与任务页「立即运行」同一条路径，
            // 连"同任务串行"的执行锁都是同一把）
            executor.clone(),
            monitor.clone(),
            login_shutdown_token,
            Some(metrics.clone()),
            events.clone(),
        ));

        // ---- Layer 8：定时任务调度器 ----
        // new() 经 Arc::new_cyclic 直接返回 Arc<Self>（自引用弱句柄，M1）；
        // 任务执行终态由 executor 统一入口发布，调度域仅在目标任务加载失败时补发
        let scheduler = SchedulerService::new(
            config.clone(),
            tasks.clone(),
            executor.clone(),
            status.clone(),
            reload_rx,
            events.clone(),
        )
        .context("初始化 SchedulerService 失败")?;

        // ---- Layer 9：更新器 ----
        // 检查命中新版本（手动 + 后台自动两条路径）发布 UpdateAvailable 的事件源
        let updater = UpdaterService::new_with_events(
            config.clone(),
            status.clone(),
            base_path.to_path_buf(),
            events.clone(),
        );

        // 启动时应用待处理更新：后台 spawn，不在构造内 await（内含网络请求，慢网会阻塞启动）。
        // 并发说明（F9）：与手动 apply_update 统一走 update_in_progress 原子标记互斥，
        // 后台路径抢不到标记即跳过并记日志；2s 延迟仅错开启动初期资源争抢。
        Self::spawn_pending_update_check(&updater, &shutdown_token);

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
            // 网络状态翻转（NetworkDown/NetworkUp）与 Profile 切换（ProfileSwitched）
            // 的事件发布源
            events: events.clone(),
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
            events,
            metrics,
            // uptime 定时器取消信号：应用级关闭令牌的 child
            uptime_cancel: shutdown_token.child_token(),
        });

        Self::spawn_uptime_tracker(&container);
        Self::spawn_log_cleanup_task(&container);

        // ---- 启动后台服务 ----
        let scheduler_handle = container.startup(&shutdown_token).await?;

        Ok((container, scheduler_handle))
    }

    /// 接线环境引导完成回调：环境重建成功后复位 Bridge 熔断（B3）
    fn wire_environment_bridge(
        bridge: &Arc<BridgeSupervisor>,
        environment: &Arc<EnvironmentManager>,
    ) {
        bridge.set_environment(environment);
        let bridge_for_cb = bridge.clone();
        environment.set_on_bootstrap_done(Arc::new(move || bridge_for_cb.reset_spawn_failures()));
    }

    /// 后台只读探测环境真实状态（uv/python --version + 浏览器缓存目录）
    ///
    /// `EnvironmentStatus` 初始全 false，磁盘完好时重启后需纠正 `/api/init-status`，
    /// 失败仅记日志，不阻断启动，不触发下载。
    ///
    /// 与 [`Self::spawn_pending_update_check`] 同构地接受关闭令牌（P2-7）：无令牌时
    /// 停机后该任务仍持 `Arc<EnvironmentManager>` 继续写状态/日志、可能 spawn uv 子
    /// 进程，与重启后的继任进程争缓存锁。
    fn spawn_environment_probe(
        environment: &Arc<EnvironmentManager>,
        shutdown_token: &CancellationToken,
    ) {
        // OpenWrt / 安卓构建不引导 uv/Python 环境，后台探测无从谈起
        if cfg!(any(feature = "openwrt", feature = "android")) {
            return;
        }
        let env_bg = environment.clone();
        let shutdown_for_probe = shutdown_token.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                _ = shutdown_for_probe.cancelled() => return,
                r = env_bg.refresh_status() => r,
            };
            if let Err(e) = result {
                tracing::warn!("启动环境探测失败: {e}");
            }
        });
    }

    /// 后台应用待定更新（2s 延迟错峰启动资源争抢，失败回滚继续以当前版本运行）
    fn spawn_pending_update_check(
        updater: &Arc<UpdaterService>,
        shutdown_token: &CancellationToken,
    ) {
        let updater_bg = updater.clone();
        let shutdown_for_update = shutdown_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_for_update.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
            }
            match updater_bg.apply_pending_on_startup().await {
                Ok(true) => tracing::info!("待定更新已应用，新版本将在下次启动生效"),
                // 无待定更新是常态，与"后台检查"合并为一条 debug 避免每次启动两条噪音
                Ok(false) => tracing::debug!("后台检查待定更新：无待定更新，跳过"),
                Err(e) => tracing::warn!("待定更新应用失败，继续以当前版本运行: {e}"),
            }
        });
    }

    /// 每秒更新运行时长：写入 Metrics 并推送 `PartialSnapshot::Uptime`，保证两处一致
    fn spawn_uptime_tracker(container: &Arc<Self>) {
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
    }

    /// 每日兜底清理过期日志与 `.trash` 回滚区：启动时的 `cleanup_old_logs`
    /// 只执行一次，关闭"自动重启"的长驻进程会让轮转文件无限累积。保留天数
    /// 每次从配置快照读取，跟随热更新。
    fn spawn_log_cleanup_task(container: &Arc<Self>) {
        let cancel_for_task = container.uptime_cancel.child_token();
        let config_for_task = container.config.clone();
        let history_for_task = container.history.clone();
        tokio::spawn(async move {
            // 启动即执行一次 .trash 轮转：重启频繁的实例 24h tick 可能长期不
            // 触发，而 .trash 此前无任何清理点（含 ENC: 密文方案文件）；30 天
            // mtime 门槛保证首启清理同样安全。日志/历史/事件流的启动清理另有
            // 入口（init_logging / sink 任务），此处只补 .trash 这一块
            Self::cleanup_trash_entries(&config_for_task.base_path()).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(86_400));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // 首个 tick 立即完成：启动时的清理已在 init_logging 做过，跳过
            interval.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_for_task.cancelled() => break,
                    _ = interval.tick() => {
                        // .trash 回滚区按 mtime 轮转 30 天（启动已执行一次，
                        // 这里管长驻实例的跨日兜底）
                        Self::cleanup_trash_entries(&config_for_task.base_path()).await;
                        let retention = config_for_task.runtime().load().logging.retention_days;
                        // 日志目录的扫描+删除为同步 IO：包 spawn_blocking，
                        // 避免在每日 async tick 内阻塞 tokio worker（文件量随
                        // 运行天数累积，单次可达数百个）
                        let base_for_logs = config_for_task.base_path();
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            crate::logging::cleanup_expired_logs(&base_for_logs, retention)
                        })
                        .await
                        {
                            tracing::warn!("过期日志清理任务异常: {e}");
                        }
                        // LOG-2：登录历史保留固定 30 天（与 Web 查询窗口对齐），
                        // 更早的 JSONL 无消费路径，随每日 housekeeping 删除
                        match history_for_task
                            .clear_older_than(crate::login::HISTORY_RETENTION_DAYS)
                            .await
                        {
                            Ok(n) if n > 0 => {
                                tracing::info!("清理过期登录历史文件 {} 个", n);
                            }
                            Ok(_) => {}
                            Err(e) => tracing::debug!("清理登录历史失败: {e}"),
                        }
                        // 事件流与登录历史同口径固定 30 天（EVENT_RETENTION_DAYS）：
                        // 启动清理已在 sink 任务内做过，这里只管跨日兜底；扫描
                        // 删除同为同步 IO，同包 spawn_blocking
                        let base_for_events = config_for_task.base_path();
                        let expired_events = tokio::task::spawn_blocking(move || {
                            crate::events::sink::cleanup_expired_event_files(
                                &crate::utils::paths::events_dir(&base_for_events),
                                crate::events::sink::EVENT_RETENTION_DAYS,
                            )
                        })
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!("过期事件流清理任务异常: {e}");
                            0
                        });
                        if expired_events > 0 {
                            tracing::info!("清理过期事件流文件 {expired_events} 个");
                        }
                    }
                }
            }
        });
    }

    /// 清理 `.trash` 回滚区过期条目（启动 + 每日兜底共用），返回删除数量
    ///
    /// 删除方案移入 `config/.trash/`（含 ENC: 密文方案文件）后此前无任何清理
    /// 点，长驻实例无限累积；现按文件 mtime 墙钟轮转 30 天
    /// （`config::service::TRASH_RETENTION_DAYS`）。卸载口径随 `config/` 整目录
    /// 继承：purge 时一并删除、`keep_user_data` 时一并保留。
    /// 目录扫描+删除为同步 IO，整体包 `spawn_blocking`（闭包拿 `base_path`
    /// 克隆），避免在 async tick 内阻塞 tokio worker。
    async fn cleanup_trash_entries(base_path: &std::path::Path) -> usize {
        let base = base_path.to_path_buf();
        let removed = tokio::task::spawn_blocking(move || {
            let trash_dir = crate::utils::paths::config_dir(&base).join(crate::config::TRASH_DIR);
            crate::config::service::cleanup_expired_trash_entries(
                &trash_dir,
                crate::config::service::TRASH_RETENTION_DAYS,
                std::time::SystemTime::now(),
            )
        })
        .await
        .unwrap_or_else(|e| {
            // JoinError 仅内部 panic 可达：清理为兜底任务，失败留待下一轮重试
            tracing::warn!("清理 .trash 任务异常: {e}");
            0
        });
        if removed > 0 {
            tracing::info!("清理 .trash 过期回滚条目 {removed} 个");
        }
        removed
    }

    /// 启动后台服务：EventSink + 推送通知 + Scheduler + Bridge Supervisor
    ///
    /// Engine 已在 `new()` 中通过 `Engine::spawn` 启动。
    /// 返回句柄由 Launcher 持有，优雅关闭时调用。
    async fn startup(
        self: &Arc<Self>,
        shutdown_token: &CancellationToken,
    ) -> Result<StartupHandles> {
        // 启动事件流落盘（订阅 EventBus；关闭令牌 + stop 双驱动，退出前排空并 flush。
        // 句柄经 StartupHandles 交给 Launcher 持有——直接 drop 会停用 stop 信号源）
        let event_sink = crate::events::spawn_event_sink(
            self.config.base_path(),
            self.events.clone(),
            shutdown_token.child_token(),
        );

        // 启动推送通知服务（订阅 EventBus，按配置的事件集合与免打扰时段过滤投递；
        // 全局开关关闭或通道为空时空转，仅周期重读配置，发送失败静默不影响主链路）。
        // NotificationConfigSource 实现挂在 Arc<dyn ConfigApi> 包装类型上，
        // 故先归一为 dyn 再包一层 Arc 以得到 Arc<dyn NotificationConfigSource>
        let config_api: std::sync::Arc<dyn crate::config::ConfigApi> = self.config.clone();
        let notification = crate::notification::spawn_notification_service(
            self.events.clone(),
            std::sync::Arc::new(config_api),
            self.config.base_path(),
        );

        // 启动定时任务调度器
        let scheduler_handle = self.scheduler.clone().start().await;

        // 启动 Bridge Supervisor（懒加载 Worker，仅启动 supervisor 主循环）
        let bridge_handle = self.bridge.spawn();

        tracing::info!("后台服务已启动（事件流落盘 + 推送通知 + 定时调度器 + Bridge）");
        Ok(StartupHandles {
            scheduler_handle,
            bridge_handle,
            event_sink,
            notification,
        })
    }
}
