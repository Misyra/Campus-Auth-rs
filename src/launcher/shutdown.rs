//! 优雅关闭编排：按逆序停止全部服务、退出信号等待与强制退出看门狗（日志 flush 托管点）

use tracing::{info, warn};

use super::LauncherState;
use crate::app;

/// 按逆序关闭所有服务
pub(super) async fn graceful_shutdown(state: &mut LauncherState) {
    info!("正在关闭...");

    // 0.5 关机时补唤醒 helper：若 apply_update 时 spawn 失败但 pending 已落盘，
    // 此处再次尝试启动 helper，确保主进程退出后有人完成替换（幂等、无 pending 时空操作）
    if let Some(container) = &state.container {
        container.updater.ensure_helper_for_shutdown();
    }

    // 0. 立即取消应用级关闭令牌：
    // - wait_for_shutdown 各监听分支（若尚未返回）被唤醒
    // - watch_engine 收到 Engine 完成通知后据此区分"应用关闭"与"Engine 崩溃"，
    //   避免关闭流程中被重启出新 Engine（托盘退出曾因此留下无托盘的僵尸进程）
    state.shutdown_token.cancel();

    // 1. 关闭 TrayManager（先停泵任务，再 drop）
    #[cfg(feature = "desktop")]
    if let Some(handle) = state.tray_handle.take() {
        handle
            .stop_with_timeout(std::time::Duration::from_secs(3))
            .await;
    }
    #[cfg(feature = "desktop")]
    if let Some(tray) = state.tray_manager.take() {
        drop(tray);
    }

    // 2. 关闭 SchedulerService（并保留 bridge_handle 供第 4 步统一关闭）
    let bridge_handle = if let Some(handles) = state.startup_handles.take() {
        handles
            .scheduler_handle
            .stop_with_timeout(std::time::Duration::from_secs(5))
            .await;
        Some(handles.bridge_handle)
    } else {
        None
    };

    // 3. 关闭 Engine（先于 Bridge，因为 Engine 可能正在使用 Bridge）
    // 经 EngineSlot 派发到「当前活跃」Engine（崩溃重启后的新实例也覆盖）
    if let Some(container) = &state.container {
        let completed = container
            .engine
            .current_handle()
            .map(|h| h.completed.clone());
        let _ = container
            .engine
            .dispatch(crate::engine::EngineCommand::Shutdown)
            .await;
        // 应用级关闭令牌已在第 0 步取消，自动传播到容器内 uptime / 登录 shutdown 的
        // child token，使在途登录 task（detached，Engine 不持有其句柄）协作退出，
        // 避免其在 Bridge 关闭后仍引用已回收的 Worker（历史遗留 #8，错误洪泛风险）。
        if let Some(token) = completed {
            // 等待 Engine run_loop 完全退出后再关 Bridge，保证 Engine 侧不再发起 Bridge 调用
            if tokio::time::timeout(std::time::Duration::from_secs(5), token.cancelled())
                .await
                .is_err()
            {
                warn!("Engine 关闭超时，继续关闭 Bridge");
            }
        }
    }

    // 4. 关闭 BridgeSupervisor：停止 supervisor 主循环（内部回收 Worker）并等待 task 退出。
    // 通过 ServiceHandle::stop 发送停止信号并 join，避免 supervisor task 与残留子进程泄漏
    // （历史遗留 F4：原实现仅发 shutdown 命令、从不调用 stop，run_supervisor task 常驻）。
    if let Some(handle) = bridge_handle {
        handle
            .stop_with_timeout(std::time::Duration::from_secs(8))
            .await;
    }

    // 5. 关闭 Axum（内部含超时与 abort，历史遗留 #18）
    if let Some(handle) = state.axum_handle.take() {
        app::stop_axum(handle).await;
    }

    // 6. 清理运行端口文件（路径经 `utils::paths` 统一）
    if let Some(container) = &state.container {
        let port_file = crate::utils::paths::runtime_port_path(&container.config.base_path());
        let _ = std::fs::remove_file(&port_file);
    }

    // 7. 释放 ServiceContainer
    state.container = None;

    // 8. 释放实例锁（drop InstanceLock -> 文件关闭 -> 锁自动释放）
    state.instance_lock = None;

    info!("已关闭");
}

/// 等待退出信号（Ctrl+C / SIGTERM/SIGHUP / 托盘退出 / Web API 关闭）
pub(super) async fn wait_for_shutdown(state: &mut LauncherState) {
    let token = state.shutdown_token.clone();
    // 克隆 shutdown_rx 而非 take 出 handle，避免 drop stop_tx 导致 Axum 过早关闭
    let shutdown_rx = state.axum_handle.as_ref().map(|h| h.shutdown_rx.clone());
    tokio::select! {
        _ = async {
            // GUI 子系统（Windows release 双击启动）下无控制台，ctrl_c 注册可能
            // 直接返回 Err——若放任该分支在启动瞬间完成，进程会秒退。注册失败时
            // 退化为永久挂起，退出路径交由关闭令牌 / Web API / 托盘。
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        } => {
            info!("收到 Ctrl+C 信号");
        }
        _ = wait_terminate_signal() => {
            info!("收到 SIGTERM/SIGHUP 信号");
        }
        _ = token.cancelled() => {
            info!("收到关闭令牌取消信号");
        }
        _ = async {
            if let Some(mut rx) = shutdown_rx {
                match rx.changed().await {
                    // 正常路径：/api/system/shutdown 等主动发送
                    Ok(()) => info!("收到 Web API 关闭信号"),
                    // Err = 所有发送端已丢弃（Axum 任务退出），服务已先行终止
                    Err(_) => info!("Web API 关闭通道已关闭"),
                }
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            // 信号来源已在分支内区分记录
        }
    }
}

/// unix 终止信号监听（SIGTERM / SIGHUP 任一触发）
///
/// 外部 `kill <pid>`、launchd / systemd 停服、会话注销等都以这两个信号送达，
/// 此前只听 Ctrl+C，unix 上的非交互停止会把 Worker 与 chromium 变成孤儿
/// （unix 无 Job Object）。注册失败（极罕见）时退化为永久挂起，与 ctrl_c
/// 分支同策略，避免 select 分支瞬间完成导致秒退。非 unix 平台永不触发。
async fn wait_terminate_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate());
        let mut hup = signal(SignalKind::hangup());
        tokio::select! {
            _ = async {
                match term.as_mut() {
                    Ok(s) => { let _ = s.recv().await; }
                    Err(_) => std::future::pending::<()>().await,
                }
            } => {}
            _ = async {
                match hup.as_mut() {
                    Ok(s) => { let _ = s.recv().await; }
                    Err(_) => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}

/// 生成退出 watchdog：优雅关闭超时后强制 `exit(0)`，作为最后防线。
///
/// Web 重启/关闭接口与定时自重启共用，统一为 30s，
/// 覆盖优雅关闭总预算（Tray 3s + Scheduler 5s + Engine 5s + Bridge 8s + Axum 5s ≈ 26s），
/// 避免强杀过早残留浏览器/子进程（A4）。
pub(crate) fn spawn_exit_watchdog(secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        // 顺序关键：warn 先进 non-blocking 缓冲，统一 flush 落盘后再 exit——
        // exit 跳过 WorkerGuard 的 Drop，不 flush 会丢掉缓冲中的全部文件日志
        tracing::warn!("优雅关闭超时 {secs}s，强制退出");
        crate::logging::flush_stashed_log_guard();
        std::process::exit(0);
    });
}
