//! 系统路由：系统信息、关机、更新、浏览器、图标、卸载、背景图、文档
//!
//! M1 细粒度 state：environment/updater/bridge/metrics 经 AppState 直字段
//! 或 `State<Arc<dyn ...>>` 提取，不再触达 `state.container`。

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::{Query, State};
use serde_json::Value;

use crate::bridge::BridgeApi;
use crate::updater::UpdaterApi;
use crate::web::error::{ApiError, data};
use crate::web::state::AppState;

/// GET /api/system/info — 系统基本信息
pub async fn system_info(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 读无锁运行时快照，避免每次请求走磁盘 mtime 校验（A2）
    let rt = state.config.runtime_snapshot();
    let base_path = state.config.base_path();
    let m = &state.metrics;
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
pub async fn restart_app(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let exe = std::env::current_exe()
        .map_err(|e| ApiError::Internal(format!("获取可执行文件路径失败: {e}")))?;
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    args.retain(|a| a != "--restarting");
    args.push("--restarting".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map_err(|e| ApiError::Internal(format!("启动新进程失败: {e}")))?;
    let _ = state.shutdown_tx.send(());
    spawn_exit_watchdog(30);
    Ok(data(Value::String("正在重启".into())))
}

/// 生成退出 watchdog：优雅关闭超时后强制 `exit(0)`，作为最后防线。
///
/// `shutdown_app` / `restart_app` / `uninstall` 三处共用，统一为 30s，
/// 覆盖优雅关闭总预算（Tray 3s + Scheduler 5s + Engine 5s + Bridge 8s + Axum 5s ≈ 26s），
/// 避免卸载等场景因强杀过早残留浏览器/子进程（A4）。
pub(crate) fn spawn_exit_watchdog(secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        tracing::warn!("优雅关闭超时 {secs}s，强制退出");
        std::process::exit(0);
    });
}

/// POST /api/system/shutdown — 优雅关闭（通知 launcher 执行完整关闭流程）
pub async fn shutdown_app(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let _ = state.shutdown_tx.send(());
    spawn_exit_watchdog(30);
    Ok(data(Value::String("正在关闭".into())))
}

