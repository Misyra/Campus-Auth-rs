//! 系统路由：系统信息、关机、更新、浏览器、图标、卸载、背景图、文档

use std::cmp::Reverse;
use std::sync::atomic::Ordering;

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::Json;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/system/info — 系统基本信息
pub async fn system_info(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    // 读无锁运行时快照，避免每次请求走磁盘 mtime 校验（A2）
    let rt = state.container.config.runtime().load();
    let base_path = state.container.config.base_path();
    let m = &state.container.metrics;
    let info = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "base_path": base_path.to_string_lossy(),
        "port": rt.app.port,
        "active_profile_id": rt.profile.id,
        "platform": std::env::consts::OS,
        "metrics": {
            "login_total": m.login_total.load(Ordering::Relaxed),
            "login_success_total": m.login_success_total.load(Ordering::Relaxed),
            "login_failure_total": m.login_failure_total.load(Ordering::Relaxed),
            "login_cancel_total": m.login_cancel_total.load(Ordering::Relaxed),
            "probe_total": m.probe_total.load(Ordering::Relaxed),
            "probe_duration_ms_avg": m.probe_duration_ms_avg.load(Ordering::Relaxed),
            "worker_spawn_total": m.worker_spawn_total.load(Ordering::Relaxed),
            "worker_crash_total": m.worker_crash_total.load(Ordering::Relaxed),
            "uptime_seconds": m.uptime_seconds.load(Ordering::Relaxed),
        },
    });
    Ok(data(info))
}

/// POST /api/system/restart — 重启应用（spawn 带 `--restarting` 的新进程后优雅退出）
///
/// 新进程带 `--restarting` 标记，launcher 会先等待旧进程释放实例锁再启动，
/// 避免双方争锁导致"重启变退出"（旧实现直接 spawn 原参数 + 200ms 后
/// `exit(0)` 硬退出，新进程抢锁失败即死，最终两个进程都消失）。
/// 旧进程通过 shutdown_tx 走完整优雅关闭流程（而非 exit(0) 跳过清理）。
pub async fn restart_app(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let exe = std::env::current_exe()
        .map_err(|e| ApiError::Internal(format!("获取可执行文件路径失败: {e}")))?;
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
    cmd.spawn()
        .map_err(|e| ApiError::Internal(format!("启动新进程失败: {e}")))?;
    // 通知 launcher 优雅关闭当前进程（新进程会等待实例锁释放）
    let _ = state.shutdown_tx.send(());
    // watchdog：优雅关闭挂死时强制退出，释放实例锁供新进程启动（A4 统一）
    spawn_exit_watchdog(30);
    Ok(data(Value::String("正在重启".into())))
}

/// 生成退出 watchdog：优雅关闭超时后强制 `exit(0)`，作为最后防线。
///
/// `shutdown_app` / `restart_app` / `uninstall` 三处共用，统一为 30s，
/// 覆盖优雅关闭总预算（Tray 3s + Scheduler 5s + Engine 5s + Bridge 8s + Axum 5s ≈ 26s），
/// 避免卸载等场景因强杀过早残留浏览器/子进程（A4）。
fn spawn_exit_watchdog(secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        tracing::warn!("优雅关闭超时 {secs}s，强制退出");
        std::process::exit(0);
    });
}

/// POST /api/system/shutdown — 优雅关闭（通知 launcher 执行完整关闭流程）
///
/// 不再使用 exit(0)；launcher 收到 shutdown_tx 信号后依次停止 Engine/Scheduler/
/// Bridge/Tray/Axum，所有服务在各自 event loop 内清理资源后再退出进程。
/// 若 30s 后仍未退出（优雅关闭挂死），最后防线才是 exit(0)。
pub async fn shutdown_app(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    // 通知 launcher 开始优雅关闭
    let _ = state.shutdown_tx.send(());
    // watchdog：30s 后若仍存活则强制退出（所有服务本应在 30s 内完成清理）
    spawn_exit_watchdog(30);
    Ok(data(Value::String("正在关闭".into())))
}

