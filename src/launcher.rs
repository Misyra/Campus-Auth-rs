//! 启动状态机：配置加载、目录权限、实例互斥、服务启动、模式分发、Engine 崩溃恢复、优雅关闭
//!
//! 三种运行模式：
//! - **full**：全部服务 + Axum + 托盘
//! - **lightweight**：仅引擎 + 托盘（Axum 按需启动）
//! - **login_once**：执行一次登录后退出

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::logging::{LogEntry, WorkerGuard, init_logging, log_broadcast_tx};
use anyhow::{Context, Result};
use clap::Parser;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::app::{self, AxumServeHandle};
use crate::config::schema::StartupAction;
use crate::container::{ServiceContainer, StartupHandles};
use crate::engine::{EngineDeps, MAX_RESTART_ATTEMPTS, RESTART_DELAY_SECS};
use crate::tray::TrayManager;
use crate::utils::lock::InstanceLock;

// ============================================================
// CLI 类型定义（由 main.rs 使用）
// ============================================================

/// CLI 参数定义
#[derive(Parser, Clone)]
#[command(name = "campus-auth", version, about)]
pub struct CliArgs {
    /// 启动模式：完整 / 轻量 / 单次登录（缺省时读 settings.json 的 app.runtime_mode）
    #[arg(short, long, value_enum)]
    pub mode: Option<RuntimeMode>,

    /// 基准路径（配置文件 / 任务 / 日志 的根目录）
    #[arg(long, env = "CAMPUS_AUTH_BASE_PATH")]
    pub base_path: Option<PathBuf>,
    /// 监听端口（默认从 settings.json 读取）
    #[arg(short, long, env = "CAMPUS_AUTH_PORT")]
    pub port: Option<u16>,

    /// 监听地址（默认 127.0.0.1，Docker 环境默认 0.0.0.0；可用 CAMPUS_AUTH_HOST 环境变量覆盖）
    #[arg(long, env = "CAMPUS_AUTH_HOST")]
    pub host: Option<String>,

    /// 启动后不自动打开浏览器
    #[arg(long)]
    pub no_browser: bool,

    /// 不显示系统托盘图标
    #[arg(long)]
    pub no_tray: bool,

    /// 强制终止已有实例后启动
    #[arg(long)]
    pub force: bool,

    /// 重启标记（新进程等待旧进程释放锁）
    #[arg(long, hide = true)]
    pub restarting: bool,

    /// 查询当前运行实例的状态
    #[arg(long)]
    pub status: bool,

    /// 停止当前运行实例
    #[arg(long)]
    pub stop: bool,

    /// 注册 / 取消开机自启动
    #[arg(long, value_enum)]
    pub autostart: Option<AutostartAction>,

    /// 启动动作（覆盖 settings.json 中的 startup_action）
    #[arg(long, value_enum)]
    pub startup_action: Option<StartupAction>,
}

/// 运行模式
#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum RuntimeMode {
    /// 完整模式（全部服务 + Web API + 托盘）
    Full,
    /// 轻量模式（仅引擎 + 托盘，Axum 按需启动）
    Lightweight,
    /// 单次登录（执行一次登录后退出）
    LoginOnce,
}

/// 自启动操作
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum AutostartAction {
    /// 注册自启动
    Enable,
    /// 取消自启动
    Disable,
}

// ============================================================
// 内部类型
// ============================================================

/// 合并后的启动配置（CLI + settings.json + 默认值）
struct AppConfig {
    port: u16,
    host: Option<String>,
    base_path: PathBuf,
    runtime_mode: RuntimeMode,
    no_tray: bool,
    /// 自动打开浏览器：CLI --no-browser 或 settings.json 关闭时为 false。
    /// 同时约束"启动后打开"与"重复启动时打开已有实例的 Web 控制台"两条路径
    auto_open_browser: bool,
}

