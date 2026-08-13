//! 启动状态机：配置加载、目录权限、实例互斥、服务启动、模式分发、Engine 崩溃恢复、优雅关闭
//!
//! 三种运行模式：
//! - **full**：全部服务 + Axum + 托盘
//! - **lightweight**：仅引擎 + 托盘（Axum 按需启动）
//! - **login_once**：执行一次登录后退出

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::*;

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
    /// 启动模式：完整 / 轻量 / 单次登录
    #[arg(short, long, value_enum, default_value_t = RuntimeMode::Full)]
    pub mode: RuntimeMode,

    /// 基准路径（配置文件 / 任务 / 日志 的根目录）
    #[arg(long)]
    pub base_path: Option<PathBuf>,

    /// 监听端口（默认从 settings.json 读取）
    #[arg(short, long)]
    pub port: Option<u16>,

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
    base_path: PathBuf,
    runtime_mode: RuntimeMode,
    no_browser: bool,
    no_tray: bool,
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
    log_tx: tokio::sync::broadcast::Sender<crate::web::state::LogEntry>,
    /// 当前活跃 Engine 的命令发送端（崩溃恢复后更新，优雅关闭时使用）
    latest_engine_cmd_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<crate::engine::EngineCommand>>>>,
    /// 日志文件非阻塞写入的 WorkerGuard，优雅关闭时 drop 以 flush 剩余日志
    _log_guard: Option<WorkerGuard>,
}

// ============================================================
// 入口
// ============================================================

/// 启动编排主入口（由 `main.rs` 调用）
pub async fn run(cli: CliArgs, base_path: PathBuf) -> Result<()> {
    // 1. 配置合并
    let app_config = load_and_merge_config(&cli, base_path).await?;

    // 2. 目录权限检查
    check_directory_permissions(&app_config.base_path)?;

    // 3. 重启等待
    if cli.restarting {
        wait_for_lock_release(&app_config.base_path).await?;
    }

    // 4. 实例锁
    let instance_lock = acquire_lock(&app_config.base_path, cli.force)?;

    // 5. 日志广播通道 + 文件日志层
    let log_tx = log_broadcast_tx();
    let log_guard = init_file_logging(&app_config.base_path, log_tx.clone());

    // 6. 引导服务容器
    info!("正在初始化服务...");
    let (container, handles) = ServiceContainer::new(&app_config.base_path).await?;
    info!("服务容器初始化完成");

    // 7. CLI --startup-action 覆盖配置文件
    if let Some(action) = cli.startup_action {
        let mut settings = container.config.load_settings();
        if settings.global.app.startup_action != action {
            settings.global.app.startup_action = action;
            if let Err(e) = container.config.save_settings(&settings).await {
                tracing::warn!("保存 startup_action 配置失败: {e}");
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
        shutdown_token: CancellationToken::new(),
        log_tx,
        latest_engine_cmd_tx: Arc::new(tokio::sync::Mutex::new(Some(
            container.engine_handle.engine.cmd_sender(),
        ))),
        _log_guard: Some(log_guard),
    };

    // 8. 创建系统托盘
    if !state.app_config.no_tray {
        let tray = TrayManager::new(crate::tray::TrayDeps {
            config: container.config.clone(),
            status: container.status.clone(),
            engine: container.engine_handle.engine.clone(),
            profile_service: container.profiles.clone(),
            updater: container.updater.clone(),
            orchestrator: container.login.clone(),
            container: container.clone(),
            log_tx: state.log_tx.clone(),
            port: state.app_config.port,
            lightweight: matches!(state.app_config.runtime_mode, RuntimeMode::Lightweight),
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
async fn load_and_merge_config(cli: &CliArgs, base_path: PathBuf) -> Result<AppConfig> {
    // 临时创建 ConfigService 读取 settings.json
    let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(1);
    let config_service = crate::config::ConfigService::new(base_path.clone(), reload_tx).await?;
    let settings = config_service.load_settings();

    let port = cli.port.unwrap_or(settings.global.app.port as u16).max(1);
    let runtime_mode = cli.mode.clone();
    let no_browser = cli.no_browser;
    // 托盘显示：CLI --no-tray 显式禁用，或配置 show_tray=false
    let no_tray = cli.no_tray || !settings.global.app.show_tray;

    Ok(AppConfig {
        port,
        base_path,
        runtime_mode,
        no_browser,
        no_tray,
    })
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
            let _ = std::fs::remove_file(base_path.join("config").join(".instance"));
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
                anyhow::bail!("等待旧进程释放锁超时（30 秒）");
            }
        }
    }
}

// ============================================================
// 日志
// ============================================================

/// 自定义日志计时器：YYYY-MM-DD HH:MM:SS 本地时间
#[derive(Clone)]
struct LocalTimer;

/// 全局日志 filter（`Targets` 支持运行时动态调整 target 级别，实现 set_log_level 热更新）
static LOG_TARGETS: OnceLock<tracing_subscriber::filter::Targets> = OnceLock::new();

/// 热更新全局日志级别（由 `set_log_level` 调用）
///
/// 第三方库保持 WARN，本项目 target（campus_auth/frontend）设为指定级别。无效级别回退 INFO。
pub fn reload_log_level(level: &str) {
    use tracing_subscriber::filter::LevelFilter;

    let lf = match level.to_ascii_uppercase().as_str() {
        "TRACE" => LevelFilter::TRACE,
        "DEBUG" => LevelFilter::DEBUG,
        "WARN" | "WARNING" => LevelFilter::WARN,
        "ERROR" => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    };
    let Some(targets) = LOG_TARGETS.get() else {
        tracing::warn!("日志 filter 未初始化，忽略级别切换");
        return;
    };
    // clone 后链式调用：Targets 内部 Arc 指向同一共享状态，with_target 更新共享 filter
    let _ = targets
        .clone()
        .with_target("campus_auth", lf)
        .with_target("frontend", lf);
    tracing::info!("日志级别已热更新为 {}", lf);
}

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))
    }
}