/// POST /api/agree — 用户同意协议（设置向导完成）
pub async fn agree_terms(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let config_dir = state.config.base_path().join("config");
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
pub async fn init_status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let config_dir = state.config.base_path().join("config");
    let agreed = config_dir.join(".agreed").exists();
    let env_status = state.environment.status();
    let password_decryption_failed = state.config.has_decryption_error();
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

const LOG_TAIL_BYTES: u64 = 512 * 1024;

fn read_log_tail(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    if size <= LOG_TAIL_BYTES {
        let mut buf = String::new();
        file.read_to_string(&mut buf).ok()?;
        return Some(buf);
    }
    file.seek(SeekFrom::Start(size - LOG_TAIL_BYTES)).ok()?;
    let mut bytes = Vec::with_capacity(LOG_TAIL_BYTES as usize);
    file.read_to_end(&mut bytes).ok()?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    match content.find('\n') {
        Some(idx) => Some(content[idx + 1..].to_string()),
        None => Some(String::new()),
    }
}

/// GET /api/logs — 读取最新日志文件内容（实时日志通过 WebSocket 推送）
pub async fn fetch_logs(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        .min(2000);
    let logs_dir = state.config.base_path().join("logs");
    let entries: Vec<crate::web::state::LogEntry> =
        tokio::task::spawn_blocking(move || -> Vec<crate::web::state::LogEntry> {
            let latest_file = std::fs::read_dir(&logs_dir).ok().and_then(|entries| {
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
                Some(entry) => read_log_tail(&entry.path())
                    .map(|content| {
                        let session_start =
                            crate::logging::session_started_at().unwrap_or_default();
                        content
                            .lines()
                            .rev()
                            .filter_map(parse_tracing_json_log)
                            .filter(|e| {
                                session_start.is_empty() || e.timestamp.as_str() >= session_start
                            })
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

fn parse_tracing_json_log(line: &str) -> Option<crate::web::state::LogEntry> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let level = v
        .get("level")
        .and_then(|x| x.as_str())
        .unwrap_or("INFO")
        .to_string();
    let timestamp = v
        .get("timestamp")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let source =
        crate::web::state::normalize_source(v.get("target").and_then(|x| x.as_str()).unwrap_or(""));
    Some(crate::web::state::LogEntry::new(
        level, message, timestamp, source,
    ))
}

/// GET /api/check-update — 检查更新
pub async fn check_update(
    State(updater): State<Arc<dyn UpdaterApi>>,
) -> Result<Json<Value>, ApiError> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    match updater.check_update().await {
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

/// POST /api/system/update — 执行更新
pub async fn apply_update(
    State(updater): State<Arc<dyn UpdaterApi>>,
) -> Result<Json<Value>, ApiError> {
    let info = updater
        .check_update()
        .await
        .map_err(|e| ApiError::Internal(format!("检查更新失败: {e}")))?
        .ok_or_else(|| ApiError::BadRequest("当前已是最新版本，无需更新".into()))?;
    updater
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
/// Playwright 管理的三种引擎按各自实际缓存探测。核心引导默认只安装 Chromium，
/// Firefox/WebKit 未安装时必须返回 installed=false，避免 UI 把 Chromium 就绪误报成全引擎可用。
pub async fn list_browsers(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let settings = state.config.load_settings_async().await;
    let chromium_installed = crate::environment::playwright_browser_installed("chromium");
    let firefox_installed = crate::environment::playwright_browser_installed("firefox");
    let webkit_installed = crate::environment::playwright_browser_installed("webkit");
    let custom_path = &settings.global.browser.browser_custom_path;
    let edge_installed = is_edge_installed();
    let chrome_installed = is_chrome_installed();
    let mut browsers = vec![
        serde_json::json!({ "name": "Chromium", "channel": "chromium", "engine": "chromium", "installed": chromium_installed }),
        serde_json::json!({ "name": "Edge", "channel": "msedge", "engine": "chromium", "installed": edge_installed }),
        serde_json::json!({ "name": "Chrome", "channel": "chrome", "engine": "chromium", "installed": chrome_installed }),
        serde_json::json!({ "name": "Firefox", "channel": "firefox", "engine": "firefox", "installed": firefox_installed }),
        serde_json::json!({ "name": "WebKit", "channel": "webkit", "engine": "webkit", "installed": webkit_installed }),
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

#[cfg(target_os = "windows")]
fn is_chrome_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(std::env::var("PROGRAMFILES").unwrap_or_default())
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        std::path::PathBuf::from(std::env::var("PROGRAMFILES(X86)").unwrap_or_default())
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
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

#[cfg(target_os = "windows")]
fn is_edge_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(std::env::var("PROGRAMFILES(X86)").unwrap_or_default())
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
        std::path::PathBuf::from(std::env::var("PROGRAMFILES").unwrap_or_default())
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
    ];
    candidates.iter().any(|p| p.exists())
}

#[cfg(target_os = "macos")]
fn is_edge_installed() -> bool {
    std::path::Path::new("/Applications/Microsoft Edge.app").exists()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn is_edge_installed() -> bool {
    std::process::Command::new("which")
        .arg("microsoft-edge")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// POST /api/install/playwright — 安装 Playwright Chromium
pub async fn install_playwright(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let env = state.environment.clone();
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
pub async fn list_icons(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let icons_dir = state.config.base_path().join("resources").join("icons");
    let mut icons = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&icons_dir).await {
        while let Some(entry) = rd.next_entry().await.ok().flatten() {
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

fn resolve_guide_path(base_path: &std::path::Path) -> std::path::PathBuf {
    let rel = std::path::Path::new("docs")
        .join("guides")
        .join("task-writing-guide.md");
    let primary = base_path.join(&rel);
    if primary.exists() {
        return primary;
    }
    if let Some(repo) = base_path.parent().and_then(|p| p.parent()) {
        let fallback = repo.join(&rel);
        if fallback.exists() {
            return fallback;
        }
    }
    let manifest_fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&rel);
    if manifest_fallback.exists() {
        return manifest_fallback;
    }
    primary
}

pub async fn task_writing_guide(
    State(config): State<Arc<dyn crate::config::ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let path = resolve_guide_path(&config.base_path());
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(data(Value::String(content))),
        Err(e) => {
            tracing::warn!("任务编写指南加载失败 ({path:?}): {e}");
            Err(ApiError::NotFound(
                "任务编写指南文件缺失，可能需要重新安装或更新软件".to_string(),
            ))
        }
    }
}

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

pub async fn stop_worker(
    State(bridge): State<Arc<dyn BridgeApi>>,
) -> Result<Json<Value>, ApiError> {
    bridge.shutdown().await;
    Ok(data(serde_json::json!({ "stopped": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tracing_json_log_extracts_fields() {
        let line = r#"{"timestamp":"2026-08-14T01:02:03Z","level":"INFO","fields":{"message":"登录成功"},"target":"campus_auth::login"}"#;
        let entry = parse_tracing_json_log(line).expect("应解析成功");
        assert_eq!(entry.level, "INFO");
        assert_eq!(entry.message, "登录成功");
        assert_eq!(entry.source, "login");
        assert!(!entry.timestamp.is_empty());
    }

    #[test]
    fn parse_tracing_json_log_preserves_all_levels() {
        for level in ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] {
            let line = format!(r#"{{"level":"{level}","fields":{{"message":"x"}}}}"#);
            let entry = parse_tracing_json_log(&line).expect("级别应保留");
            assert_eq!(entry.level, level);
        }
    }

    #[test]
    fn parse_tracing_json_log_handles_invalid_and_missing_fields() {
        assert!(parse_tracing_json_log("not json").is_none());
        let entry = parse_tracing_json_log(r#"{"level":"WARN"}"#).expect("结构不完整也应解析");
        assert_eq!(entry.message, "");
    }

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get as route_get;
    use tower::ServiceExt;

    use crate::updater::{UpdateInfo, UpdaterError};

    enum MockOutcome {
        None,
        Info(UpdateInfo),
        Err(String),
    }

    struct MockUpdaterApi(MockOutcome);

    #[async_trait::async_trait]
    impl UpdaterApi for MockUpdaterApi {
        async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
            match &self.0 {
                MockOutcome::None => Ok(None),
                MockOutcome::Info(i) => Ok(Some(i.clone())),
                MockOutcome::Err(msg) => Err(UpdaterError::HttpsRequired(msg.clone())),
            }
        }

        async fn apply_update(&self, _info: &UpdateInfo) -> Result<(), UpdaterError> {
            Ok(())
        }
    }

    fn sample_info() -> UpdateInfo {
        UpdateInfo {
            current_version: "1.0.0".into(),
            latest_version: "1.1.0".into(),
            update_available: true,
            url: "https://example.com/app.zip".into(),
            sha256: "deadbeef".into(),
            size: Some(1024),
            notes: Some("修复若干问题".into()),
            release_date: None,
        }
    }

    async fn run_check(outcome: MockOutcome) -> Value {
        let api: Arc<dyn UpdaterApi> = Arc::new(MockUpdaterApi(outcome));
        let app = axum::Router::new()
            .route("/api/check-update", route_get(check_update))
            .with_state(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/check-update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_check_update_with_info() {
        let v = run_check(MockOutcome::Info(sample_info())).await;
        let d = &v["data"];
        assert_eq!(d["has_update"], true);
        assert_eq!(d["latest"], "1.1.0");
        assert_eq!(d["current"], "1.0.0");
    }

    #[tokio::test]
    async fn test_check_update_no_update() {
        let v = run_check(MockOutcome::None).await;
        let d = &v["data"];
        assert_eq!(d["has_update"], false);
        assert_eq!(d["latest"], env!("CARGO_PKG_VERSION"));
        assert!(d.get("error").is_none());
    }

    #[tokio::test]
    async fn test_check_update_error_field() {
        let v = run_check(MockOutcome::Err("网络超时".into())).await;
        let d = &v["data"];
        assert_eq!(d["has_update"], false);
        assert!(d["error"].as_str().unwrap().contains("网络超时"));
    }
}
