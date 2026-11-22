//! Engine 崩溃恢复：监控 Engine task 退出，按上限自动重启并恢复监测状态（含手动暂停）

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::LauncherState;
use crate::config::{ConfigService, ProfileService};
use crate::engine::{EngineDeps, EngineSlot, MAX_RESTART_ATTEMPTS, RESTART_DELAY_SECS};
use crate::events::EventBus;
use crate::login::LoginOrchestrator;
use crate::monitor::MonitorService;
use crate::network::detect::NetworkDetect;
use crate::status::StatusManager;

/// 崩溃恢复所需依赖子集（从 [`ServiceContainer`](crate::container::ServiceContainer)
/// 提取；测试可不经全容器直接组装）
struct RecoveryDeps {
    /// 状态管理器（崩溃前快照的读取来源）
    status: Arc<StatusManager>,
    /// 配置服务（新 Engine 构造 + 定时暂停窗口判定的读取来源）
    config: Arc<ConfigService>,
    /// Profile 服务（新 Engine 构造）
    profile_service: Arc<ProfileService>,
    /// 登录编排器（重启时取消 source=auto 的在途登录；新 Engine 构造）
    orchestrator: Arc<LoginOrchestrator>,
    /// 网络监测服务（新 Engine 构造）
    monitor_service: Arc<MonitorService>,
    /// 事件总线（重启实例与初始实例共用同一总线）
    events: Arc<EventBus>,
    /// 网络检测器（新 Engine 构造）
    network_detect: Arc<dyn NetworkDetect>,
    /// Engine 可替换句柄槽（重启后原子换入新句柄）
    slot: EngineSlot,
    /// 应用级关闭令牌（关闭流程取消时停止重启循环）
    shutdown: CancellationToken,
}

/// 监控 Engine task，崩溃后最多重启 `MAX_RESTART_ATTEMPTS` 次
///
/// 完成检测：`EngineHandle.completed`（CancellationToken）在 Engine 退出（含
/// panic，经 CompletionGuard 的 Drop 触发）时取消，等待者零延迟唤醒。
///
/// 引用收口（todo 7.3 中期方案落地）：重启产生的新句柄经
/// `EngineSlot::replace()` 原子换入，Web/托盘/关闭流程
/// 经 slot 取当前活跃 Engine，不再持有已死引用。
///
/// 状态恢复：崩溃前若监测处于 Running，重启完成后按原状态重发 Start；
/// 崩溃前若处于暂停态（快照 pause_active），Start 后补发 Pause 恢复暂停
/// （P3-11，详见 [`scheduled_pause_window_active`] 的取舍说明），消除
/// 「崩溃自愈后 Engine 以 monitoring=false 空转、监测静默失效」与
/// 「用户手动暂停被崩溃吞掉、自动登录静默恢复」两类问题。
///
/// panic 与正常退出不再区分：Engine 正常退出的唯一路径是收到 Shutdown
/// 命令，而 Shutdown 仅在应用级关闭令牌取消后发送——此时 biased select
/// 先命中 cancelled 分支返回，不进入重启循环。token 未取消时的任何退出
/// 均按崩溃处理。
pub(super) fn watch_engine(state: &LauncherState) -> JoinHandle<()> {
    // 容器缺失属逻辑不变量违反，防御性降级为 no-op 任务，避免 panic（历史遗留 #11）
    let Some(container) = state.container.as_ref() else {
        return tokio::spawn(async {});
    };
    spawn_recovery_watcher(RecoveryDeps {
        status: container.status.clone(),
        config: container.config.clone(),
        profile_service: container.profiles.clone(),
        orchestrator: container.login.clone(),
        monitor_service: container.monitor.clone(),
        events: container.events.clone(),
        // 检测器随重启实例构造一次（与初始实例同一实现，不复用旧 Arc：
        // 检测器内部无状态，重建无代价）
        network_detect: crate::network::detect::create_detector(),
        slot: container.engine.clone(),
        shutdown: state.shutdown_token.clone(),
    })
}