/// 从 settings.json 读取日志保留天数（`global.logging.retention_days`），失败回退默认 7
fn read_retention_days(base_path: &Path) -> u32 {
    let path = base_path.join("config").join("settings.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("global")?
                .get("logging")?
                .get("retention_days")?
                .as_u64()
        })
        .map(|d| d as u32)
        .unwrap_or(7)
}

/// 删除 logs/ 目录下超过保留天数的旧日志文件
///
/// 仅删除修改时间早于 cutoff 的 `.log` 文件，跳过当前正在写入的 `app.log`
/// （`tracing_appender::rolling::daily` 生成 `app.log.YYYY-MM-DD` 轮转文件）。
fn cleanup_old_logs(logs_dir: &Path, retention_days: u32) {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::from(retention_days) * 86_400);
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // 仅处理日志文件，跳过当前活跃文件
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "app.log" || !name.starts_with("app.log") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    if removed > 0 {
        tracing::info!("清理过期日志文件 {} 个（保留 {} 天）", removed, retention_days);
    }
}

/// 初始化日志系统：控制台层 + 文件层（按日期轮转）+ 广播层（WebSocket 推送）
///
/// 全局 subscriber 只能 init 一次，所有层在此统一注册。
fn init_file_logging(
    base_path: &Path,
    log_tx: tokio::sync::broadcast::Sender<crate::web::state::LogEntry>,
) -> WorkerGuard {
    let logs_dir = base_path.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);

    // 启动时清理过期日志：按 settings.json 的 logging.retention_days 保留，
    // 删除超过保留天数的旧轮转文件，避免日志无限累积（对齐原项目 loguru retention）。
    cleanup_old_logs(&logs_dir, read_retention_days(base_path));

    // 动态 filter：`Targets` 支持运行时调整级别（set_log_level 热更新）。
    // 第三方库默认 WARN，本项目 target（campus_auth/frontend）默认 INFO。
    let targets = tracing_subscriber::filter::Targets::new()
        .with_default(tracing_subscriber::filter::LevelFilter::WARN)
        .with_target("campus_auth", tracing_subscriber::filter::LevelFilter::INFO)
        .with_target("frontend", tracing_subscriber::filter::LevelFilter::INFO);
    let _ = LOG_TARGETS.set(targets.clone());

    // 本地时区计时器：YYYY-MM-DD HH:MM:SS 格式
    let local_timer = LocalTimer;

    // 控制台层：人类可读格式输出到 stderr
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr)
        .with_timer(local_timer.clone())
        .with_filter(targets.clone());

    // 文件层：JSON 格式按日轮转
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "app.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_timer(local_timer.clone())
        .json()
        .with_filter(targets.clone());

    // 广播层：将 tracing 事件转发到 broadcast channel 供 WebSocket 推送
    let broadcast_layer = tracing_subscriber::fmt::layer()
        .with_writer(BroadcastWriter::new(log_tx))
        .with_ansi(false)
        .with_target(true)
        .without_time()
        .with_filter(targets);

    if let Err(e) = tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .with(broadcast_layer)
        .try_init()
    {
        eprintln!("日志层注册失败（可能已初始化）: {e}");
    }

    guard
}