/// POST /api/agree — 用户同意协议（设置向导完成）
pub async fn agree_terms(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    // 标记用户已同意协议（写入配置或标记文件）
    let config_dir = state.container.config.base_path().join("config");
    let agreed_file = config_dir.join(".agreed");
    tokio::fs::write(&agreed_file, chrono::Utc::now().to_rfc3339()).await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/health — 健康检查
pub async fn health_check() -> Json<Value> {
    data(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// GET /api/init-status — 初始化状态
pub async fn init_status(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let config_dir = state.container.config.base_path().join("config");
    let agreed = config_dir.join(".agreed").exists();
    let env_status = state.container.environment.status();
    let password_decryption_failed = state.container.config.has_decryption_error();
    Ok(data(serde_json::json!({
        "agreed": agreed,
        "ready": env_status.capability_ready,
        "password_decryption_failed": password_decryption_failed,
        "environment": {
            "uv_ready": env_status.uv_ready,
            "python_ready": env_status.python_ready,
            "playwright_ready": env_status.playwright_ready,
            "git_ready": env_status.git_ready,
            "capability_ready": env_status.capability_ready,
            "stage": format!("{:?}", env_status.stage),
            "progress": env_status.progress,
            "last_error": env_status.last_error,
        },
    })))
}

/// GET /api/logs — 读取最新日志文件内容（实时日志通过 WebSocket 推送）
///
/// 日志文件为 tracing JSON 格式（每行一个 JSON 对象），解析后返回前端期望的
/// `LogEntry[]`（`{timestamp, level, message, source}`）。
pub async fn fetch_logs(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        // 钳制上限，避免传 99999999 全量解析 MB 级日志
        .min(2000);
    let logs_dir = state.container.config.base_path().join("logs");
    // 日志文件可能达 MB 级，read_dir + read_to_string + JSON 解析为阻塞 I/O 与 CPU 密集操作，
    // 整体放入 spawn_blocking 避免阻塞 tokio worker 线程
    let entries: Vec<crate::web::state::LogEntry> =
        tokio::task::spawn_blocking(move || -> Vec<crate::web::state::LogEntry> {
            // 查找最新的日志文件（按文件名排序，app.log.YYYY-MM-DD 格式）
            let latest_file = std::fs::read_dir(&logs_dir)
                .ok()
                .and_then(|entries| {
                    let mut files: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .map(|n| n.starts_with("app.log"))
                                .unwrap_or(false)
                        })
                        .collect();
                    files.sort_by_key(|a| Reverse(a.file_name()));
                    files.into_iter().next()
                });
            match latest_file {
                Some(entry) => std::fs::read_to_string(entry.path())
                    .ok()
                    .map(|content| {
                        // 从最新行开始反向遍历 → 过滤 TRACE/DEBUG → 取 limit 条 → 再反转为从旧到新
                        // 先 filter 后 take 确保返回 limit 条有效日志（不受旧噪音日志影响）
                        content
                            .lines()
                            .rev()
                            .filter_map(parse_tracing_json_log)
                            .take(limit)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect()
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        })
        .await
        .map_err(|e| ApiError::Internal(format!("日志读取任务失败: {e}")))?;
    Ok(data(serde_json::to_value(entries)?))
}

/// 解析 tracing JSON 日志行为 LogEntry
///
/// tracing json 格式：`{"timestamp":"...","level":"INFO","fields":{"message":"..."},"target":"..."}`
/// 过滤 TRACE/DEBUG 级别（这些是噪音日志，不向前端返回）
fn parse_tracing_json_log(line: &str) -> Option<crate::web::state::LogEntry> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("INFO").to_string();
    // 过滤噪音级别：TRACE/DEBUG 不返回给前端
    if level == "TRACE" || level == "DEBUG" {
        return None;
    }
    let timestamp = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let source = crate::web::state::normalize_source(
        v.get("target").and_then(|x| x.as_str()).unwrap_or(""),
    );
    Some(crate::web::state::LogEntry { level, message, timestamp, source })
}

/// GET /api/check-update — 检查更新
///
/// 返回字段对齐前端契约：`has_update`(bool) / `latest`(string) / `current`(string) /
/// `error`(string,可选)。
pub async fn check_update(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    match state.container.updater.check_update().await {
        Ok(Some(info)) => Ok(data(info)),
        Ok(None) => Ok(data(serde_json::json!({
            "has_update": false,
            "current": current,
            "latest": current,
        }))),
        Err(e) => Ok(data(serde_json::json!({
            "has_update": false,
            "current": current,
            "latest": current,
            "error": e.to_string(),
        }))),
    }
}

/// POST /api/system/update — 执行更新（下载 zip 到 staging 并触发助手替换）
///
/// 先调用 `check_update` 获取最新版本信息，再 `apply_update` 执行下载与暂存。
pub async fn apply_update(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let info = state
        .container
        .updater
        .check_update()
        .await
        .map_err(|e| ApiError::Internal(format!("检查更新失败: {e}")))?
        .ok_or_else(|| ApiError::BadRequest("当前已是最新版本，无需更新".into()))?;
    state
        .container
        .updater
        .apply_update(&info)
        .await
        .map_err(|e| ApiError::Internal(format!("应用更新失败: {e}")))?;
    Ok(data(serde_json::json!({
        "message": "更新已暂存，重启后生效",
        "version": info.latest_version,
    })))
}

/// GET /api/browsers — 可用浏览器列表
///
/// 返回基于配置的可用浏览器列表（自定义路径优先，其次 Playwright 内置浏览器）。
pub async fn list_browsers(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let settings = state.container.config.load_settings();
    let env_status = state.container.environment.status();
    let playwright_installed = env_status.playwright_ready;
    let custom_path = &settings.global.browser.browser_custom_path;
    let edge_installed = is_edge_installed();
    let chrome_installed = is_chrome_installed();
    let mut browsers = vec![
        serde_json::json!({ "name": "Chromium", "channel": "chromium", "engine": "chromium", "installed": playwright_installed }),
        serde_json::json!({ "name": "Edge", "channel": "msedge", "engine": "chromium", "installed": edge_installed }),
        serde_json::json!({ "name": "Chrome", "channel": "chrome", "engine": "chromium", "installed": chrome_installed }),
        serde_json::json!({ "name": "Firefox", "channel": "firefox", "engine": "firefox", "installed": playwright_installed }),
        serde_json::json!({ "name": "WebKit", "channel": "webkit", "engine": "webkit", "installed": playwright_installed }),
    ];
    if !custom_path.is_empty() {
        browsers.insert(
            0,
            serde_json::json!({
                "name": "自定义浏览器",
                "channel": "custom",
                "engine": settings.global.browser.custom_browser_engine,
                "path": custom_path,
                "installed": true,
                "custom": true,
            }),
        );
    }
    Ok(data(serde_json::json!({
        "browsers": browsers,
        "current": settings.global.browser.browser_channel,
    })))
}

/// 检测系统是否安装了 Google Chrome
#[cfg(target_os = "windows")]
fn is_chrome_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(
            std::env::var("PROGRAMFILES").unwrap_or_default(),
        )
        .join("Google")
        .join("Chrome")
        .join("Application")
        .join("chrome.exe"),
        std::path::PathBuf::from(
            std::env::var("PROGRAMFILES(X86)").unwrap_or_default(),
        )
        .join("Google")
        .join("Chrome")
        .join("Application")
        .join("chrome.exe"),
        std::path::PathBuf::from(
            std::env::var("LOCALAPPDATA").unwrap_or_default(),
        )
        .join("Google")
        .join("Chrome")
        .join("Application")
        .join("chrome.exe"),
    ];
    candidates.iter().any(|p| p.exists())
}

#[cfg(target_os = "macos")]
fn is_chrome_installed() -> bool {
    std::path::Path::new("/Applications/Google Chrome.app").exists()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn is_chrome_installed() -> bool {
    std::process::Command::new("which")
        .arg("google-chrome")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 检测系统是否安装了 Microsoft Edge
#[cfg(target_os = "windows")]
fn is_edge_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(
            std::env::var("PROGRAMFILES(X86)").unwrap_or_default(),
        )
        .join("Microsoft")
        .join("Edge")
        .join("Application")
        .join("msedge.exe"),
        std::path::PathBuf::from(
            std::env::var("PROGRAMFILES").unwrap_or_default(),
        )
        .join("Microsoft")
        .join("Edge")
        .join("Application")
        .join("msedge.exe"),
    ];
    candidates.iter().any(|p| p.exists())
}

/// 检测系统是否安装了 Microsoft Edge（macOS）
#[cfg(target_os = "macos")]
fn is_edge_installed() -> bool {
    std::path::Path::new("/Applications/Microsoft Edge.app").exists()
}

/// 检测系统是否安装了 Microsoft Edge（Linux）
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn is_edge_installed() -> bool {
    std::process::Command::new("which")
        .arg("microsoft-edge")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// POST /api/install/playwright — 安装 Playwright Chromium
///
/// 触发环境管理器安装 Playwright 浏览器（异步执行，进度通过 StatusManager 推送）。
pub async fn install_playwright(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let env = state.container.environment.clone();
    // 后台执行安装，避免阻塞响应；进度通过 StatusManager 推送
    tokio::spawn(async move {
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("Playwright 安装失败: {e}");
        }
    });
    Ok(data(serde_json::json!({
        "message": "Playwright 安装已启动，进度请通过状态推送查看",
    })))
}

/// GET /api/icons — 可用图标列表
///
/// 扫描资源图标目录返回可用图标。目录不存在时返回空列表。
pub async fn list_icons(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let icons_dir = state.container.config.base_path().join("resources").join("icons");
    let mut icons = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&icons_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".png") || name.ends_with(".ico") || name.ends_with(".svg") {
                    let stem = name.split('.').next().unwrap_or(name).to_string();
                    icons.push(serde_json::json!({ "name": stem, "file": name }));
                }
            }
        }
    }
    Ok(data(icons))
}

