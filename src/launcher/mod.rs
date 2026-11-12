//! 启动状态机：配置加载、目录权限、实例互斥、服务启动、模式分发、Engine 崩溃恢复、优雅关闭
//!
//! 三种运行模式：
//! - **full**：全部服务 + Axum + 托盘
//! - **lightweight**：仅引擎 + 托盘（Axum 按需启动）
//! - **login_once**：执行一次登录后退出
//!
//! 子模块按职责划分：`cli`（CLI 参数定义）、`config`（启动配置合并与目录权限
//! 预检）、`locks`（实例互斥锁）、`modes`（三种模式分发执行）、`engine_watch`
//! （Engine 崩溃恢复）、`restart`（定时自重启与后继进程）、`shutdown`（优雅
//! 关闭与退出信号）。共享内部状态（AppConfig / LauncherState）留在本文件。

mod cli;
mod config;
mod engine_watch;
mod locks;
mod modes;
mod restart;
mod shutdown;

pub use cli::{AutostartAction, CliArgs, RuntimeMode};
pub(crate) use restart::{collect_args_without_restarting, spawn_restart_successor};
pub(crate) use shutdown::spawn_exit_watchdog;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::app::{self, AxumServeHandle};
use crate::container::{ServiceContainer, StartupHandles};
use crate::logging::{LogEntry, init_logging, log_broadcast_tx};
#[cfg(feature = "desktop")]
use crate::tray::TrayManager;
use crate::utils::lock::InstanceLock;

use config::{check_directory_permissions, load_and_merge_config};
use locks::{acquire_lock, wait_for_lock_release};
use modes::{launch_full, launch_lightweight, launch_login_once};
use shutdown::graceful_shutdown;

// ============================================================
// 内部状态
// ============================================================

/// 合并后的启动配置（CLI + settings.json + 默认值）
struct AppConfig {
    port: u16,
    host: Option<String>,
    base_path: PathBuf,
    runtime_mode: RuntimeMode,
    #[cfg(feature = "desktop")]
    no_tray: bool,
    /// 自动打开浏览器：CLI --no-browser 或 settings.json 关闭时为 false。
    /// 同时约束"启动后打开"与"重复启动时打开已有实例的 Web 控制台"两条路径
    auto_open_browser: bool,
    /// 日志初始级别（与 settings.json 单次解析得出，透传给 `init_logging`）
    log_level: tracing_subscriber::filter::LevelFilter,
    /// 日志保留天数（同上，避免日志模块重复读文件）
    log_retention_days: u32,
    /// 是否写日志文件（false 时 `init_logging` 跳过文件层）
    log_file_enabled: bool,
    /// 管理密码（settings.json 的 app.management_password；覆盖值缺省时生效）。
    /// 启动期即读取用于非回环绑定的凭据门禁（服务容器尚未初始化，无法走 ConfigService）
    management_password: String,
    /// 管理密码覆盖值（CLI `--management-password` / env，非空时优先于配置值）
    management_password_override: Option<String>,
    /// 显式豁免无凭据的非回环绑定（--allow-unauthenticated-bind）
    allow_unauthenticated_bind: bool,
}