/// 崩溃恢复主循环（从 [`watch_engine`] 抽出，依赖经 [`RecoveryDeps`] 注入）
fn spawn_recovery_watcher(deps: RecoveryDeps) -> JoinHandle<()> {
    let RecoveryDeps {
        status,
        config,
        profile_service,
        orchestrator,
        monitor_service,
        events,
        network_detect,
        slot,
        shutdown: shutdown_token,
    } = deps;

    tokio::spawn(async move {
        // 等待初始 Engine 完成（completed 在正常退出与 panic 时均触发）。
        // 同时监听 shutdown 信号，避免应用关闭时持续阻塞在此等待。
        let initial_completed = slot.current_handle().map(|h| h.completed.clone());
        if let Some(token) = initial_completed {
            tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                _ = token.cancelled() => {}
            }
        }

        // 应用正在关闭（graceful_shutdown 会取消令牌）：初始 Engine 的退出
        // 属预期行为，直接返回，绝不重启
        if shutdown_token.is_cancelled() {
            return;
        }

        // 首次（初始 Engine）非正常退出：重启循环内的后续退出各有 error 留痕，
        // 此处补首崩记录，避免「只在重启 info 中隐含崩溃」导致误判为正常重启
        error!("Engine 异常退出（疑似崩溃），进入重启流程");

        let mut restart_count: u32 = 0;

        loop {
            // 应用关闭：停止重启循环
            if shutdown_token.is_cancelled() {
                return;
            }

            // 捕获崩溃前状态（Engine 崩溃后快照停留在最后一次合并值，正是恢复
            // 依据）：Running 表示监测中，重启后需恢复；pause_active 表示监测
            // 处于暂停态（手动暂停与定时窗口的合并值，取舍见下方恢复分支）
            let snap = status.borrow();
            let was_monitoring = snap.engine_state == crate::status::EngineState::Running;
            let was_paused = snap.pause_active;
            drop(snap);

            // 通知 Orchestrator 取消 source=auto 的在途登录（reason 进用户可见的
            // 取消文案，须为中文而非英文标识符）
            orchestrator
                .cancel_auto_pending("引擎异常重启，自动登录已中止")
                .await;

            restart_count += 1;
            if restart_count > MAX_RESTART_ATTEMPTS {
                error!("Engine 重启次数耗尽（{MAX_RESTART_ATTEMPTS} 次），标记为 Dead");
                // 清空 slot，后续命令派发按 ChannelClosed 快速失败
                slot.clear();
                // 仅 state 置 Dead，其余字段回填崩溃前快照：避免用 Unknown/默认值
                // 覆盖最后的真实网络结论；last_check 沿用旧值（非检测事件不刷新，G3）
                let snap = status.borrow();
                status.merge(crate::status::PartialSnapshot::Engine {
                    state: crate::status::EngineState::Dead,
                    network: snap.network_status,
                    assessment: snap.connectivity.clone(),
                    evidence: snap.last_probe_evidence.clone(),
                    last_check: snap.last_check_time.unwrap_or_else(chrono::Local::now),
                    pause: snap.pause_active,
                    cooling_down: snap.cooling_down,
                    cooling_down_remaining: snap.cooling_down_remaining,
                    consecutive_failures: snap.consecutive_failures,
                });
                return;
            }

            info!(
                attempt = restart_count,
                max = MAX_RESTART_ATTEMPTS,
                "Engine 重启中（立即换入新实例，{}s 冷却后恢复检测）...",
                RESTART_DELAY_SECS
            );

            // ENG-1：先 spawn 新 Engine 并原子换入 slot，再做冷却等待——
            // 原顺序（先 sleep 后 spawn）会在 5s 窗口内让 slot 持已死句柄，
            // Web/托盘所有命令派发得到 ChannelClosed。新 Engine 处于 Stopped
            // 态即可正常接收命令；冷却 sleep 仅延后「恢复监测」时机，保留
            // 崩溃循环限速语义（连续崩溃仍以 ≥5s 间隔消耗重启次数）。
            // 从 ConfigService 重新加载配置，内部状态重置为默认值
            let deps = EngineDeps {
                config_service: config.clone(),
                profile_service: profile_service.clone(),
                orchestrator: orchestrator.clone(),
                status_manager: status.clone(),
                monitor_service: monitor_service.clone(),
                network_detect: network_detect.clone(),
                // 重启实例与初始实例共用同一事件总线（容器横切持有）
                events: events.clone(),
            };
            let new_handle = crate::engine::Engine::spawn(deps);
            let new_completed = new_handle.completed.clone();

            // 引用收口：新句柄原子换入 slot，Web/托盘/关闭流程即刻指向新 Engine
            slot.replace(new_handle);

            // 等待重启冷却，期间若收到 shutdown 则提前退出
            tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS)) => {}
            }

            // 按崩溃前状态恢复监测（monitoring=false 空转会让用户以为还在监测）
            if was_monitoring {
                match slot.dispatch(crate::engine::EngineCommand::Start).await {
                    Ok(()) => info!("Engine（重启 #{restart_count}）已按崩溃前状态恢复检测"),
                    Err(e) => warn!("Engine（重启 #{restart_count}）恢复检测失败: {e:?}"),
                }
                // P3-11：崩溃前处于暂停态（快照 pause_active）时补发 Pause——
                // 用户显式的手动暂停不得被崩溃吞掉，否则重启后自动登录静默恢复。
                // 快照的 pause 合并了手动暂停与定时窗口，无法逐项区分，取舍为：
                // 恢复时刻定时窗口生效时不补发（新 Engine 自身的
                // is_any_pause_active 门控已覆盖暂停语义）——否则窗口结束后
                // manual_paused 残留，常见夜间崩溃会把检测永久钉死在暂停态，
                // 只能等用户手动 Resume；代价是「手动暂停 + 恰在定时窗口内
                // 崩溃」的窄场景丢一次暂停恢复（窗口结束后多跑一次检测）
                if was_paused && !scheduled_pause_window_active(&config) {
                    match slot.dispatch(crate::engine::EngineCommand::Pause).await {
                        Ok(()) => {
                            info!("Engine（重启 #{restart_count}）已按崩溃前暂停态恢复暂停")
                        }
                        Err(e) => warn!("Engine（重启 #{restart_count}）恢复暂停失败: {e:?}"),
                    }
                }
            }

            // 等待新 Engine 完成；shutdown 优先（正常关闭路径，不重启）
            tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                _ = new_completed.cancelled() => {}
            }
            error!("Engine（重启 #{restart_count}）退出，继续尝试重启");
        }
    })
}