/// GET /api/uninstall/detect — 卸载检测
///
/// 返回卸载时将清理的目录与文件清单（不执行实际删除），每项为一个 UninstallItem。
pub async fn detect_uninstall(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let base = state.container.config.base_path();
    let mut items = Vec::new();
    for (key, label, sub) in [
        ("config", "配置目录", "config"),
        ("logs", "日志目录", "logs"),
        ("environment", "环境目录", "environment"),
        ("tasks", "任务目录", "tasks"),
        ("update", "更新目录", "update"),
    ] {
        let path = base.join(sub);
        items.push(serde_json::json!({
            "key": key,
            "label": label,
            "exists": path.exists(),
            "description": path.to_string_lossy(),
        }));
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    items.push(serde_json::json!({
        "key": "executable",
        "label": "可执行文件",
        "exists": !exe.is_empty() && std::path::Path::new(&exe).exists(),
        "description": exe,
    }));
    Ok(data(serde_json::json!(items)))
}

/// POST /api/uninstall — 执行卸载
///
/// 生成并写入卸载助手脚本（batch），然后退出程序。
/// 用户手动运行该脚本完成残留文件清理。
pub async fn uninstall(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let base = state.container.config.base_path();

    // 如果 helper 存在则直接写入卸载脚本并退出
    let uninstall_script = base.join("uninstall.bat");
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // 卸载脚本：首次运行时把自身副本复制到 %TEMP% 再从副本执行，避免
    // `rd /s /q "{base}"` 删除正在运行的 bat 自身所在目录时因文件被锁而残留
    // （7.3：原实现直接运行会残留 base/uninstall.bat）。
    let script = format!(
        "@echo off\r\n\
         chcp 65001 > nul\r\n\
         if \"%1\"==\"run_from_temp\" goto :run\r\n\
         copy /y \"%~f0\" \"%TEMP%\\campus-auth-uninstall.bat\" > nul\r\n\
         start \"\" \"%TEMP%\\campus-auth-uninstall.bat\" run_from_temp\r\n\
         exit /b 0\r\n\
         :run\r\n\
         echo Campus-Auth 卸载助手\r\n\
         echo =====================================\r\n\
         echo.\r\n\
         echo 即将删除 Campus-Auth 所有文件...\r\n\
         timeout /t 3 /nobreak > nul\r\n\
         echo.\r\n\
         taskkill /f /im campus-auth.exe 2>nul\r\n\
         taskkill /f /im campus-auth-helper.exe 2>nul\r\n\
         timeout /t 1 /nobreak > nul\r\n\
         rd /s /q \"{base}\" 2>nul\r\n\
         del /f /q \"{exe}\" 2>nul\r\n\
         del /f /q \"%TEMP%\\campus-auth-uninstall.bat\" 2>nul\r\n\
         echo.\r\n\
         echo 卸载完成。\r\n\
         pause\r\n",
        base = base.display(),
    );

    tokio::fs::write(&uninstall_script, script).await?;

    // 通知 launcher 优雅关闭
    let _ = state.shutdown_tx.send(());
    // watchdog：统一 30s，覆盖优雅关闭总预算，避免卸载时强杀过早晨残留浏览器/子进程（A4）
    spawn_exit_watchdog(30);

    Ok(data(serde_json::json!({
        "message": "卸载脚本已生成，程序即将退出。请手动运行 uninstall.bat 完成清理。",
        "script_path": uninstall_script.to_string_lossy(),
    })))
}

// ---- 背景图管理 ----

#[derive(Deserialize)]
pub struct BackgroundFetchBody {
    /// 图片 URL
    pub url: String,
}

/// 背景图存储目录
fn background_dir(state: &AppState) -> std::path::PathBuf {
    state.container.config.base_path().join("config").join("background")
}

/// 从文件名中提取安全文件名（防路径穿越），失败则用 UUID 生成
fn safe_filename(original: Option<String>) -> String {
    let candidate = original.unwrap_or_else(|| format!("bg-{}", uuid::Uuid::new_v4()));
    std::path::Path::new(&candidate)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("bg-{}", uuid::Uuid::new_v4()))
}

