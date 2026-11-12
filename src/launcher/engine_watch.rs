//! Engine 崩溃恢复：监控 Engine task 退出，按上限自动重启并恢复监测状态

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use super::LauncherState;
use crate::engine::{EngineDeps, MAX_RESTART_ATTEMPTS, RESTART_DELAY_SECS};

/// 监控 Engine task，崩溃后最多重启 `MAX_RESTART_ATTEMPTS` 次
///
/// 完成检测：`EngineHandle.completed`（CancellationToken）在 Engine 退出（含
/// panic，经 CompletionGuard 的 Drop 触发）时取消，等待者零延迟唤醒。
///
/// 引用收口（todo 7.3 中期方案落地）：重启产生的新句柄经
/// `container.engine.replace()` 原子换入 EngineSlot，Web/托盘/关闭流程
/// 经 slot 取当前活跃 Engine，不再持有已死引用。
///
/// 状态恢复：崩溃前若监测处于 Running，重启完成后按原状态重发 Start，
/// 消除「崩溃自愈后 Engine 以 monitoring=false 空转、监测静默失效」。
///
/// panic 与正常退出不再区分：Engine 正常退出的唯一路径是收到 Shutdown
/// 命令，而 Shutdown 仅在应用级关闭令牌取消后发送——此时 biased select
/// 先命中 cancelled 分支返回，不进入重启循环。token 未取消时的任何退出
/// 均按崩溃处理。
pub(super) fn watch_engine(state: &LauncherState) -> JoinHandle<()> {
    // 容器缺失属逻辑不变量违反，防御性降级为 no-op 任务，避免 panic（历史遗留 #11）
    let Some(container_ref) = state.container.as_ref() else {
        return tokio::spawn(async {});
    };
    let container = container_ref.clone();
    let status = container.status.clone();
    let config = container.config.clone();
    let profile_service = container.profiles.clone();
    let orchestrator = container.login.clone();
    let monitor_service = container.monitor.clone();
    let network_detect = crate::network::detect::create_detector();
    let shutdown_token = state.shutdown_token.clone();

    tokio::spawn(async move {
        // 等待初始 Engine 完成（completed 在正常退出与 panic 时均触发）。
        // 同时监听 shutdown 信号，避免应用关闭时持续阻塞在此等待。
        let initial_completed = container
            .engine
            .current_handle()
            .map(|h| h.completed.clone());
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

            // 捕获崩溃前监测状态：Running 表示监测中，重启后需恢复
            // （Engine 崩溃后快照停留在最后的 Running/Stopped 值，正是恢复依据）
            let was_monitoring =
                status.borrow().engine_state == crate::status::EngineState::Running;

            // 通知 Orchestrator 取消 source=auto 的在途登录（reason 进用户可见的
            // 取消文案，须为中文而非英文标识符）
            orchestrator
                .cancel_auto_pending("引擎异常重启，自动登录已中止")
                .await;

            restart_count += 1;
            if restart_count > MAX_RESTART_ATTEMPTS {
                error!("Engine 重启次数耗尽（{MAX_RESTART_ATTEMPTS} 次），标记为 Dead");
                // 清空 slot，后续命令派发按 ChannelClosed 快速失败
                container.engine.clear();
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
                events: container.events.clone(),
            };
            let new_handle = crate::engine::Engine::spawn(deps);
            let new_completed = new_handle.completed.clone();

            // 引用收口：新句柄原子换入 slot，Web/托盘/关闭流程即刻指向新 Engine
            container.engine.replace(new_handle);

            // 等待重启冷却，期间若收到 shutdown 则提前退出
            tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS)) => {}
            }

            // 按崩溃前状态恢复监测（monitoring=false 空转会让用户以为还在监测）
            if was_monitoring {
                match container
                    .engine
                    .dispatch(crate::engine::EngineCommand::Start)
                    .await
                {
                    Ok(()) => info!("Engine（重启 #{restart_count}）已按崩溃前状态恢复检测"),
                    Err(e) => warn!("Engine（重启 #{restart_count}）恢复检测失败: {e:?}"),
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