/// 定时暂停窗口此刻是否生效（P3-11 恢复判据）
///
/// 判定口径与 Engine 内部 `is_scheduled_pause_active`（run_loop.rs）逐条一致：
/// `pause.enabled` 总开关 + 当前本地时刻落入窗口。窗口 containment 复用
/// [`crate::notification::filter::in_quiet_window`]（start==end 视为全天、
/// 同天半开区间、跨天取并集），与 engine 的 `is_in_pause_window` 语义相同，
/// 避免两份窗口实现各自演化。窗口字段越界（NaiveTime 构造失败）按未生效
/// 处理：恢复侧宁可多恢复一次检测，也不误判为「窗口生效」而吞掉手动暂停。
fn scheduled_pause_window_active(config: &Arc<ConfigService>) -> bool {
    let cfg = config.runtime().load();
    if !cfg.pause.enabled {
        return false;
    }
    let to_time = |h: u8, m: u8| chrono::NaiveTime::from_hms_opt(h as u32, m as u32, 0);
    match (
        to_time(cfg.pause.start_hour, cfg.pause.start_minute),
        to_time(cfg.pause.end_hour, cfg.pause.end_minute),
    ) {
        (Some(start), Some(end)) => {
            crate::notification::filter::in_quiet_window(chrono::Local::now().time(), start, end)
        }
        (None, _) | (_, None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigReloadSignal;
    use crate::engine::{Engine, EngineCommand};
    use crate::events::EventBus;
    use crate::login::LoginHistoryService;
    use crate::network::detect::{InterfaceInfo, NetworkError};
    use crate::status::{EngineState, StatusManager};
    use crate::tasks::{TaskExecutor, TaskManager};
    use crate::utils::metrics::Metrics;
    use std::time::Duration;

    /// 良性网络检测器：全部立即返回空结果，保证恢复流程测试不做任何真实
    /// 网络 IO（探测路径立即完成，不挂起、不外连）
    struct BenignDetect;

    #[async_trait::async_trait]
    impl NetworkDetect for BenignDetect {
        async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
            Ok(vec![])
        }
        async fn default_gateways(&self) -> Result<Vec<std::net::Ipv4Addr>, NetworkError> {
            Ok(vec![])
        }
        async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
            Ok(None)
        }
    }

    /// 组装崩溃恢复测试依赖（真实服务 + 良性检测器 + 显式关闭定时暂停窗口）
    ///
    /// 监测探测全部禁用、auth_url 指向本机无监听端口（连接立即被拒），
    /// 恢复流程触发的检测不产生任何外部网络依赖；`pause.enabled` 显式关闭
    /// ——PauseSettings 默认启用（23-6 点），夜间跑测试会让
    /// [`scheduled_pause_window_active`] 命中窗口、补发 Pause 分支被跳过，
    /// 用例不得依赖墙钟时间（与 engine 测试同口径）。
    async fn make_recovery_deps() -> (
        tempfile::TempDir,
        RecoveryDeps,
        Arc<StatusManager>,
        EngineSlot,
        CancellationToken,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel::<ConfigReloadSignal>(8);
        let config = ConfigService::new(base.clone(), reload_tx).await.unwrap();

        // 探测全禁用 + 周期调大：恢复流程中的 Start 触发的检测即刻完成且
        // 不外连；auth_url 指向本机 9 号端口（无监听，连接立即被拒），
        // 避开「Browser 渠道且 auth_url 为空」的重定向兜底分支做真实请求
        let mut settings = config.load_settings();
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.http_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.check_interval = 3600;
        settings.global.pause.enabled = false;
        config.save_settings(&settings).await.unwrap();
        config.reload().await.unwrap();
        let profiles = Arc::new(ProfileService::new(config.clone()));
        let mut profile = profiles.get_profile("default").unwrap();
        profile.auth_url = "http://127.0.0.1:9/login".to_string();
        profile.trigger_url = String::new();
        profiles
            .update_profile("default", profile, false)
            .await
            .unwrap();

        let status = Arc::new(StatusManager::new());
        let metrics = Metrics::new();
        let monitor = Arc::new(
            MonitorService::new(
                config.clone(),
                Arc::new(BenignDetect),
                Some(metrics.clone()),
            )
            .unwrap(),
        );
        let history = Arc::new(LoginHistoryService::new(&base));
        let bridge = crate::bridge::BridgeSupervisor::new(
            base.clone(),
            config.clone(),
            status.clone(),
            Some(metrics.clone()),
        );
        let environment = crate::environment::EnvironmentManager::new(base.clone(), status.clone());
        let tasks = TaskManager::new(&base);
        let script_runner = TaskExecutor::new(
            &base,
            status.clone(),
            bridge.clone(),
            environment.clone(),
            config.clone(),
        );
        let orchestrator = Arc::new(crate::login::LoginOrchestrator::new(
            config.clone(),
            history,
            status.clone(),
            bridge,
            environment,
            tasks,
            script_runner,
            monitor.clone(),
            CancellationToken::new(),
            Some(metrics),
        ));

        let engine_deps = EngineDeps {
            config_service: config.clone(),
            profile_service: profiles.clone(),
            orchestrator: orchestrator.clone(),
            status_manager: status.clone(),
            monitor_service: monitor.clone(),
            network_detect: Arc::new(BenignDetect),
            events: Arc::new(EventBus::new()),
        };
        let initial = Engine::spawn(engine_deps);
        let slot = EngineSlot::new(initial);
        let shutdown = CancellationToken::new();

        let deps = RecoveryDeps {
            status: status.clone(),
            config,
            profile_service: profiles,
            orchestrator,
            monitor_service: monitor,
            events: Arc::new(EventBus::new()),
            network_detect: Arc::new(BenignDetect),
            slot: slot.clone(),
            shutdown: shutdown.clone(),
        };
        (tmp, deps, status, slot, shutdown)
    }

    /// 轮询等待条件成立（虚拟时钟下 1.5s 预算）
    async fn wait_for(cond: impl Fn() -> bool) {
        for _ in 0..1_500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("等待条件超时（虚拟时钟 1.5s 内未满足）");
    }

    /// P3-11：暂停态 → 模拟 Engine 崩溃重启 → 断言恢复后仍暂停
    #[tokio::test(start_paused = true)]
    async fn test_recovery_restores_manual_pause_after_crash() {
        let (_tmp, deps, status, slot, shutdown) = make_recovery_deps().await;
        let _watcher = spawn_recovery_watcher(deps);

        // 崩溃前：Start 进入监测 → Pause 手动暂停
        slot.dispatch(EngineCommand::Start).await.unwrap();
        wait_for(|| status.borrow().engine_state == EngineState::Running).await;
        slot.dispatch(EngineCommand::Pause).await.unwrap();
        wait_for(|| status.borrow().pause_active).await;

        // 模拟崩溃：直接取消初始 Engine 的完成令牌（panic 时 CompletionGuard
        // 的 Drop 就是这个效果）；task 本体不再动，快照停留在 Running+暂停
        let initial = slot.current_handle().unwrap();
        initial.completed.cancel();

        // 恢复断言：新 Engine 换入（句柄指针变化）且状态收敛回「运行中 + 暂停」
        wait_for(|| {
            let new_engine = slot.current_engine();
            new_engine.is_some_and(|e| !Arc::ptr_eq(&e, &initial.engine))
        })
        .await;
        wait_for(|| {
            let snap = status.borrow();
            snap.engine_state == EngineState::Running && snap.pause_active
        })
        .await;

        // 清场：关闭令牌取消 → 恢复循环退出（task 随测试 runtime 结束回收）
        shutdown.cancel();
    }

    /// 对照组：崩溃前未暂停 → 恢复后不误入暂停态
    #[tokio::test(start_paused = true)]
    async fn test_recovery_without_pause_stays_unpaused() {
        let (_tmp, deps, status, slot, shutdown) = make_recovery_deps().await;
        let _watcher = spawn_recovery_watcher(deps);

        slot.dispatch(EngineCommand::Start).await.unwrap();
        wait_for(|| status.borrow().engine_state == EngineState::Running).await;

        let initial = slot.current_handle().unwrap();
        initial.completed.cancel();

        wait_for(|| {
            let new_engine = slot.current_engine();
            new_engine.is_some_and(|e| !Arc::ptr_eq(&e, &initial.engine))
        })
        .await;
        wait_for(|| {
            let snap = status.borrow();
            snap.engine_state == EngineState::Running && !snap.pause_active
        })
        .await;

        shutdown.cancel();
    }
}