/// 根据 Content-Type 返回图片扩展名（不含 `.`）
fn ext_from_content_type(ct: &str) -> Option<&'static str> {
    match ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "image/bmp" => Some("bmp"),
        "image/x-icon" => Some("ico"),
        _ => None,
    }
}

/// 根据 magic bytes 识别图片格式（Content-Type 缺失或不可信时兜底）
fn ext_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        Some("png")
    } else if bytes.len() >= 3 && &bytes[0..3] == b"\xFF\xD8\xFF" {
        Some("jpg")
    } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
        Some("bmp")
    } else if bytes.len() >= 4 && (&bytes[0..4] == b"\x00\x00\x01\x00") {
        Some("ico")
    } else {
        None
    }
}

/// 确保文件名带正确的图片扩展名。
/// 优先沿用原扩展名；无扩展名或不在白名单时，按 Content-Type / magic bytes 补全。
fn ensure_image_extension(filename: &str, content_type: &str, bytes: &[u8]) -> String {
    const ALLOWED: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"];
    let path = std::path::Path::new(filename);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ALLOWED.contains(&ext.to_ascii_lowercase().as_str()) {
            return filename.to_string();
        }
    }
    // 无扩展名或不合规：补全
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("bg");
    let ext = ext_from_content_type(content_type)
        .or_else(|| ext_from_magic(bytes))
        .unwrap_or("bin");
    format!("{stem}.{ext}")
}

