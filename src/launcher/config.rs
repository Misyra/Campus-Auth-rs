//! 启动配置合并：defaults -> settings.json -> CLI 覆盖，及关键目录写权限预检

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::AppConfig;
use super::cli::{CliArgs, RuntimeMode};

/// 加载并合并配置：defaults -> settings.json -> CLI 覆盖
pub(super) fn load_and_merge_config(cli: &CliArgs, base_path: PathBuf) -> Result<AppConfig> {
    // 轻量读取 settings.json 的启动字段，不创建完整 ConfigService——
    // 正式容器在 run() 中统一初始化，避免重复目录 I/O / 迁移 / 解密（历史遗留 M2）。
    let startup = read_startup_settings(&base_path);

    let port = cli.port.unwrap_or(startup.port).max(1);
    // CLI --mode 显式指定时优先生效；缺省时沿用 settings.json 的 app.runtime_mode
    let runtime_mode = cli.mode.clone().unwrap_or(startup.mode);
    // macOS：托盘已禁用（用户决策，W6——tray-icon 要求主线程 NSApplication
    // 事件循环，主线程运行 tokio runtime 无法满足）；轻量模式在 mac 上会既无
    // 托盘也无 Web 入口，统一降级为完整模式保证可用
    #[cfg(any(
        target_os = "macos",
        not(feature = "desktop"),
        feature = "openwrt",
        feature = "android"
    ))]
    let runtime_mode = match runtime_mode {
        RuntimeMode::Lightweight => {
            // 此刻 tracing subscriber 尚未初始化（init_logging 在 run() 中才执行），
            // tracing::warn 是 no-op、降级原因恒不可见——必须走 stderr 同步输出（P3-15）
            eprintln!("当前构建不支持托盘，轻量模式降级为完整模式");
            RuntimeMode::Full
        }
        other => other,
    };
    // 托盘显示：CLI --no-tray 显式禁用，或配置 show_tray=false
    #[cfg(feature = "desktop")]
    let no_tray = cli.no_tray || !startup.show_tray;
    // CLI --no-browser 与 settings.json 的 auto_start_browser 任一关闭即不打开
    let auto_open_browser = !cfg!(any(feature = "openwrt", feature = "android"))
        && !cli.no_browser
        && startup.auto_open_browser;
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
        #[cfg(feature = "desktop")]
        no_tray,
        auto_open_browser,
        log_level: startup.log_level,
        log_retention_days: startup.log_retention_days,
        log_file_enabled: startup.log_file_enabled,
        management_password: startup.management_password,
        management_password_override: cli.management_password.clone(),
        allow_unauthenticated_bind: cli.allow_unauthenticated_bind,
    })
}

/// 预读的启动子集（settings.json 单次解析产物）
///
/// 历史包袱：启动字段 / 日志级别 / 保留天数曾由三处各自读文件解析。
/// 现由 [`read_startup_settings`] 一次读出，日志配置透传 `init_logging`，
/// 避免 `launcher` / `logging` 重复 I/O。
struct StartupSettings {
    port: u16,
    #[cfg(feature = "desktop")]
    show_tray: bool,
    mode: RuntimeMode,
    auto_open_browser: bool,
    log_level: tracing_subscriber::filter::LevelFilter,
    log_retention_days: u32,
    log_file_enabled: bool,
    /// 管理密码（settings.json 的 app.management_password，供凭据门禁启动期读取）
    management_password: String,
}

/// 读取 settings.json 中的启动字段：端口、托盘显示、运行模式、自动打开浏览器 + 日志配置
///
/// 轻量读取（不创建完整 ConfigService——正式容器在 `run()` 中统一初始化，
/// 避免重复目录 I/O / 迁移 / 解密，历史遗留 M2）。
/// 单次文件读取 + 单次 JSON 解析，日志部分委托 `logging::logging_config_from_value`。
fn read_startup_settings(base_path: &Path) -> StartupSettings {
    use tracing_subscriber::filter::LevelFilter;
    let default = StartupSettings {
        port: crate::app::DEFAULT_PORT,
        #[cfg(feature = "desktop")]
        show_tray: true,
        mode: RuntimeMode::Full,
        auto_open_browser: true,
        log_level: LevelFilter::INFO,
        log_retention_days: 7,
        log_file_enabled: true,
        management_password: String::new(),
    };
    let settings_path = crate::utils::paths::settings_path(base_path);
    let raw = match std::fs::read_to_string(&settings_path) {
        Ok(raw) => raw,
        // 文件不存在属正常路径（首次运行），静默回退默认
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return default,
        Err(e) => {
            tracing::debug!(
                path = %settings_path.display(),
                error = %e,
                "读取启动配置失败，使用默认启动参数"
            );
            return default;
        }
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        tracing::debug!(
            path = %settings_path.display(),
            "settings.json 解析失败，使用默认启动参数"
        );
        return default;
    };
    let app = value.get("global").and_then(|g| g.get("app"));
    let port = app
        .and_then(|a| a.get("port"))
        .and_then(|p| p.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(default.port);
    #[cfg(feature = "desktop")]
    let show_tray = app
        .and_then(|a| a.get("show_tray"))
        .and_then(|t| t.as_bool())
        .unwrap_or(true);
    // 配置里的 runtime_mode 此前从未被启动逻辑消费（设置页改了也不生效）
    let mode = match app
        .and_then(|a| a.get("runtime_mode"))
        .and_then(|m| m.as_str())
    {
        Some("lightweight") => RuntimeMode::Lightweight,
        _ => RuntimeMode::Full,
    };
    // 兼容迁移前的旧字段名 auto_open_browser（迁移在 ConfigService 初始化时才执行）
    let auto_open_browser = app
        .and_then(|a| {
            a.get("auto_start_browser")
                .or_else(|| a.get("auto_open_browser"))
        })
        .and_then(|t| t.as_bool())
        .unwrap_or(true);
    let (log_level, log_retention_days, log_file_enabled) =
        crate::logging::logging_config_from_value(&value);
    // 管理密码：启动期轻量读，供非回环绑定的凭据门禁（容器初始化前无法走 ConfigService）
    let management_password = app
        .and_then(|a| a.get("management_password"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    StartupSettings {
        port,
        #[cfg(feature = "desktop")]
        show_tray,
        mode,
        auto_open_browser,
        log_level,
        log_retention_days,
        log_file_enabled,
        management_password,
    }
}

/// 检查关键目录的写入权限
///
/// 先经 `utils::paths::ensure_runtime_dirs` 一次性预建全部运行时目录，
/// 再对四类顶层目录做写探测（`config`/`tasks` 必需，`logs`/`environment` 降级）。
pub(super) fn check_directory_permissions(base_path: &Path) -> Result<()> {
    use crate::utils::paths;
    // 预建失败直接阻断：后续 ConfigService / TaskManager / 日志均依赖目录存在。
    paths::ensure_runtime_dirs(base_path)?;
    paths::check_dir_writable(&paths::config_dir(base_path), "config", true)?;
    paths::check_dir_writable(&paths::tasks_dir(base_path), "tasks", true)?;
    paths::check_dir_writable(&paths::logs_dir(base_path), "logs", false)?;
    paths::check_dir_writable(&paths::env_dir(base_path), "environment", false)?;
    Ok(())
}
