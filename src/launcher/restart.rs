//! 定时自重启：运行时长阈值判定、后继进程派生（`--restarting` 标记）与启动参数采集

use std::sync::atomic::Ordering;

use tracing::{error, info};

use super::LauncherState;
use super::cli::RuntimeMode;
use super::shutdown::spawn_exit_watchdog;

/// 启动定时自重启计时任务（full / lightweight 模式）
///
/// 每分钟读取一次 `app.auto_restart_hours` 与运行时长（Metrics.uptime_seconds），
/// 达到阈值即复用 `POST /api/system/restart` 同款机制：先 spawn 带 `--restarting`
/// 的后继进程（它会等待本进程释放实例锁），再取消应用级关闭令牌走优雅关闭，
/// 并武装 30s 退出 watchdog。配置运行时修改无需重启即生效。
pub(super) fn spawn_auto_restart_timer(state: &LauncherState) {
    // 单次登录模式进程本就即用即退，无需定时重启
    if matches!(state.app_config.runtime_mode, RuntimeMode::LoginOnce) {
        return;
    }
    let Some(container) = state.container.clone() else {
        return;
    };
    let token = state.shutdown_token.clone();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            }
            let hours = container
                .config
                .load_settings()
                .global
                .app
                .auto_restart_hours;
            if !should_auto_restart(
                hours,
                container.metrics.uptime_seconds.load(Ordering::Relaxed),
            ) {
                continue;
            }
            info!("运行时长已达 {hours} 小时，执行定时自重启");
            // 有待应用更新时不得生成后继进程：后继进程运行的是旧 exe，会锁住
            // 目标文件导致更新助手替换必然失败（os error 32）。此时走纯退出，
            // 由更新助手完成替换并用新版本重启（关机路径会兜底补唤醒助手）。
            if container.updater.has_pending_update() {
                info!("检测到待应用更新，跳过后继进程生成，退出后由更新助手替换并重启新版本");
            } else if let Err(e) = spawn_restart_successor() {
                // 后继进程启动失败时放弃本轮重启（保持当前进程运行），避免每分钟重试刷屏
                error!("定时自重启：启动后继进程失败（{e}），本次已放弃，保持当前进程运行");
                return;
            }
            spawn_exit_watchdog(30);
            token.cancel();
            return;
        }
    });
    state.track_background_task(handle);
}

/// 定时自重启触发判定（纯函数，便于单测）
///
/// `hours == 0` 表示未启用；运行时长达到 `hours * 3600` 秒即触发。
fn should_auto_restart(hours: u32, uptime_secs: u64) -> bool {
    hours > 0 && uptime_secs >= u64::from(hours) * 3600
}

/// 采集当前进程启动参数并剥离 `--restarting`（UPD-3 共用口径）
///
/// args_os：参数含非法 Unicode 时 env::args() 会 panic，args_os 不会。
/// 更新 pending 的 `original_args` 与重启后继共用本函数，避免两处口径漂移——
/// 若更新后新进程继承 `--restarting`，会错误进入「等待前驱释放锁」的重启语义，
/// 跳过重复启动检测等正常行为。
pub(crate) fn collect_args_without_restarting() -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    args.retain(|a| a != "--restarting");
    args
}

/// spawn 带 `--restarting` 标记的后继进程（定时自重启与 Web 重启接口共用）
///
/// 后继进程会先等待本进程释放实例锁再启动（见 `locks::wait_for_lock_release`），
/// 因此调用方必须**先 spawn 后继、再触发本进程优雅关闭**。
pub(crate) fn spawn_restart_successor() -> Result<(), String> {
    // procd 会在进程退出后重启，另起后继会让服务管理器丢失实际进程归属。
    if cfg!(feature = "openwrt") && std::env::var("CAMPUS_AUTH_PROCD").as_deref() == Ok("1") {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| format!("获取可执行文件路径失败: {e}"))?;
    let mut args = collect_args_without_restarting();
    args.push("--restarting".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args);
    // Windows：避免从 GUI 进程 spawn 出闪烁的控制台窗口（与其他子进程一致）
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("启动新进程失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::should_auto_restart;

    #[test]
    fn test_should_auto_restart() {
        // 未启用
        assert!(!should_auto_restart(0, 100 * 3600));
        // 未到阈值
        assert!(!should_auto_restart(24, 23 * 3600 + 3599));
        // 恰好到达与超过
        assert!(should_auto_restart(24, 24 * 3600));
        assert!(should_auto_restart(6, 7 * 3600));
    }
}