/// GET /api/background/{filename} — 获取背景图
///
/// 返回原始图片字节 + 正确 Content-Type，供前端 CSS url() 直接引用。
pub async fn get_background(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // 防路径穿越：提取安全文件名，并拒绝包含 `..` 的输入
    if filename.contains("..") {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let safe_name = safe_filename(Some(filename));
    let dir = background_dir(&state);
    let path = dir.join(&safe_name);
    // 确保最终路径仍在背景图目录之内
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("背景图 {} 不存在", safe_name)));
    }
    let bytes = tokio::fs::read(&path).await?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    Ok(([(header::CONTENT_TYPE, mime)], bytes))
}

/// POST /api/background/upload — 上传背景图（multipart/form-data，字段名 file）
pub async fn upload_background(
    State(state): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<Value>, ApiError> {
    let dir = background_dir(&state);
    tokio::fs::create_dir_all(&dir).await?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart 解析失败: {e}")))?
    {
        if field.name() == Some("file") {
            let original = field.file_name().map(|s| s.to_string());
            let content_type = field
                .content_type()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("读取文件字节失败: {e}")))?;
            let safe = safe_filename(original);
            let filename = ensure_image_extension(&safe, &content_type, &bytes);
            let path = dir.join(&filename);
            tokio::fs::write(&path, &bytes).await?;
            return Ok(data(serde_json::json!({
                "filename": filename,
                "url": format!("/api/background/{}", filename),
            })));
        }
    }
    Err(ApiError::BadRequest("缺少 file 字段".into()))
}