/// 广播写入器：将格式化的日志发送到 broadcast channel
struct BroadcastWriter {
    tx: tokio::sync::broadcast::Sender<crate::web::state::LogEntry>,
}

impl BroadcastWriter {
    fn new(tx: tokio::sync::broadcast::Sender<crate::web::state::LogEntry>) -> Self {
        Self { tx }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BroadcastWriter {
    type Writer = BroadcastWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        BroadcastWriterGuard {
            tx: self.tx.clone(),
            buf: Vec::new(),
        }
    }
}

/// 写入器守卫：缓冲写入，flush 或 drop 时发送到广播通道
struct BroadcastWriterGuard {
    tx: tokio::sync::broadcast::Sender<crate::web::state::LogEntry>,
    buf: Vec<u8>,
}

impl BroadcastWriterGuard {
    /// 发送缓冲的日志（无剩余数据或非 UTF-8 时跳过）
    fn send_buffered(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = match String::from_utf8(std::mem::take(&mut self.buf)) {
            Ok(t) => t,
            Err(_) => return,
        };
        let entry = parse_log_line(&text);
        let _ = self.tx.send(entry);
    }
}

impl std::io::Write for BroadcastWriterGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffered();
        Ok(())
    }
}

/// 关键修复：`tracing-subscriber` 的 fmt layer 在 `on_event` 里只调用
/// `io::Write::write_all`，**不调用 `flush`**。若仅在 flush 里发送，日志实时推送会
/// 完全失效（WebSocket 收不到任何后端日志）。因此改为在 Drop 里兜底发送，
/// flush 仍保留以兼容可能显式调用 flush 的路径。
impl Drop for BroadcastWriterGuard {
    fn drop(&mut self) {
        self.send_buffered();
    }
}

/// 解析 tracing fmt 层输出为 LogEntry
///
/// tracing fmt 层（with_target(true).without_time().with_ansi(false)）输出格式：
/// `  INFO campus_auth::launcher: 正在初始化服务...`
/// `  WARN campus_auth::bridge: 某警告: key=value`
/// 也兼容 `[LEVEL target] message` 和裸消息格式。
fn parse_log_line(line: &str) -> crate::web::state::LogEntry {
    let trimmed = line.trim();
    let now = chrono::Local::now().to_rfc3339();

    // 格式 1：[LEVEL target] message
    if trimmed.starts_with('[') {
        if let Some(end_bracket) = trimmed.find(']') {
            let header = &trimmed[1..end_bracket];
            let message = trimmed[end_bracket + 1..].trim().to_string();
            let parts: Vec<&str> = header.splitn(2, ' ').collect();
            let level = parts.first().unwrap_or(&"INFO").to_string();
            let source = crate::web::state::normalize_source(parts.get(1).copied().unwrap_or(""));
            return crate::web::state::LogEntry {
                level,
                message,
                timestamp: now,
                source,
            };
        }
    }

    // 格式 2：tracing fmt 层输出 "LEVEL target: message"
    // LEVEL 是 TRACE/DEBUG/INFO/WARN/ERROR，前面可能有空格
    let upper = trimmed.to_uppercase();
    for lvl in ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] {
        if upper.starts_with(lvl) {
            let rest = trimmed[lvl.len()..].trim_start();
            // rest 形如 "campus_auth::launcher: 消息"
            if let Some(colon_pos) = rest.find(": ") {
                let source = crate::web::state::normalize_source(rest[..colon_pos].trim());
                let message = rest[colon_pos + 2..].trim().to_string();
                return crate::web::state::LogEntry {
                    level: lvl.to_string(),
                    message,
                    timestamp: now,
                    source,
                };
            }
            // 只有 LEVEL 无 ": " 分隔，rest 全部作为 message
            return crate::web::state::LogEntry {
                level: lvl.to_string(),
                message: rest.to_string(),
                timestamp: now,
                source: String::new(),
            };
        }
    }

    // 回退：整行作为消息
    crate::web::state::LogEntry {
        level: "INFO".to_string(),
        message: trimmed.to_string(),
        timestamp: now,
        source: String::new(),
    }
}

// ============================================================
// 模式分发
// ============================================================

