//! 三种运行模式的分发执行：full（全部服务）/ lightweight（仅引擎）/ login_once（单次登录）

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::LauncherState;
use super::engine_watch::watch_engine;
use super::open_browser;
use super::restart::spawn_auto_restart_timer;
use super::shutdown::wait_for_shutdown;
use crate::app;
use crate::config::schema::StartupAction;
use crate::container::ServiceContainer;

/// 完整模式：全部服务 + Axum + 托盘，阻塞等待退出信号
pub(super) async fn launch_full(state: &mut LauncherState) -> Result<()> {
    // 容器缺失属逻辑不变量违反，防御性返回错误而非 panic（历史遗留 #11）
    let container = state
        .container
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("服务容器尚未初始化"))?
        .clone();

    // 端口已在服务容器初始化前绑定，此处只组装 Router 并启动 serve task。
    let prepared = state
        .prepared_axum
        .take()
        .ok_or_else(|| anyhow::anyhow!("完整模式缺少预绑定的 Axum 监听器"))?;
    let handle = app::start_axum_with_listener(
        container,
        state.log_tx.clone(),
        prepared,
        state.app_config.management_password_override.clone(),
        state.app_config.allow_unauthenticated_bind,
    )
    .context("Web 控制台启动失败")?;
    let port = handle.port;
    if let Some(lock) = &state.instance_lock
        && let Err(e) = lock.record_port(port)
    {
        warn!(port = port, error = %e, "记录运行端口到实例锁失败");
    }
    state.axum_handle = Some(handle);
    let bind_host = state.app_config.host.as_deref().unwrap_or("127.0.0.1");
    // 展示地址跟随真实绑定地址（P3-13）：--host 192.168.x.x 的 LAN 部署此前被
    // 一律显示成 127.0.0.1，日志与提示双错；0.0.0.0 照实显示（全接口绑定的
    // 事实陈述，不是可访问的目的地址）
    let display_host = display_host_for(bind_host);
    info!("Web 控制台已启动: http://{display_host}:{port}");

    // CLI --no-browser 与设置项 app.auto_start_browser 任一关闭即不打开：
    // 此前配置项是死开关，UI「静默启动」切换无任何效果
    if state.app_config.auto_open_browser {
        // 打开失败（无桌面环境等）时同样给出终端入口，避免界面彻底不可达
        if !open_browser(&state.app_config.base_path, port) {
            print_headless_ui_hint(state, port, bind_host);
        }
    } else {
        print_headless_ui_hint(state, port, bind_host);
    }

    // Engine 崩溃恢复
    let watch_handle = watch_engine(state);

    // 启动托盘（在专用 OS 线程上构建图标与菜单）
    // macOS 的禁用拦截收敛在 TrayManager::spawn 内部单点执行（W6 用户决策）
    #[cfg(feature = "desktop")]
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    // 按 startup_action 派发启动动作（Monitor / LoginOnce）
    if let Some(container) = state.container.as_ref() {
        apply_startup_action(state, container).await;
    }

    // 后台更新检查
    spawn_background_update_check(state);

    // 定时自重启计时
    spawn_auto_restart_timer(state);

    // 与 lightweight / login_once 模式对称的运行中留痕
    info!("完整模式运行中");

    // 事件循环
    wait_for_shutdown(state).await;
    watch_handle.abort();
    state.abort_background_tasks();

    Ok(())
}

/// 轻量模式：仅引擎 + 托盘（Axum 按需启动）
pub(super) async fn launch_lightweight(state: &mut LauncherState) -> Result<()> {
    let watch_handle = watch_engine(state);

    // 启动托盘（轻量模式也显示托盘；macOS 由 spawn 内部单点拦截返回空句柄）
    #[cfg(feature = "desktop")]
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    if let Some(container) = state.container.as_ref() {
        apply_startup_action(state, container).await;
    }

    // G15：轻量模式下此刻 Axum 尚未监听（按需启动），记录哨兵端口 0——
    // `.instance` 文件仍提供 PID/存活状态供 --status 查询，但 --stop 不会向
    // 未监听端口发无效请求。真实端口在 Axum 绑定后由托盘按需启动路径
    // （write_instance_port）回写。
    if let Some(ref lock) = state.instance_lock
        && let Err(e) = lock.record_port(0)
    {
        tracing::debug!(error = %e, "记录哨兵端口 0 失败（--status 将看不到端口）");
    }

    spawn_background_update_check(state);

    spawn_auto_restart_timer(state);

    info!("轻量模式运行中（Axum 按需启动）");
    wait_for_shutdown(state).await;
    watch_handle.abort();
    state.abort_background_tasks();

    Ok(())
}