/// POST /api/background/fetch-url — 从 URL 获取背景图
pub async fn fetch_url_background(
    State(state): State<AppState>,
    Json(body): Json<BackgroundFetchBody>,
) -> Result<Json<Value>, ApiError> {
    // SSRF 防护：校验 URL 合法性与目标 IP 安全性
    validate_url_not_private(&body.url).await?;

    let dir = background_dir(&state);
    tokio::fs::create_dir_all(&dir).await?;
    let response = state
        .container
        .environment
        .http_client()
        .get(&body.url)
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("请求图片失败: {e}")))?;
    // 验证 Content-Type 为图片类型，防止下载非图片内容
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.starts_with("image/") {
        return Err(ApiError::BadRequest(format!(
            "URL 返回非图片类型: {}",
            content_type
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| ApiError::Internal(format!("读取图片字节失败: {e}")))?;
    // 从 URL 路径提取文件名，失败则用 UUID 生成
    let extracted = body
        .url
        .split('?')
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let safe = safe_filename(extracted);
    let filename = ensure_image_extension(&safe, &content_type, &bytes);
    let path = dir.join(&filename);
    tokio::fs::write(&path, &bytes).await?;
    Ok(data(serde_json::json!({
        "filename": filename,
        "url": format!("/api/background/{}", filename),
    })))
}

/// DELETE /api/background/{filename} — 删除背景图
pub async fn delete_background(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // 防路径穿越：提取安全文件名，并拒绝包含 `..` 的输入
    if filename.contains("..") {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let safe_name = safe_filename(Some(filename));
    let dir = background_dir(&state);
    let path = dir.join(&safe_name);
    // 确保最终路径仍在背景图目录之内
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("背景图 {} 不存在", safe_name)));
    }
    tokio::fs::remove_file(&path).await?;
    Ok(data(Value::String("ok".into())))
}

// ---- 文档 ----

/// GET /api/docs/task-writing-guide — 任务编写指南
///
/// 从 `docs/guides/task-writing-guide.md` 读取并返回 Markdown 文本。
/// 文件缺失时返回 404。
pub async fn task_writing_guide(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let path = state
        .container
        .config
        .base_path()
        .join("docs")
        .join("guides")
        .join("task-writing-guide.md");
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(data(Value::String(content))),
        Err(e) => {
            tracing::warn!("任务编写指南加载失败 ({path:?}): {e}");
            Err(ApiError::NotFound(
                "任务编写指南文件缺失，可能需要重新安装或更新软件".to_string(),
            ))
        }
    }
}