/// 启动过程中累积的中间状态
struct LauncherState {
    app_config: AppConfig,
    instance_lock: Option<InstanceLock>,
    container: Option<Arc<ServiceContainer>>,
    startup_handles: Option<StartupHandles>,
    /// 完整模式在服务初始化前预绑定的监听器；启动 Axum 时消费。
    prepared_axum: Option<app::PreparedAxumListener>,
    axum_handle: Option<AxumServeHandle>,
    #[cfg(feature = "desktop")]
    tray_manager: Option<Arc<TrayManager>>,
    /// 托盘泵任务句柄（spawn 后填充，优雅关闭时 stop）
    #[cfg(feature = "desktop")]
    tray_handle: Option<crate::tray::ServiceHandle>,
    shutdown_token: CancellationToken,
    log_tx: tokio::sync::broadcast::Sender<LogEntry>,
    /// 辅助后台任务句柄（更新检查/自重启计时/启动动作派发；UPD-9：spawn 后
    /// 丢弃句柄无法 join 验证，关闭时序无保证）。watchdog 不在此列——它是
    /// 优雅关闭超时后的最后防线，必须活过关闭流程。
    background_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl LauncherState {
    /// 登记后台任务句柄，供关闭流程统一 abort
    fn track_background_task(&self, handle: tokio::task::JoinHandle<()>) {
        self.background_tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }

    /// 中止全部已登记后台任务（这些任务本身也监听 shutdown_token，abort 仅
    /// 兜底保证关闭时序，不改变优雅退出语义）
    fn abort_background_tasks(&self) {
        let mut tasks = self
            .background_tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for t in tasks.drain(..) {
            t.abort();
        }
    }
}

// ============================================================
// 入口
// ============================================================

/// 未解压运行的早期拦截（最优先）
///
/// 若检测到从压缩包虚拟路径 / 临时目录缺同级资源直接启动：桌面端弹窗 + stderr 同步输出并拒绝启动。
/// - Docker / 显式 --base-path 覆盖 / 环境变量或 --allow-temp 已放行时降级放行，见 `utils::archive_guard::check_archive_block`
/// - `--status` / `--stop` / `--autostart` 由 `main.rs` 在 `launcher::run` 之前已短路，无需额外短路
fn guard_archive_run(cli: &CliArgs, base_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("campus-auth"));
    if let Some(msg) =
        crate::utils::archive_guard::check_archive_block(&exe, base_path, cli.allow_temp)
    {
        // 同步通道（CLI 控制台可见）+ 异步通道（GUI 双击无控制台时弹窗可见）
        eprintln!("\n{msg}\n");
        #[cfg(windows)]
        crate::utils::archive_guard::show_block_dialog(&msg);
        anyhow::bail!("{msg}");
    }
    Ok(())
}

/// 启动编排主入口（由 `main.rs` 调用）
pub async fn run(cli: CliArgs, base_path: PathBuf) -> Result<()> {
    // 0. 未解压运行拦截（目录权限、实例锁、日志之前）
    guard_archive_run(&cli, &base_path)?;

    // 1. 配置合并
    let app_config = load_and_merge_config(&cli, base_path)?;

    // 2. 日志子系统（级别/保留天数/文件开关由步骤 1 单次解析透传，不再重复读文件）。
    //    必须先于目录检查/重启等待/实例锁初始化：锁冲突、强杀残留等早期 warn
    //    才能进日志流，而不是丢在 subscriber 就绪之前
    let log_tx = log_broadcast_tx();
    let log_guard = init_logging(
        &app_config.base_path,
        log_tx.clone(),
        app_config.log_level,
        app_config.log_retention_days,
        app_config.log_file_enabled,
    );

    // WorkerGuard 存入全局唯一托管点（`logging::stash_log_guard`）：正常退出由
    // 本函数末尾统一 flush；优雅关闭卡死被 exit watchdog 强杀时同样先 flush 再
    // exit——exit 跳过 Drop，不 flush 则 non-blocking writer 缓冲的文件日志
    // （含定位卡死最关键的尾段）全部丢失。
    crate::logging::stash_log_guard(log_guard);
    let result = run_after_logging(cli, app_config, log_tx).await;
    if let Err(error) = &result {
        error!(error = %format!("{error:#}"), "启动失败");
    }
    // 取出并 drop guard：non-blocking writer flush 后关闭后台线程，
    // 保证上面的“启动失败”及全部启动期日志落盘
    crate::logging::flush_stashed_log_guard();
    result
}