/// 完整模式：全部服务 + Axum + 托盘，阻塞等待退出信号
async fn launch_full(state: &mut LauncherState) -> Result<()> {
    let container = state.container.as_ref().unwrap().clone();

    // 启动 Axum
    match app::start_axum(container.clone(), state.log_tx.clone(), state.app_config.port).await {
        Ok(handle) => {
            let port = handle.port;
            if let Some(ref lock) = state.instance_lock {
                let _ = lock.record_port(port);
            }
            state.axum_handle = Some(handle);
            info!("Web 控制台已启动: http://127.0.0.1:{port}");

            if !state.app_config.no_browser {
                open_browser(port);
            }
        }
        Err(e) => {
            let msg = e.to_string();
            // 中英双语端口占用检测
            if msg.contains("被占用") || msg.contains("address in use") || msg.contains("AddrInUse") {
                anyhow::bail!("端口 {} 被占用: {e}", state.app_config.port);
            }
            warn!("Axum 启动失败 ({e})，降级到轻量模式");
        }
    }

    // Engine 崩溃恢复
    let watch_handle = watch_engine(state);

    // 启动托盘（在专用 OS 线程上构建图标与菜单）
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    // 后台更新检查
    spawn_background_update_check(state);

    // 事件循环
    wait_for_shutdown(state).await;
    watch_handle.abort();

    Ok(())
}

/// 轻量模式：仅引擎 + 托盘（Axum 按需启动）
async fn launch_lightweight(state: &mut LauncherState) -> Result<()> {
    let watch_handle = watch_engine(state);

    // 启动托盘（轻量模式也显示托盘）
    if let Some(tray) = state.tray_manager.as_ref() {
        state.tray_handle = Some(tray.spawn());
    }

    if let Some(ref lock) = state.instance_lock {
        let _ = lock.record_port(state.app_config.port);
    }

    spawn_background_update_check(state);

    info!("轻量模式运行中（Axum 按需启动）");
    wait_for_shutdown(state).await;
    watch_handle.abort();

    Ok(())
}