/// GET /api/docs/task-manual — 任务使用手册
pub async fn task_manual() -> Json<Value> {
    data(Value::String(
        "# 任务使用手册\n\n\
         - 通过 `POST /api/tasks` 创建任务\n\
         - 通过 `POST /api/tasks/active/{task_id}` 设置活跃任务\n\
         - 通过 `POST /api/login` 触发登录，将自动执行活跃任务\n\n\
         调试请使用 `POST /api/debug/start` 启动调试会话。"
            .into(),
    ))
}

// ---- 工具函数 ----

/// 判断 IP 是否属于私有/保留地址段（含回环、链路本地、RFC 1918、IPv6 ULA）
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || match ip {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_unique_local(),
        }
}

/// SSRF 防护：校验 URL 是否为 HTTPS，并确认目标 IP 不在私有/保留地址段
///
/// 对域名先做 DNS 解析，再逐条检查解析结果；任一 IP 命中私有段即拒绝。
async fn validate_url_not_private(url: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ApiError::BadRequest(format!("无效 URL: {e}")))?;
    // 仅允许 HTTPS
    if parsed.scheme() != "https" {
        return Err(ApiError::BadRequest("仅允许 HTTPS URL".into()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest("URL 缺少 host".into()))?;
    // 若 host 本身是 IP 地址，直接校验
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(ip) {
            return Err(ApiError::BadRequest("目标 IP 位于私有/保留地址段".into()));
        }
        return Ok(());
    }
    // 域名：DNS 解析后逐条校验；解析失败时拒绝（不容忍间歇性 DNS 失败绕过）
    let addrs: Vec<std::net::SocketAddr> =
        tokio::net::lookup_host(format!("{}:443", host))
            .await
            .map_err(|_| ApiError::BadRequest("DNS 解析失败，拒绝请求".into()))?
            .collect();
    if addrs.is_empty() {
        return Err(ApiError::Internal("DNS 解析无结果".into()));
    }
    for addr in &addrs {
        if is_private_ip(addr.ip()) {
            return Err(ApiError::BadRequest(
                "域名解析结果包含私有/保留 IP 地址".into(),
            ));
        }
    }
    Ok(())
}