/// 日志初始化后的启动主流程
///
/// 由 [`run`] 在日志 guard 存活期间调用，任何早期错误返回后都会先记录、flush，
/// 再交由 `main` 向 stderr 展示。
async fn run_after_logging(
    cli: CliArgs,
    app_config: AppConfig,
    log_tx: tokio::sync::broadcast::Sender<LogEntry>,
) -> Result<()> {
    info!(
        startup_source = if cli.startup_launch { "system_autostart" } else { "manual_or_other" },
        exe_path = ?std::env::current_exe().ok(),
        base_path = %app_config.base_path.display(),
        "启动来源"
    );
    #[cfg(windows)]
    {
        let base = app_config.base_path.clone();
        match tokio::task::spawn_blocking(move || crate::utils::platform::self_start_status(&base))
            .await
        {
            Ok(Ok(status)) => {
                if status.registered && !status.enabled {
                    warn!(autostart = ?status, "自启动注册异常，请在设置中检查并显式修复");
                } else {
                    info!(autostart = ?status, "系统自启动状态");
                }
            }
            Ok(Err(error)) => warn!(%error, "无法读取系统自启动状态"),
            Err(error) => warn!(%error, "自启动诊断任务失败"),
        }
    }
    // 3. 目录权限检查
    check_directory_permissions(&app_config.base_path)?;

    // 4-5. 重启场景在等待到锁后直接持有，避免“探测成功后 drop 再获取”的竞态窗口。
    let instance_lock = if cli.restarting {
        wait_for_lock_release(&app_config.base_path).await?
    } else {
        match acquire_lock(&app_config.base_path, cli.force).await {
            Ok(lock) => lock,
            Err(e) => {
                // 已有实例运行时（典型的双击 exe 重复启动场景）不再直接报错退出，
                // 而是打开运行中实例的 Web 控制台后正常退出——GUI 双击无控制台，
                // 浏览器就是"已在运行"的用户可见信号。轻量模式（端口 0）没有
                // Web 入口，维持原报错。
                if !cli.force
                    && let Some(info) = crate::utils::lock::query_instance(&app_config.base_path)
                    && info.running
                    && info.port > 0
                    && app_config.auto_open_browser
                {
                    // 与首启路径同口径：URL 携带 bootstrap fragment（fragment 不进
                    // 服务器日志；运行中实例的凭据文件与本次启动共享同一 base_path）
                    let bootstrap = crate::web::auth::read_bootstrap_file(&app_config.base_path);
                    let url = crate::web::auth::ui_url(info.port, bootstrap.as_deref());
                    if open::that(&url).is_ok() {
                        // 控制台层写 stderr（终端启动可见）；本路径提前返回后，
                        // run 外层统一 drop log_guard 并 flush 该条日志。
                        // 日志只记不含 fragment 的基础 URL，绝不记录 bootstrap 秘钥
                        info!(
                            pid = info.pid,
                            url = %format!("http://127.0.0.1:{}", info.port),
                            "已有实例运行中，已在浏览器打开其 Web 控制台"
                        );
                        return Ok(());
                    }
                }
                return Err(e);
            }
        }
    };

    // 启动信息留痕：版本 /
    // 根目录 / 运行模式；非完整模式的实际端口按需分配（配置值无意义），省略该字段
    if matches!(app_config.runtime_mode, RuntimeMode::Full) {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            base_path = %app_config.base_path.display(),
            port = app_config.port,
            mode = "full",
            "应用启动"
        );
    } else {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            base_path = %app_config.base_path.display(),
            mode = ?app_config.runtime_mode,
            "应用启动"
        );
    }

    // 完整模式先真实占住监听端口，再初始化服务容器。这样端口权限等不可恢复
    // 错误不会启动 Engine / Scheduler / Bridge 后立刻整套关闭；可恢复的占用或
    // Windows 10013 已在 prepare_axum_listener 内切换到内核分配端口。
    // 凭据门禁参数在此一并确定：容器尚未初始化，配置值取启动期轻量读结果
    // （覆盖值来自 CLI/env），生效值计算与 exchange 运行期共用同一纯函数。
    let bind_gate = app::BindCredentialGate {
        effective_password: crate::web::auth::effective_management_password(
            app_config.management_password_override.as_deref(),
            &app_config.management_password,
        )
        .into_owned(),
        allow_unauthenticated_bind: app_config.allow_unauthenticated_bind,
    };
    let prepared_axum = if matches!(app_config.runtime_mode, RuntimeMode::Full) {
        Some(
            app::prepare_axum_listener(app_config.port, app_config.host.as_deref(), &bind_gate)
                .await
                .context("Web 控制台端口准备失败")?,
        )
    } else {
        None
    };

    // 6. 引导服务容器
    // 应用级关闭令牌在此创建：传入容器派生 uptime/登录 shutdown 的 child，
    // 并作为 LauncherState 的 shutdown_token 统一驱动关闭（A3）。
    info!("正在初始化服务...");
    let shutdown_token = CancellationToken::new();
    let (container, handles) =
        ServiceContainer::new(&app_config.base_path, shutdown_token.clone()).await?;
    info!("服务容器初始化完成");

    // login_once 模式登录即退，调度器的启动触发任务须放弃执行；
    // 调度器无法从配置可靠区分 CLI 注入的 login_once，在模式分发前显式标记
    container
        .scheduler
        .set_login_once_mode(matches!(app_config.runtime_mode, RuntimeMode::LoginOnce));

    // 7. CLI --startup-action 覆盖配置文件
    if let Some(action) = cli.startup_action {
        let mut settings = container.config.load_settings();
        if settings.global.app.startup_action != action {
            settings.global.app.startup_action = action;
            if let Err(e) = container.config.save_settings(&settings).await {
                tracing::warn!("保存 startup_action 配置失败: {e}");
            } else {
                info!(action = ?settings.global.app.startup_action, "已按 CLI 参数覆盖 startup_action 配置");
            }
        }
    }

    let mut state = LauncherState {
        app_config,
        instance_lock: Some(instance_lock),
        container: Some(container.clone()),
        startup_handles: Some(handles),
        prepared_axum,
        axum_handle: None,
        #[cfg(feature = "desktop")]
        tray_manager: None,
        #[cfg(feature = "desktop")]
        tray_handle: None,
        shutdown_token,
        log_tx,
        background_tasks: std::sync::Mutex::new(Vec::new()),
    };

    // 8. 创建系统托盘
    // macOS 也照常创建（纯通道结构，无副作用），真正的禁用拦截在
    // TrayManager::spawn 内部单点执行——macOS 返回空句柄，托盘永不启动
    // Docker 环境无显示服务器，强制禁用托盘
    #[cfg(feature = "desktop")]
    let no_tray_effective =
        state.app_config.no_tray || crate::app::is_docker_env() || cfg!(feature = "openwrt");
    #[cfg(feature = "desktop")]
    if !no_tray_effective {
        let tray = TrayManager::new(crate::tray::TrayDeps {
            config: container.config.clone(),
            status: container.status.clone(),
            engine: container.engine.clone(),
            login: container.login.clone(),
            profile_service: container.profiles.clone(),
            container: container.clone(),
            log_tx: state.log_tx.clone(),
            port: state.app_config.port,
            host: state.app_config.host.clone(),
            lightweight: matches!(state.app_config.runtime_mode, RuntimeMode::Lightweight),
            management_password_override: state.app_config.management_password_override.clone(),
            allow_unauthenticated_bind: state.app_config.allow_unauthenticated_bind,
            shutdown: state.shutdown_token.clone(),
        });
        state.tray_manager = Some(tray);
    }

    // 9. 模式分发
    let result = match state.app_config.runtime_mode {
        RuntimeMode::Full => launch_full(&mut state).await,
        RuntimeMode::Lightweight => launch_lightweight(&mut state).await,
        RuntimeMode::LoginOnce => launch_login_once(&mut state).await,
    };

    // 10. 优雅关闭（无论成功失败）
    graceful_shutdown(&mut state).await;

    result
}

// ============================================================
// 辅助函数
// ============================================================

/// 打开系统默认浏览器（URL 携带 `#auth=<bootstrap>` fragment，见 `web::auth::ui_url`）
///
/// 日志只记录不含 fragment 的基础 URL，绝不记录 bootstrap 秘钥；凭据文件尚未
/// 生成/读取失败时退化为不带 fragment 的普通 URL（此时 Axum 启动路径已生成凭据，
/// 正常情况下不会走到退化分支）。
fn open_browser(base_path: &Path, port: u16) {
    let bootstrap = crate::web::auth::read_bootstrap_file(base_path);
    let url = crate::web::auth::ui_url(port, bootstrap.as_deref());
    if let Err(e) = open::that(&url) {
        warn!("打开浏览器失败 (http://127.0.0.1:{port}): {e}");
    } else {
        info!("已打开浏览器: http://127.0.0.1:{port}（已携带 auth fragment）");
    }
}