/// 按 settings.global.app.startup_action 派发启动动作
///
/// `login_once` 的口径：登录**成功**后取消应用级关闭令牌退出程序（与设置页
/// 文案「成功后自动退出程序」一致），失败/取消保持驻留供用户排查重试。
///
/// 该配置此前只有写入点（CLI --startup-action / autostart API），从未被启动
/// 逻辑消费——默认值 Monitor 的语义"启动后进入监测"实际从未生效，
/// Engine 一直以 monitoring=false 空转等待用户手动触发。
async fn apply_startup_action(state: &LauncherState, container: &Arc<ServiceContainer>) {
    let settings = container.config.load_settings();
    match settings.global.app.startup_action {
        StartupAction::Monitor => {
            match container
                .engine
                .dispatch(crate::engine::EngineCommand::Start)
                .await
            {
                Ok(()) => info!("按 startup_action=monitor 已启动检测"),
                Err(e) => warn!("按 startup_action 启动检测失败: {e:?}"),
            }
        }
        StartupAction::LoginOnce => {
            info!("按 startup_action=login_once 触发单次登录");
            let orchestrator = container.login.clone();
            // 设置页该选项承诺「成功后自动退出程序」：成功即取消应用级关闭令牌，
            // wait_for_shutdown 唤醒后走统一优雅关闭，与 CLI --mode login-once
            // 共用同一退出路径；失败/取消不退出，保持驻留便于 Web 控制台排查
            let shutdown = state.shutdown_token.clone();
            let handle = tokio::spawn(async move {
                let handle = orchestrator
                    .submit(crate::status::LoginSource::LoginOnce, None, None)
                    .await;
                let result = handle.await_result().await;
                if result.is_success() {
                    info!(
                        message = %result.message,
                        "启动单次登录成功，按 startup_action 配置退出程序"
                    );
                    shutdown.cancel();
                } else {
                    warn!(message = %result.message, "启动单次登录失败，程序保持运行");
                }
            });
            state.track_background_task(handle);
        }
        StartupAction::None => {
            tracing::debug!("startup_action=None，不派发启动动作");
        }
    }
}

/// 单次登录模式：执行一次登录后退出
pub(super) async fn launch_login_once(state: &mut LauncherState) -> Result<()> {
    // 容器缺失属逻辑不变量违反，防御性返回错误而非 panic（历史遗留 #11）
    let container = state
        .container
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("服务容器尚未初始化"))?;

    info!("单次登录模式");
    let handle = container
        .login
        .submit(crate::status::LoginSource::LoginOnce, None, None)
        .await;
    let result = handle.await_result().await;

    info!(
        success = result.is_success(),
        message = %result.message,
        duration = ?result.duration,
        "单次登录完成"
    );

    if !result.is_success() {
        anyhow::bail!("登录失败: {}", result.message);
    }
    Ok(())
}

/// 启动后台更新检查任务
fn spawn_background_update_check(state: &LauncherState) {
    if let Some(container) = &state.container {
        let handle = container
            .updater
            .start_background_check(state.shutdown_token.clone());
        state.track_background_task(handle);
    }
}

/// 绑定地址 → 展示用 host 文本（P3-13）
///
/// 与监听侧共用 [`app::parse_bind_addr`] 的解析口径（不可解析的 host 同样
/// 回退回环），展示的必然是实际绑定的地址；IPv6 按 URL 书写惯例补方括号
/// （`http://[::1]:port`），IPv4 原样。
fn display_host_for(bind_host: &str) -> String {
    match app::parse_bind_addr(bind_host) {
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        ip => ip.to_string(),
    }
}

/// 无浏览器 + 无托盘（且回环监听）时，把带引导秘钥的完整界面地址打到终端。
///
/// 该组合下没有任何界面入口——托盘「复制界面链接」不存在、也不自动开浏览器，
/// 用户只能去读 0600 的密钥文件，headless 自测/无桌面环境等于「打不开」。
/// 只写 stderr（终端可见），**不进日志文件与诊断包**：后端日志按设计只记录
/// 不含 fragment 的基础 URL，引导秘钥不得落盘。
fn print_headless_ui_hint(state: &LauncherState, port: u16, bind_host: &str) {
    // no_tray 字段仅桌面构建存在；其余构建恒无托盘，视同「无托盘」成立
    #[cfg(feature = "desktop")]
    let no_tray = state.app_config.no_tray;
    #[cfg(not(feature = "desktop"))]
    let no_tray = true;
    // 回环判据用真实绑定地址（P3-13）：此前拿展示地址做字符串硬比较，
    // `--host ::1` 等回环绑定会被误判为非回环而不打印；解析口径与监听侧
    // parse_bind_addr 完全一致，杜绝两处判定漂移
    if !app::parse_bind_addr(bind_host).is_loopback()
        || !no_tray
        || app::is_docker_env()
        || cfg!(any(feature = "openwrt", feature = "android"))
    {
        return;
    }
    let bootstrap = crate::web::auth::read_bootstrap_file(&state.app_config.base_path);
    eprintln!(
        "界面入口（含引导秘钥，勿转发）：{}",
        crate::web::auth::ui_url(port, bootstrap.as_deref())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// display_host_for：跟随真实绑定地址（P3-13），IPv6 补方括号
    #[test]
    fn test_display_host_follows_bind_address() {
        assert_eq!(display_host_for("127.0.0.1"), "127.0.0.1");
        assert_eq!(display_host_for("0.0.0.0"), "0.0.0.0");
        // LAN 部署此前被硬编码显示成 127.0.0.1，日志与实际绑定地址不符
        assert_eq!(display_host_for("192.168.1.5"), "192.168.1.5");
        assert_eq!(display_host_for("::1"), "[::1]");
        // 不可解析的 host 与监听侧 parse_bind_addr 同口径回退回环
        assert_eq!(display_host_for("not-an-ip"), "127.0.0.1");
    }

    /// 回环判据口径（P3-13）：按绑定地址 is_loopback 判定，
    /// 0.0.0.0（全接口）与 LAN 地址均不触发提示
    #[test]
    fn test_hint_loopback_criterion_matches_bind_addr() {
        assert!(app::parse_bind_addr("127.0.0.1").is_loopback());
        assert!(app::parse_bind_addr("::1").is_loopback());
        assert!(!app::parse_bind_addr("0.0.0.0").is_loopback());
        assert!(!app::parse_bind_addr("192.168.1.5").is_loopback());
        // 监听侧对主机名回退回环绑定，提示判据保持同一结论
        assert!(app::parse_bind_addr("localhost").is_loopback());
    }
}