/// POST /api/worker/stop — 手动关闭浏览器（优雅停止 Python Worker）
///
/// 停止当前运行的 Worker 进程及其浏览器实例。Supervisor 保持运行，
/// 下次任务到来时会自动重新启动 Worker。
pub async fn stop_worker(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    state.container.bridge.shutdown().await;
    Ok(data(serde_json::json!({ "stopped": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ tracing JSON 日志解析 ============

    #[test]
    fn parse_tracing_json_log_extracts_fields() {
        let line = r#"{"timestamp":"2026-08-14T01:02:03Z","level":"INFO","fields":{"message":"登录成功"},"target":"campus_auth::login"}"#;
        let entry = parse_tracing_json_log(line).expect("应解析成功");
        assert_eq!(entry.level, "INFO");
        assert_eq!(entry.message, "登录成功");
        // source 经 normalize_source 归一化（去掉 crate 前缀，取首段）
        assert_eq!(entry.source, "login");
        assert!(!entry.timestamp.is_empty());
    }

    #[test]
    fn parse_tracing_json_log_filters_noise_levels() {
        for level in ["TRACE", "DEBUG"] {
            let line = format!(r#"{{"level":"{level}","fields":{{"message":"x"}}}}"#);
            assert!(parse_tracing_json_log(&line).is_none(), "{level} 应被过滤");
        }
        // INFO 级别的普通日志应保留
        let info = r#"{"level":"INFO","fields":{"message":"x"}}"#;
        assert!(parse_tracing_json_log(info).is_some());
    }

    #[test]
    fn parse_tracing_json_log_handles_invalid_and_missing_fields() {
        assert!(parse_tracing_json_log("not json").is_none());
        // 缺少 fields.message 时回退为空字符串而非报错
        let entry = parse_tracing_json_log(r#"{"level":"WARN"}"#).expect("结构不完整也应解析");
        assert_eq!(entry.message, "");
    }

    // ============ 背景图扩展名识别 ============

    #[test]
    fn ext_from_content_type_maps_known_types() {
        assert_eq!(ext_from_content_type("image/png"), Some("png"));
        assert_eq!(ext_from_content_type("image/jpeg"), Some("jpg"));
        assert_eq!(ext_from_content_type("image/webp"), Some("webp"));
        assert_eq!(ext_from_content_type("image/svg+xml"), Some("svg"));
        // 带参数 / 大小写混合 / 未知类型
        assert_eq!(ext_from_content_type("image/PNG; charset=utf-8"), Some("png"));
        assert_eq!(ext_from_content_type("application/octet-stream"), None);
        assert_eq!(ext_from_content_type(""), None);
    }

    #[test]
    fn ext_from_magic_recognizes_common_formats() {
        assert_eq!(ext_from_magic(b"\x89PNG\r\n\x1a\nxxxx"), Some("png"));
        assert_eq!(ext_from_magic(b"\xFF\xD8\xFF\xE0xxxx"), Some("jpg"));
        assert_eq!(ext_from_magic(b"GIF89a"), Some("gif"));
        assert_eq!(ext_from_magic(b"BMxxxx"), Some("bmp"));
        assert_eq!(ext_from_magic(b"\x00\x00\x01\x00xxxx"), Some("ico"));
        // WEBP: RIFF....WEBP
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(b"0000WEBP");
        assert_eq!(ext_from_magic(&webp), Some("webp"));
        // 未知 magic
        assert_eq!(ext_from_magic(b"hello"), None);
    }

    #[test]
    fn ensure_image_extension_keeps_allowed_and_fills_missing() {
        // 已合规扩展名：原样保留
        assert_eq!(ensure_image_extension("bg.png", "image/png", &[]), "bg.png");
        assert_eq!(ensure_image_extension("bg.JPG", "image/jpeg", &[]), "bg.JPG");
        // 无扩展名：按 Content-Type 补全
        assert_eq!(ensure_image_extension("bg", "image/webp", &[]), "bg.webp");
        // Content-Type 不可信时回退 magic
        assert_eq!(ensure_image_extension("photo", "", b"\x89PNG\r\n\x1a\n1"), "photo.png");
        // 不合规扩展名：改成 Content-Type 对应的
        assert_eq!(ensure_image_extension("bg.exe", "image/jpeg", &[]), "bg.jpg");
        // 完全无法识别：回退 bin
        assert_eq!(ensure_image_extension("weird", "", b"zzz"), "weird.bin");
    }

    // ============ 背景图文件名安全 ============

    #[test]
    fn safe_filename_strips_path_components() {
        // 路径穿越尝试：只取 file_name
        assert_eq!(safe_filename(Some("../../etc/passwd".into())), "passwd");
        // 正常文件名
        assert_eq!(safe_filename(Some("sunset.png".into())), "sunset.png");
    }

    #[test]
    fn safe_filename_falls_back_to_uuid_on_empty() {
        let fallback = safe_filename(None);
        assert!(!fallback.is_empty());
        assert!(fallback.starts_with("bg-"));
    }

    // ============ 背景图 URL SSRF 校验（私有 IP 判定） ============

    #[test]
    fn is_private_ip_detects_private_and_reserved() {
        use std::net::IpAddr;
        assert!(is_private_ip(IpAddr::V4("127.0.0.1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V4("10.0.0.1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V4("192.168.1.1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V4("169.254.0.1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6("::1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6("fc00::1".parse().unwrap())));
        // 公网地址放行
        assert!(!is_private_ip(IpAddr::V4("8.8.8.8".parse().unwrap())));
        assert!(!is_private_ip(IpAddr::V6("2606:4700:4700::1111".parse().unwrap())));
    }
}