/// 单次登录模式：执行一次登录后退出
async fn launch_login_once(state: &mut LauncherState) -> Result<()> {
    let container = state.container.as_ref().unwrap();

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
/// - 初始 Engine 通过 `Arc<ServiceContainer>` 持有，无法直接消费 JoinHandle，
///   使用 `is_finished()` 轮询检测完成。
/// - 重启的 Engine 拥有 `EngineHandle` 所有权，通过 `into_result()` 区分
///   panic（Err）与正常退出（Ok），仅在 panic 时继续重启。
fn watch_engine(state: &LauncherState) -> JoinHandle<()> {
    let container = state.container.as_ref().unwrap().clone();
    let status = container.status.clone();
    let config = container.config.clone();
    let profile_service = container.profiles.clone();
    let orchestrator = container.login.clone();
    let monitor_service = container.monitor.clone();
    let network_detect = crate::network::detect::create_detector();
    let base_path = container.config.base_path();
    let latest_engine_cmd_tx = state.latest_engine_cmd_tx.clone();
    let shutdown_token = state.shutdown_token.clone();

    tokio::spawn(async move {
        // 使用 Notify 零延迟等待初始 Engine 完成（替代 1s 轮询 is_finished）。
        // 同时监听 shutdown 信号，避免应用关闭时持续阻塞在此等待。
        tokio::select! {
            biased;
            _ = shutdown_token.cancelled() => return,
            _ = container.engine_handle.completed.notified() => {}
        }

        let mut restart_count: u32 = 0;

        loop {
            // 应用关闭：停止重启循环
            if shutdown_token.is_cancelled() {
                return;
            }

            // 通知 Orchestrator 取消 source=auto 的在途登录
            orchestrator.cancel_auto_pending("engine_crashed").await;

            restart_count += 1;
            if restart_count > MAX_RESTART_ATTEMPTS {
                error!("Engine 重启次数耗尽（{MAX_RESTART_ATTEMPTS} 次），标记为 Dead");
                // 清空共享 cmd_tx，避免 graceful_shutdown 向已死 Engine 发送命令
                *latest_engine_cmd_tx.lock().await = None;
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
                base_path: base_path.clone(),
            };
            let new_handle = crate::engine::Engine::spawn(deps);

            // 更新共享 cmd_tx，使 graceful_shutdown 能向新 Engine 发送 Shutdown
            *latest_engine_cmd_tx.lock().await = Some(new_handle.engine.cmd_sender());

            // 通过 into_result() 获取精确的退出结果，区分 panic 与正常退出。
            // 同时监听 shutdown：收到关闭信号时不再继续重启（新 Engine 由
            // graceful_shutdown 通过 latest_engine_cmd_tx 发送 Shutdown）。
            let exit_result = tokio::select! {
                biased;
                _ = shutdown_token.cancelled() => return,
                r = new_handle.into_result() => r,
            };
            match exit_result {
                Err(e) => {
                    error!("Engine（重启 #{restart_count}）task panic: {e}");
                    // 继续循环，尝试下一次重启
                }
                Ok(()) => {
                    // 正常退出（可能收到 Shutdown 命令），无需继续重启
                    info!("Engine（重启 #{restart_count}）正常退出");
                    return;
                }
            }
        }
    })
}

// ============================================================
// 优雅关闭
// ============================================================

/// 按逆序关闭所有服务
async fn graceful_shutdown(state: &mut LauncherState) {
    info!("正在关闭...");

    // 1. 关闭 TrayManager（先停泵任务，再 drop）
    if let Some(handle) = state.tray_handle.take() {
        if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(3), handle.stop()).await {
            warn!("TrayManager 关闭超时: {e}");
        }
    }
    if let Some(tray) = state.tray_manager.take() {
        drop(tray);
    }

    // 2. 关闭 SchedulerService（并保留 bridge_handle 供第 4 步统一关闭）
    let bridge_handle = if let Some(handles) = state.startup_handles.take() {
        if let Err(e) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handles.scheduler_handle.stop(),
        )
        .await
        {
            warn!("SchedulerService 关闭超时: {e}");
        }
        Some(handles.bridge_handle)
    } else {
        None
    };

    // 3. 关闭 Engine（先于 Bridge，因为 Engine 可能正在使用 Bridge）
    if let Some(tx) = state.latest_engine_cmd_tx.lock().await.take() {
        let _ = tx.send(crate::engine::EngineCommand::Shutdown).await;
    }

    // 4. 关闭 BridgeSupervisor：停止 supervisor 主循环（内部回收 Worker）并等待 task 退出。
    // 通过 ServiceHandle::stop 发送停止信号并 join，避免 supervisor task 与残留子进程泄漏
    // （历史遗留 F4：原实现仅发 shutdown 命令、从不调用 stop，run_supervisor task 常驻）。
    if let Some(handle) = bridge_handle {
        if tokio::time::timeout(std::time::Duration::from_secs(8), handle.stop())
            .await
            .is_err()
        {
            warn!("BridgeSupervisor 关闭超时");
        }
    }

    // 5. 关闭 Axum
    if let Some(handle) = state.axum_handle.take() {
        if tokio::time::timeout(std::time::Duration::from_secs(5), app::stop_axum(handle))
            .await
            .is_err()
        {
            warn!("Axum 关闭超时，强制中止");
        }
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
// 重启
// ============================================================

/// 重启：spawn 新进程 + --restarting 标记，当前进程优雅退出
pub(crate) async fn _restart(_state: &LauncherState) -> Result<()> {
    let exe = std::env::current_exe().context("无法获取当前 exe 路径")?;
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    args.retain(|a| a != "--restarting");
    args.push("--restarting".to_string());

    info!("正在重启（spawn 新进程）...");
    std::process::Command::new(&exe)
        .args(&args)
        .spawn()
        .context("无法 spawn 新进程")?;

    Ok(())
}

// ============================================================
// 辅助函数
// ============================================================

/// 创建日志广播通道的发送端
fn log_broadcast_tx() -> tokio::sync::broadcast::Sender<crate::web::state::LogEntry> {
    let (tx, _) = tokio::sync::broadcast::channel(1024);
    tx
}

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

/// 等待退出信号（Ctrl+C / 托盘退出 / Web API 关闭）
async fn wait_for_shutdown(state: &mut LauncherState) {
    let token = state.shutdown_token.clone();
    // 克隆 shutdown_rx 而非 take 出 handle，避免 drop stop_tx 导致 Axum 过早关闭
    let shutdown_rx = state.axum_handle.as_ref().map(|h| h.shutdown_rx.clone());
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("收到 Ctrl+C 信号");
        }
        _ = token.cancelled() => {
            info!("收到关闭令牌取消信号");
        }
        _ = async {
            if let Some(mut rx) = shutdown_rx {
                let _ = rx.changed().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            info!("收到 Web API 关闭信号");
        }
    }
}