/// 启动过程中累积的中间状态
pub(crate) struct LauncherState {
    app_config: AppConfig,
    instance_lock: Option<InstanceLock>,
    container: Option<Arc<ServiceContainer>>,
    startup_handles: Option<StartupHandles>,
    axum_handle: Option<AxumServeHandle>,
    tray_manager: Option<Arc<TrayManager>>,
    /// 托盘泵任务句柄（spawn 后填充，优雅关闭时 stop）
    tray_handle: Option<crate::tray::ServiceHandle>,
    shutdown_token: CancellationToken,
    log_tx: tokio::sync::broadcast::Sender<LogEntry>,
    /// 日志文件非阻塞写入的 WorkerGuard，优雅关闭时 drop 以 flush 剩余日志
    _log_guard: Option<WorkerGuard>,
}

// ============================================================
// 入口
// ============================================================

/// 启动编排主入口（由 `main.rs` 调用）
pub async fn run(cli: CliArgs, base_path: PathBuf) -> Result<()> {
    // 1. 配置合并
    let app_config = load_and_merge_config(&cli, base_path)?;

    // 2. 目录权限检查
    check_directory_permissions(&app_config.base_path)?;

    // 3. 重启等待
    if cli.restarting {
        wait_for_lock_release(&app_config.base_path).await?;
    }

    // 4. 实例锁
    let instance_lock = match acquire_lock(&app_config.base_path, cli.force) {
        Ok(lock) => lock,
        Err(e) => {
            // 已有实例运行时（典型的双击 exe 重复启动场景）不再直接报错退出，
            // 而是打开运行中实例的 Web 控制台后正常退出——GUI 子系统下 stderr
            // 不可见，静默失败会让用户以为双击无响应。轻量模式（端口 0）没有
            // Web 入口，维持原报错。
            if !cli.force {
                if let Some(info) = crate::utils::lock::query_instance(&app_config.base_path) {
                    if info.running && info.port > 0 && app_config.auto_open_browser {
                        let url = format!("http://127.0.0.1:{}", info.port);
                        if open::that(&url).is_ok() {
                            // 此刻日志系统尚未初始化（tracing 无 subscriber），同步落 stderr
                            eprintln!(
                                "已有实例运行中（PID {}），已在浏览器打开 Web 控制台: {url}",
                                info.pid
                            );
                            return Ok(());
                        }
                    }
                }
            }
            return Err(e);
        }
    };

    // 5. 日志广播通道 + 文件日志层
    let log_tx = log_broadcast_tx();
    let log_guard = init_logging(&app_config.base_path, log_tx.clone());

    // 启动信息留痕（日志系统刚就绪，此前的 tracing 日志无 subscriber）：版本 /
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

    // 6. 引导服务容器
    // 应用级关闭令牌在此创建：传入容器派生 uptime/登录 shutdown 的 child，
    // 并作为 LauncherState 的 shutdown_token 统一驱动关闭（A3）。
    info!("正在初始化服务...");
    let shutdown_token = CancellationToken::new();
    let (container, handles) =
        ServiceContainer::new(&app_config.base_path, shutdown_token.clone()).await?;
    info!("服务容器初始化完成");

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
        axum_handle: None,
        tray_manager: None,
        tray_handle: None,
        shutdown_token,
        log_tx,
        _log_guard: Some(log_guard),
    };

    // 8. 创建系统托盘
    // macOS 也照常创建（纯通道结构，无副作用），真正的禁用拦截在
    // TrayManager::spawn 内部单点执行——macOS 返回空句柄，托盘永不启动
    // Docker 环境无显示服务器，强制禁用托盘
    let no_tray_effective = state.app_config.no_tray || crate::app::is_docker_env();
    if !no_tray_effective {
        let tray = TrayManager::new(crate::tray::TrayDeps {
            config: container.config.clone(),
            status: container.status.clone(),
            engine: container.engine.clone(),
            profile_service: container.profiles.clone(),
            updater: container.updater.clone(),
            container: container.clone(),
            log_tx: state.log_tx.clone(),
            port: state.app_config.port,
            host: state.app_config.host.clone(),
            lightweight: matches!(state.app_config.runtime_mode, RuntimeMode::Lightweight),
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
// 配置 & 权限
// ============================================================

/// 加载并合并配置：defaults -> settings.json -> CLI 覆盖
fn load_and_merge_config(cli: &CliArgs, base_path: PathBuf) -> Result<AppConfig> {
    // 轻量读取 settings.json 的启动字段，不创建完整 ConfigService——
    // 正式容器在 run() 中统一初始化，避免重复目录 I/O / 迁移 / 解密（历史遗留 M2）。
    let (file_port, file_show_tray, file_mode, file_auto_open) = read_startup_settings(&base_path);

    let port = cli.port.unwrap_or(file_port).max(1);
    // CLI --mode 显式指定时优先生效；缺省时沿用 settings.json 的 app.runtime_mode
    let runtime_mode = cli.mode.clone().unwrap_or(file_mode);
    // macOS：托盘已禁用（用户决策，W6——tray-icon 要求主线程 NSApplication
    // 事件循环，主线程运行 tokio runtime 无法满足）；轻量模式在 mac 上会既无
    // 托盘也无 Web 入口，统一降级为完整模式保证可用
    #[cfg(target_os = "macos")]
    let runtime_mode = match runtime_mode {
        RuntimeMode::Lightweight => {
            warn!("macOS 暂不支持托盘，轻量模式降级为完整模式");
            RuntimeMode::Full
        }
        other => other,
    };
    // 托盘显示：CLI --no-tray 显式禁用，或配置 show_tray=false
    let no_tray = cli.no_tray || !file_show_tray;
    // CLI --no-browser 与 settings.json 的 auto_start_browser 任一关闭即不打开
    let auto_open_browser = !cli.no_browser && file_auto_open;
    // 绑定地址：CLI --host 显式指定时优先生效；否则 Docker 环境默认 0.0.0.0
    let host = cli.host.clone().or_else(|| {
        if crate::app::is_docker_env() {
            Some("0.0.0.0".to_string())
        } else {
            None
        }
    });

    Ok(AppConfig {
        port,
        host,
        base_path,
        runtime_mode,
        no_tray,
        auto_open_browser,
    })
}

/// 读取 settings.json 中的启动字段：端口、托盘显示、运行模式、自动打开浏览器
///
/// 轻量读取（不创建完整 ConfigService——正式容器在 `run()` 中统一初始化，
/// 避免重复目录 I/O / 迁移 / 解密，历史遗留 M2）。
fn read_startup_settings(base_path: &Path) -> (u16, bool, RuntimeMode, bool) {
    let default_port = crate::app::DEFAULT_PORT;
    let default_mode = RuntimeMode::Full;
    let settings_path = base_path
        .join(crate::config::CONFIG_DIR)
        .join(crate::config::SETTINGS_FILE);
    let raw = match std::fs::read_to_string(&settings_path) {
        Ok(raw) => raw,
        // 文件不存在属正常路径（首次运行），静默回退默认
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (default_port, true, default_mode, true);
        }
        Err(e) => {
            tracing::debug!(
                path = %settings_path.display(),
                error = %e,
                "读取启动配置失败，使用默认启动参数"
            );
            return (default_port, true, default_mode, true);
        }
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        tracing::debug!(
            path = %settings_path.display(),
            "settings.json 解析失败，使用默认启动参数"
        );
        return (default_port, true, default_mode, true);
    };
    let app = value.get("global").and_then(|g| g.get("app"));
    let port = app
        .and_then(|a| a.get("port"))
        .and_then(|p| p.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(default_port);
    let show_tray = app
        .and_then(|a| a.get("show_tray"))
        .and_then(|t| t.as_bool())
        .unwrap_or(true);
    // 配置里的 runtime_mode 此前从未被启动逻辑消费（设置页改了也不生效）
    let runtime_mode = match app
        .and_then(|a| a.get("runtime_mode"))
        .and_then(|m| m.as_str())
    {
        Some("lightweight") => RuntimeMode::Lightweight,
        _ => default_mode,
    };
    // 兼容迁移前的旧字段名 auto_open_browser（迁移在 ConfigService 初始化时才执行）
    let auto_open_browser = app
        .and_then(|a| {
            a.get("auto_start_browser")
                .or_else(|| a.get("auto_open_browser"))
        })
        .and_then(|t| t.as_bool())
        .unwrap_or(true);
    (port, show_tray, runtime_mode, auto_open_browser)
}

/// 检查关键目录的写入权限
fn check_directory_permissions(base_path: &Path) -> Result<()> {
    check_dir_writable(&base_path.join("config"), "config", true)?;
    check_dir_writable(&base_path.join("tasks"), "tasks", true)?;
    check_dir_writable(&base_path.join("logs"), "logs", false)?;
    check_dir_writable(&base_path.join("environment"), "environment", false)?;
    Ok(())
}

/// 检查单个目录可写性
fn check_dir_writable(path: &Path, name: &str, required: bool) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let test_file = path.join(format!(".write_test_{}", std::process::id()));
    match std::fs::write(&test_file, b"test") {
        Ok(()) => {
            let _ = std::fs::remove_file(&test_file);
            Ok(())
        }
        Err(e) if required => {
            anyhow::bail!("{name} 目录不可写 ({}): {e}", path.display());
        }
        Err(e) => {
            warn!("{name} 目录不可写 ({}): {e}（功能降级）", path.display());
            Ok(())
        }
    }
}

// ============================================================
// 实例互斥
// ============================================================

/// 获取实例锁
fn acquire_lock(base_path: &Path, force: bool) -> Result<InstanceLock> {
    match InstanceLock::try_acquire(base_path) {
        Ok(lock) => Ok(lock),
        Err(_) if force => {
            warn!("已有实例运行中，--force 终止...");
            if let Some(info) = crate::utils::lock::query_instance(base_path) {
                if info.running {
                    crate::utils::lock::force_kill(info.pid);
                    for retry in 0..5 {
                        std::thread::sleep(std::time::Duration::from_millis(200 * (retry + 1)));
                        if let Ok(lock) = InstanceLock::try_acquire(base_path) {
                            return Ok(lock);
                        }
                    }
                    anyhow::bail!("强制终止后无法获取锁");
                }
            }
            if let Err(e) = std::fs::remove_file(base_path.join("config").join(".instance")) {
                tracing::debug!(error = %e, "清理残留的 .instance 锁文件失败");
            }
            InstanceLock::try_acquire(base_path).context("强制终止后仍无法获取锁")
        }
        Err(e) => Err(e).context("无法获取实例锁（已有实例在运行，使用 --force 强制终止）"),
    }
}

/// 重启场景：等待旧进程释放锁
async fn wait_for_lock_release(base_path: &Path) -> Result<()> {
    let timeout = std::time::Duration::from_secs(30);
    let interval = std::time::Duration::from_millis(200);
    let start = std::time::Instant::now();

    info!("重启场景：等待旧进程释放实例锁...");
    loop {
        match InstanceLock::try_acquire(base_path) {
            Ok(lock) => {
                drop(lock);
                return Ok(());
            }
            Err(_) if start.elapsed() < timeout => {
                tokio::time::sleep(interval).await;
            }
            Err(_) => {
                error!("等待旧进程释放锁超时（30 秒），本次启动中止");
                anyhow::bail!("等待旧进程释放锁超时（30 秒）");
            }
        }
    }
}

// ============================================================
// 模式分发
// ============================================================

/// 完整模式：全部服务 + Axum + 托盘，阻塞等待退出信号
async fn launch_full(state: &mut LauncherState) -> Result<()> {
    // 容器缺失属逻辑不变量违反，防御性返回错误而非 panic（历史遗留 #11）
    let container = state
        .container
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("服务容器尚未初始化"))?
        .clone();

    // 启动 Axum
    match app::start_axum(
        container.clone(),
        state.log_tx.clone(),
        state.app_config.port,
        state.app_config.host.as_deref(),
    )
    .await
    {
        Ok(handle) => {
            let port = handle.port;
            if let Some(ref lock) = state.instance_lock {
                if let Err(e) = lock.record_port(port) {
                    warn!(port = port, error = %e, "记录运行端口到实例锁失败");
                }
            }
            state.axum_handle = Some(handle);
            let bind_host = state.app_config.host.as_deref().unwrap_or("127.0.0.1");
            // Docker 环境绑定 0.0.0.0 时，提示地址仍显示为可访问的 host:port
            let display_host = if bind_host == "0.0.0.0" {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            };
            info!("Web 控制台已启动: http://{display_host}:{port}");

            // CLI --no-browser 与设置项 app.auto_start_browser 任一关闭即不打开：
            // 此前配置项是死开关，UI「静默启动」切换无任何效果
            if state.app_config.auto_open_browser {
                open_browser(port);
            }
        }
        Err(e) => {
            let msg = e.to_string();
            // 中英双语端口占用检测
            if msg.contains("被占用") || msg.contains("address in use") || msg.contains("AddrInUse")
            {
                anyhow::bail!("端口 {} 被占用: {e}", state.app_config.port);
            }
            warn!("Axum 启动失败 ({e})，降级到轻量模式");
        }
    }

    // Engine 崩溃恢复
    let watch_handle = watch_engine(state);

    // 启动托盘（在专用 OS 线程上构建图标与菜单）
    // macOS 的禁用拦截收敛在 TrayManager::spawn 内部单点执行（W6 用户决策）
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    // 按 startup_action 派发启动动作（Monitor / LoginOnce）
    if let Some(container) = state.container.as_ref() {
        apply_startup_action(container).await;
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

    Ok(())
}

/// 轻量模式：仅引擎 + 托盘（Axum 按需启动）
async fn launch_lightweight(state: &mut LauncherState) -> Result<()> {
    let watch_handle = watch_engine(state);

    // 启动托盘（轻量模式也显示托盘；macOS 由 spawn 内部单点拦截返回空句柄）
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    if let Some(container) = state.container.as_ref() {
        apply_startup_action(container).await;
    }

    // G15：轻量模式下此刻 Axum 尚未监听（按需启动），记录哨兵端口 0——
    // `.instance` 文件仍提供 PID/存活状态供 --status 查询，但 --stop 不会向
    // 未监听端口发无效请求。真实端口在 Axum 绑定后由托盘按需启动路径
    // （write_instance_port）回写。
    if let Some(ref lock) = state.instance_lock {
        if let Err(e) = lock.record_port(0) {
            tracing::debug!(error = %e, "记录哨兵端口 0 失败（--status 将看不到端口）");
        }
    }

    spawn_background_update_check(state);

    spawn_auto_restart_timer(state);

    info!("轻量模式运行中（Axum 按需启动）");
    wait_for_shutdown(state).await;
    watch_handle.abort();

    Ok(())
}

/// 按 settings.global.app.startup_action 派发启动动作
///
/// 该配置此前只有写入点（CLI --startup-action / autostart API），从未被启动
/// 逻辑消费——默认值 Monitor 的语义"启动后进入监测"实际从未生效，
/// Engine 一直以 monitoring=false 空转等待用户手动触发。
async fn apply_startup_action(container: &Arc<ServiceContainer>) {
    let settings = container.config.load_settings();
    match settings.global.app.startup_action {
        StartupAction::Monitor => {
            match container
                .engine
                .dispatch(crate::engine::EngineCommand::Start)
                .await
            {
                Ok(()) => info!("按 startup_action=monitor 已启动监测"),
                Err(e) => warn!("按 startup_action 启动监测失败: {e:?}"),
            }
        }
        StartupAction::LoginOnce => {
            info!("按 startup_action=login_once 触发单次登录");
            let orchestrator = container.login.clone();
            tokio::spawn(async move {
                let handle = orchestrator
                    .submit(crate::status::LoginSource::LoginOnce, None, None)
                    .await;
                let result = handle.await_result().await;
                if result.success {
                    info!(message = %result.message, "启动单次登录成功");
                } else {
                    warn!(message = %result.message, "启动单次登录失败");
                }
            });
        }
        StartupAction::None => {
            tracing::debug!("startup_action=None，不派发启动动作");
        }
    }
}

/// 单次登录模式：执行一次登录后退出
async fn launch_login_once(state: &mut LauncherState) -> Result<()> {
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
        success = result.success,
        message = %result.message,
        duration = ?result.duration,
        "单次登录完成"
    );

    if !result.success {
        anyhow::bail!("登录失败: {}", result.message);
    }
    Ok(())
}

// ============================================================
// Engine 崩溃恢复
// ============================================================

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
fn watch_engine(state: &LauncherState) -> JoinHandle<()> {
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

            // 通知 Orchestrator 取消 source=auto 的在途登录
            orchestrator.cancel_auto_pending("engine_crashed").await;

            restart_count += 1;
            if restart_count > MAX_RESTART_ATTEMPTS {
                error!("Engine 重启次数耗尽（{MAX_RESTART_ATTEMPTS} 次），标记为 Dead");
                // 清空 slot，后续命令派发按 ChannelClosed 快速失败
                container.engine.clear();
                status.merge(crate::status::PartialSnapshot::Engine {
                    state: crate::status::EngineState::Dead,
                    network: crate::status::NetworkStatus::Offline,
                    last_check: chrono::Local::now(),
                    pause: false,
                    cooling_down: false,
                    cooling_down_remaining: None,
                    consecutive_failures: 0,
                });
                return;
            }

            info!(
                attempt = restart_count,
                max = MAX_RESTART_ATTEMPTS,
                "Engine 重启中（{}s 后）...",
                RESTART_DELAY_SECS
            );
            // 等待重启延迟，期间若收到 shutdown 则提前退出
            tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS)) => {}
            }

            // 从 ConfigService 重新加载配置，内部状态重置为默认值
            let deps = EngineDeps {
                config_service: config.clone(),
                profile_service: profile_service.clone(),
                orchestrator: orchestrator.clone(),
                status_manager: status.clone(),
                monitor_service: monitor_service.clone(),
                network_detect: network_detect.clone(),
            };
            let new_handle = crate::engine::Engine::spawn(deps);
            let new_completed = new_handle.completed.clone();

            // 引用收口：新句柄原子换入 slot，Web/托盘/关闭流程即刻指向新 Engine
            container.engine.replace(new_handle);

            // 按崩溃前状态恢复监测（monitoring=false 空转会让用户以为还在监测）
            if was_monitoring {
                match container
                    .engine
                    .dispatch(crate::engine::EngineCommand::Start)
                    .await
                {
                    Ok(()) => info!("Engine（重启 #{restart_count}）已按崩溃前状态恢复监测"),
                    Err(e) => warn!("Engine（重启 #{restart_count}）恢复监测失败: {e:?}"),
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

// ============================================================
// 优雅关闭
// ============================================================

/// 按逆序关闭所有服务
async fn graceful_shutdown(state: &mut LauncherState) {
    info!("正在关闭...");

    // 0. 立即取消应用级关闭令牌：
    // - wait_for_shutdown 各监听分支（若尚未返回）被唤醒
    // - watch_engine 收到 Engine 完成通知后据此区分"应用关闭"与"Engine 崩溃"，
    //   避免关闭流程中被重启出新 Engine（托盘退出曾因此留下无托盘的僵尸进程）
    state.shutdown_token.cancel();

    // 1. 关闭 TrayManager（先停泵任务，再 drop）
    if let Some(handle) = state.tray_handle.take() {
        handle
            .stop_with_timeout(std::time::Duration::from_secs(3))
            .await;
    }
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

    // 6. 清理运行端口文件
    if let Some(container) = &state.container {
        let port_file = container
            .config
            .base_path()
            .join("config")
            .join(app::RUNTIME_PORT_FILE);
        let _ = std::fs::remove_file(&port_file);
    }

    // 7. 释放 ServiceContainer
    state.container = None;

    // 8. 释放实例锁（drop InstanceLock -> 文件关闭 -> 锁自动释放）
    state.instance_lock = None;

    info!("已关闭");

    // 9. 释放日志 guard（flush 剩余日志后关闭文件句柄，必须在最后一条日志之后）
    state._log_guard = None;
}

// ============================================================
// 辅助函数
// ============================================================

/// 打开系统默认浏览器
fn open_browser(port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    if let Err(e) = open::that(&url) {
        warn!("打开浏览器失败 ({url}): {e}");
    } else {
        info!("已打开浏览器: {url}");
    }
}

/// 启动后台更新检查任务
fn spawn_background_update_check(state: &LauncherState) {
    if let Some(container) = &state.container {
        container
            .updater
            .start_background_check(state.shutdown_token.clone());
    }
}

/// 启动定时自重启计时任务（full / lightweight 模式）
///
/// 每分钟读取一次 `app.auto_restart_hours` 与运行时长（Metrics.uptime_seconds），
/// 达到阈值即复用 `POST /api/system/restart` 同款机制：先 spawn 带 `--restarting`
/// 的后继进程（它会等待本进程释放实例锁），再取消应用级关闭令牌走优雅关闭，
/// 并武装 30s 退出 watchdog。配置运行时修改无需重启即生效。
fn spawn_auto_restart_timer(state: &LauncherState) {
    // 单次登录模式进程本就即用即退，无需定时重启
    if matches!(state.app_config.runtime_mode, RuntimeMode::LoginOnce) {
        return;
    }
    let Some(container) = state.container.clone() else {
        return;
    };
    let token = state.shutdown_token.clone();
    tokio::spawn(async move {
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
            if let Err(e) = spawn_restart_successor() {
                // 后继进程启动失败时放弃本轮重启（保持当前进程运行），避免每分钟重试刷屏
                error!("定时自重启：启动后继进程失败（{e}），本次已放弃，保持当前进程运行");
                return;
            }
            spawn_exit_watchdog(30);
            token.cancel();
            return;
        }
    });
}

/// 定时自重启触发判定（纯函数，便于单测）
///
/// `hours == 0` 表示未启用；运行时长达到 `hours * 3600` 秒即触发。
fn should_auto_restart(hours: u32, uptime_secs: u64) -> bool {
    hours > 0 && uptime_secs >= u64::from(hours) * 3600
}

/// spawn 带 `--restarting` 标记的后继进程（定时自重启与 Web 重启接口共用）///
/// 后继进程会先等待本进程释放实例锁再启动（见 [`wait_for_lock_release`]），
/// 因此调用方必须**先 spawn 后继、再触发本进程优雅关闭**。
pub(crate) fn spawn_restart_successor() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取可执行文件路径失败: {e}"))?;
    // args_os：参数含非法 Unicode 时 env::args() 会 panic，args_os 不会
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    args.retain(|a| a != "--restarting");
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

/// 生成退出 watchdog：优雅关闭超时后强制 `exit(0)`，作为最后防线。
///
/// Web 重启/关闭接口与定时自重启共用，统一为 30s，
/// 覆盖优雅关闭总预算（Tray 3s + Scheduler 5s + Engine 5s + Bridge 8s + Axum 5s ≈ 26s），
/// 避免强杀过早残留浏览器/子进程（A4）。
pub(crate) fn spawn_exit_watchdog(secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        tracing::warn!("优雅关闭超时 {secs}s，强制退出");
        std::process::exit(0);
    });
}

/// 等待退出信号（Ctrl+C / SIGTERM/SIGHUP / 托盘退出 / Web API 关闭）
async fn wait_for_shutdown(state: &mut LauncherState) {
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
