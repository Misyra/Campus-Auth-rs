//! 系统路由：系统信息、关机、更新、浏览器、图标、背景图、文档
//!
//! M1 细粒度 state：environment/updater/bridge/metrics 经 AppState 直字段
//! 或 `State<Arc<dyn ...>>` 提取，不再触达 `state.container`。

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::bridge::BridgeApi;
use crate::browser::{is_chrome_installed, is_edge_installed};
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
        // 实际监听端口优先读运行时端口文件：`--port` CLI 覆盖与冲突重试
        // 只体现在该文件；无文件（如轻量模式未启动 Web）时回退配置值。
        "port": crate::utils::paths::read_runtime_port(&base_path).unwrap_or(rt.app.port),
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
/// 后继进程生成逻辑与定时自重启共用 [`crate::launcher::spawn_restart_successor`]。
pub async fn restart_app(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 生命周期事件：重启是关键用户操作，info 留痕
    tracing::info!("收到重启请求，生成后继进程并开始优雅关闭");
    crate::launcher::spawn_restart_successor().map_err(ApiError::Internal)?;
    // 通知 launcher 优雅关闭当前进程（新进程会等待实例锁释放）
    let _ = state.shutdown_tx.send(());
    // watchdog：优雅关闭挂死时强制退出，释放实例锁供新进程启动（A4 统一）
    crate::launcher::spawn_exit_watchdog(30);
    Ok(data(Value::String("正在重启".into())))
}

/// POST /api/system/shutdown — 优雅关闭（通知 launcher 执行完整关闭流程）
///
/// 不再使用 exit(0)；launcher 收到 shutdown_tx 信号后依次停止 Engine/Scheduler/
/// Bridge/Tray/Axum，所有服务在各自 event loop 内清理资源后再退出进程。
/// 若 30s 后仍未退出（优雅关闭挂死），最后防线才是 exit(0)。
pub async fn shutdown_app(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 生命周期事件：关闭是关键用户操作，info 留痕
    tracing::info!("收到关闭请求，开始优雅关闭");
    // 通知 launcher 开始优雅关闭
    let _ = state.shutdown_tx.send(());
    // watchdog：30s 后若仍存活则强制退出（所有服务本应在 30s 内完成清理）
    crate::launcher::spawn_exit_watchdog(30);
    Ok(data(Value::String("正在关闭".into())))
}

/// POST /api/agree — 用户同意协议（设置向导完成）
pub async fn agree_terms(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 标记用户已同意协议（写入配置或标记文件）
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
            "capability_ready": env_status.capability_ready,
            "stage": format!("{:?}", env_status.stage),
            "progress": env_status.progress,
            "last_error": env_status.last_error,
        },
    })))
}

/// 日志尾部读取的最大字节数（512KB）
///
/// 日志文件可能达数百 MB，而 limit（≤2000 条）对应的最新日志绝大多数场景
/// 都在尾部 512KB 内，全量读入内存再解析纯属浪费。
const LOG_TAIL_BYTES: u64 = 512 * 1024;

/// 读取日志文件尾部（最多 [`LOG_TAIL_BYTES`] 字节），文件较小时全读
///
/// 从中间位置起读时，首行可能是不完整的行（且可能以残缺的多字节 UTF-8
/// 字符开头），统一丢弃第一行；其余行保证完整。
/// `pub(crate)` 供 `routes::debug::feedback_bundle` 复用（同文件内尾段语义）。
pub(crate) fn read_log_tail(path: &std::path::Path) -> Option<String> {
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
    // 丢弃第一行（seek 位置切在行中间时该行不完整）
    match content.find('\n') {
        Some(idx) => Some(content[idx + 1..].to_string()),
        None => Some(String::new()),
    }
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
    let logs_dir = state.config.base_path().join("logs");
    // 日志文件可能达 MB 级，read_dir + 尾部读取 + JSON 解析为阻塞 I/O 与 CPU 密集操作，
    // 整体放入 spawn_blocking 避免阻塞 tokio worker 线程
    let entries: Vec<crate::web::state::LogEntry> =
        tokio::task::spawn_blocking(move || -> Vec<crate::web::state::LogEntry> {
            // 查找最新的日志文件（按文件名排序，app.log.YYYY-MM-DD 格式）
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
                        // 从最新行开始反向解析 → 过滤本次会话 → 取 limit 条 → 再反转为从旧到新。
                        // 各级别统一保留，展示级别由前端筛选器决定。
                        // 会话过滤：面板只显示本次启动后的日志，不回显历史运行的旧内容。
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

/// 解析 tracing JSON 日志行为 LogEntry
///
/// tracing json 格式：`{"timestamp":"...","level":"INFO","fields":{"message":"..."},"target":"..."}`
/// 保留所有级别；是否展示由前端筛选器决定，保证刷新历史与实时日志一致。
/// 结构化字段拼接到消息尾部（` key=value`），与 WS 实时条目（BroadcastLayer）信息量对齐。
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
    let fields = v.get("fields").and_then(|f| f.as_object());
    let mut message = fields
        .and_then(|f| f.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(fields) = fields {
        for (key, value) in fields {
            if key == "message" {
                continue;
            }
            // 字符串值不带引号（对齐 tracing Debug 渲染的可读性），其余用紧凑 JSON
            let rendered = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            message.push_str(&format!(" {key}={rendered}"));
        }
    }
    let source =
        crate::web::state::normalize_source(v.get("target").and_then(|x| x.as_str()).unwrap_or(""));
    Some(crate::web::state::LogEntry::new(
        level, message, timestamp, source,
    ))
}

// ---- 日志导出 ----

/// 日志导出包总量上限（50MiB，对齐 feedback-bundle 资源上限，防内存 zip 失控）
const LOG_EXPORT_MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024;

/// 导出包内的单个文件（包内相对路径 + 内容）
type CollectedFile = (String, Vec<u8>);

/// 列出目录下文件名满足谓词的普通文件（目录不存在返回空集）
fn list_files_where(dir: &std::path::Path, pred: impl Fn(&str) -> bool) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            (pred(name) && e.path().is_file()).then_some(e.path())
        })
        .collect()
}

/// 收集日志导出包的文件清单（纯同步，供 spawn_blocking 调用与单测直测）
///
/// 覆盖 `logs/app.log*`（tracing_appender 按日轮转，日期后缀越晚越新）与
/// `logs/login_history/*.jsonl`。按文件名降序（新→旧）读取，总量超过
/// `max_total_bytes` 时跳过并记录说明（先读 metadata 防止把超限大文件整块
/// 读入内存），单文件读取失败也只记说明，绝不中断导出。
fn collect_log_export_files(
    base: &std::path::Path,
    max_total_bytes: usize,
) -> (Vec<CollectedFile>, Vec<String>) {
    let mut files: Vec<CollectedFile> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut total = 0usize;

    let mut log_files = list_files_where(&base.join("logs"), |n| n.starts_with("app.log"));
    let mut history_files = list_files_where(&base.join("logs").join("login_history"), |n| {
        n.ends_with(".jsonl")
    });
    // 文件名降序 = 新→旧：超限时优先保留较新文件
    log_files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    history_files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    for (path, rel_prefix) in log_files
        .into_iter()
        .map(|p| (p, "logs"))
        .chain(history_files.into_iter().map(|p| (p, "login_history")))
    {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let rel = format!("{rel_prefix}/{name}");
        let size = std::fs::metadata(&path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        if total + size > max_total_bytes {
            notes.push(format!("文件 {rel} 会使总量超出 50MiB 上限，已跳过"));
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                total += bytes.len();
                files.push((rel, bytes));
            }
            Err(e) => notes.push(format!("读取 {rel} 失败: {e}")),
        }
    }
    (files, notes)
}

/// GET /api/logs/export — 导出日志压缩包（zip，供随 bug 上传排查）
///
/// 包内内容：
/// - `logs/`：全部 app.log* 轮转文件（当前 + 历史）
/// - `login_history/`：登录历史 JSONL
/// - `meta.json`：版本/平台/生成时间 + 脱敏配置摘要（仅布尔位 `has_password`，
///   不含密码明文与加密配置原文）+ 环境就绪状态
/// - `README.txt`：内容与隐私口径说明；截断/读取失败逐条记录，不以 500 打断导出
pub async fn export_logs(
    State(config): State<Arc<dyn crate::config::ConfigApi>>,
    State(environment): State<Arc<dyn crate::environment::EnvironmentApi>>,
) -> Result<impl IntoResponse, ApiError> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let base = config.base_path();
    let rt = config.runtime_snapshot();
    let settings = config.load_settings_async().await;
    let env_status = environment.status();
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();

    // meta 脱敏口径与 feedback-bundle 一致：凭据只给 has_password 布尔位
    let meta = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "generated_at": now.to_rfc3339(),
        "log_level": settings.global.logging.level,
        "log_file_enabled": settings.global.logging.file_enabled,
        "log_retention_days": settings.global.logging.retention_days,
        "browser_channel": rt.browser.browser_channel,
        "active_profile": rt.profile.id,
        "auth_url": rt.profile.auth_url,
        "isp": rt.profile.isp,
        "has_password": !rt.profile.password.as_str().is_empty(),
        "environment": {
            "uv_ready": env_status.uv_ready,
            "python_ready": env_status.python_ready,
            "playwright_ready": env_status.playwright_ready,
        },
    });

    // 目录遍历 + 文件读取为阻塞 I/O，放入 spawn_blocking 避免阻塞 tokio worker
    let collect_base = base.clone();
    let (files, notes) = tokio::task::spawn_blocking(move || {
        collect_log_export_files(&collect_base, LOG_EXPORT_MAX_TOTAL_BYTES)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("日志收集任务失败: {e}")))?;

    let mut readme = String::new();
    readme.push_str("Campus-Auth 日志导出包\n=======================\n\n");
    readme.push_str(&format!(
        "- 导出时间：{}\n",
        now.format("%Y-%m-%d %H:%M:%S")
    ));
    readme.push_str(&format!("- 版本：{}\n", env!("CARGO_PKG_VERSION")));
    readme.push_str("- logs/：运行日志（tracing JSON 格式，每行一个事件）\n");
    readme.push_str("- login_history/：登录历史（JSONL）\n");
    readme.push_str(
        "- meta.json：环境与配置脱敏摘要（不含密码明文与加密配置原文）\n\n请连同 bug 描述一并上传。\n",
    );
    if !notes.is_empty() {
        readme.push_str("\n注意：\n");
        for note in &notes {
            readme.push_str(&format!("- {note}\n"));
        }
    }

    // 内存打 zip（与 feedback-bundle 同款：Deflated + 0o644）
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let mut write_entry = |name: &str, bytes: &[u8]| -> Result<(), ApiError> {
            zw.start_file(name, opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(bytes)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(())
        };
        write_entry("README.txt", readme.as_bytes())?;
        write_entry("meta.json", meta.to_string().as_bytes())?;
        for (rel, bytes) in &files {
            write_entry(rel, bytes)?;
        }
        zw.finish().map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let bytes = buf.into_inner();
    let filename = format!("campus-auth-logs-{stamp}.zip");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    ))
}

/// GET /api/check-update — 检查更新
///
/// 返回字段对齐前端契约：`has_update`(bool) / `latest`(string) / `current`(string) /
/// `error`(string,可选)。网络路径同下载：显式代理（设置 use_proxy）优先，
/// 未配置跟随系统代理。
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

/// GET /api/update-state — 上次更新检查状态（设置页"上次检查时间"数据源）
///
/// 只读回放 `update/last_check.json`：手动"立即检查"与后台自动检查成功/失败
/// 都会刷新；文件缺失（从未检查过）返回空对象，由前端显示"从未检查"。
pub async fn update_state(
    State(updater): State<Arc<dyn UpdaterApi>>,
) -> Result<Json<Value>, ApiError> {
    let state = updater.last_check_state().unwrap_or_default();
    Ok(data(serde_json::to_value(state)?))
}

/// 前端"检查更新"已确认的版本快照（请求体，可省略）
///
/// 省略时服务端重新拉取清单（兼容旧调用方）；提供时必须三项齐备且通过
/// 安全校验（URL 白名单 / 摘要格式 / semver 比较），任一不通过即回退重查。
/// 目的：消除"检查更新展示 v5.0.1 → 点击更新时服务端重查已发布 v5.0.2"的
/// 版本漂移窗口，同时省一次全量清单 + sha 伴随文件拉取。
#[derive(Debug, Default, Deserialize)]
pub struct ApplyUpdateBody {
    /// 已确认的目标版本号
    version: Option<String>,
    /// 已确认的下载包 URL
    url: Option<String>,
    /// 已确认的下载包 SHA256（64 位 hex）
    sha256: Option<String>,
}

impl ApplyUpdateBody {
    /// 三项齐备且通过安全校验时，构造可直接下发的 [`UpdateInfo`]
    ///
    /// 任一校验不通过返回 `None`，调用方回退到服务端重新拉取清单。
    fn into_pinned(self) -> Option<crate::updater::UpdateInfo> {
        let (version, url, sha256) = (self.version?, self.url?, self.sha256?);
        // 与下载路径同一白名单：https 放行，http 仅精确回环。
        // pin 允许任意 https host 并未放宽信任面——release_source_url 本就
        // 可由 config API 修改且走同一白名单
        if !crate::updater::check::is_allowed_update_url(&url) {
            tracing::warn!(%url, "拒绝固定版本更新：URL 未通过白名单校验");
            return None;
        }
        // 摘要须为 64 位 hex（后续 download_and_verify 仍会复核一次内容）
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            tracing::warn!("拒绝固定版本更新：SHA256 格式非法");
            return None;
        }
        // 版本闸门：U3 的"pending 版本不高于当前则跳过"只存在于启动
        // self_replace 路径（updater/mod.rs），helper 替换路径完全没有版本
        // 检查——缺此闸门时 pin 一个旧版本号可畅通走完"下载 → helper 替换"，
        // 形成降级通道
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
        let remote = semver::Version::parse(&version).ok()?;
        if !crate::updater::check::compare_versions(&current, &remote) {
            tracing::warn!(%version, "拒绝固定版本更新：不高于当前版本");
            return None;
        }
        Some(crate::updater::UpdateInfo {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: version,
            update_available: true,
            url,
            sha256,
            // size 不参与下载（进度取响应 Content-Length），pin 路径置空无副作用
            size: None,
            notes: None,
            release_date: None,
        })
    }
}

/// POST /api/system/update — 执行更新（下载 zip 到 staging 并触发助手替换）
///
/// 请求体可携带"检查更新"阶段已确认的版本快照（version/url/sha256）以固定版本；
/// 省略或校验不通过时服务端重新拉取清单（行为与旧版一致）。
/// 检查与下载使用同一网络路径（显式代理优先，未配置跟随系统代理）。
pub async fn apply_update(
    State(updater): State<Arc<dyn UpdaterApi>>,
    body: Option<Json<ApplyUpdateBody>>,
) -> Result<Json<Value>, ApiError> {
    let info = match body.and_then(|Json(b)| b.into_pinned()) {
        Some(pinned) => {
            tracing::info!(version = %pinned.latest_version, "使用前端确认的固定版本");
            pinned
        }
        None => updater
            .check_update()
            .await
            .map_err(|e| ApiError::Internal(format!("检查更新失败: {e}")))?
            .ok_or_else(|| ApiError::BadRequest("当前已是最新版本，无需更新".into()))?,
    };
    tracing::info!(version = %info.latest_version, "开始下载并暂存更新");
    updater.apply_update(&info).await.map_err(|e| {
        tracing::warn!(version = %info.latest_version, "应用更新失败: {e}");
        ApiError::Internal(format!("应用更新失败: {e}"))
    })?;
    Ok(data(serde_json::json!({
        "message": "更新已暂存，重启后生效",
        "version": info.latest_version,
    })))
}

/// GET /api/browsers — 可用浏览器列表
///
/// Playwright 管理的浏览器按实际缓存分别探测；核心引导默认只安装 Chromium。
pub async fn list_browsers(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let settings = state.config.load_settings_async().await;
    let chromium_installed =
        crate::environment::bootstrap::playwright_browser_installed("chromium");
    let firefox_installed = crate::environment::bootstrap::playwright_browser_installed("firefox");
    let webkit_installed = crate::environment::bootstrap::playwright_browser_installed("webkit");
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

#[derive(Debug, Default, Deserialize)]
pub struct InstallPlaywrightQuery {
    browser: Option<String>,
}

fn normalize_playwright_browser(browser: Option<&str>) -> Result<String, ApiError> {
    let browser = browser.unwrap_or("chromium").trim().to_ascii_lowercase();
    if matches!(browser.as_str(), "chromium" | "firefox" | "webkit") {
        Ok(browser)
    } else {
        Err(ApiError::BadRequest(format!(
            "不支持安装浏览器 {browser:?}，仅支持 chromium/firefox/webkit"
        )))
    }
}

fn should_explicitly_install_after_ensure(
    browser: &str,
    playwright_ready_before: bool,
    playwright_ready_after: bool,
) -> bool {
    // 引导刚装完 chromium（false→true）时跳过重复安装；引导因系统浏览器存在
    // 而跳过下载时（after 仍为 false）必须显式安装——此前以 capability_ready
    // 为判据，该场景下首次点击永远装不上却返回成功
    !(browser == "chromium" && !playwright_ready_before && playwright_ready_after)
}

async fn perform_playwright_install(
    environment: &dyn crate::environment::EnvironmentApi,
    browser: &str,
) -> Result<(), crate::environment::EnvironmentError> {
    let playwright_ready_before = environment.status().playwright_ready;
    environment.ensure_capability().await?;
    let playwright_ready_after = environment.status().playwright_ready;
    if should_explicitly_install_after_ensure(
        browser,
        playwright_ready_before,
        playwright_ready_after,
    ) {
        environment.install_playwright_browser(browser).await?;
    }
    Ok(())
}

/// POST /api/install/playwright — 安装 Playwright 管理浏览器
///
/// `?browser=chromium|firefox|webkit`；省略参数保持旧行为，默认 Chromium。
/// 请求会等待安装完成：后端失败直接返回错误，避免前端只能长时间轮询猜测结果。
pub async fn install_playwright(
    State(environment): State<Arc<dyn crate::environment::EnvironmentApi>>,
    Query(params): Query<InstallPlaywrightQuery>,
) -> Result<Json<Value>, ApiError> {
    let browser = normalize_playwright_browser(params.browser.as_deref())?;
    tracing::info!(browser = %browser, "开始安装 Playwright 浏览器");
    perform_playwright_install(environment.as_ref(), &browser)
        .await
        .map_err(|e| {
            tracing::warn!(browser = %browser, "Playwright {browser} 安装失败: {e}");
            ApiError::Internal(format!("Playwright {browser} 安装失败: {e}"))
        })?;
    Ok(data(serde_json::json!({
        "browser": browser,
        "installed": true,
        "message": "Playwright 浏览器安装完成",
    })))
}

/// POST /api/environment/bootstrap — 初始化 Python 环境（uv sync + Chromium）
///
/// 复用 `EnvironmentApi::ensure_capability`，经 `BootstrapGate` 保证并发幂等：
/// 并发点击只跑一次下载/同步，其余等待者复用首个结果。同步等待完成直接返回
/// 结果供按钮展示成功/失败，避免前端额外轮询竞态（对齐 `install_playwright` 的同步模型）。
pub async fn bootstrap_environment(
    State(environment): State<Arc<dyn crate::environment::EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    environment
        .ensure_capability()
        .await
        .map_err(|e| ApiError::Internal(format!("环境初始化失败: {e}")))?;
    let st = environment.status();
    Ok(data(serde_json::json!({
        "capability_ready": st.capability_ready,
        "uv_ready": st.uv_ready,
        "python_ready": st.python_ready,
        "playwright_ready": st.playwright_ready,
        "stage": format!("{:?}", st.stage),
        "progress": st.progress,
        "last_error": st.last_error,
    })))
}

/// GET /api/icons — 可用图标列表
///
/// 扫描资源图标目录返回可用图标。目录不存在时返回空列表。
pub async fn list_icons(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let icons_dir = state.config.base_path().join("resources").join("icons");
    let mut icons = Vec::new();
    // 目录扫描用 tokio::fs，避免同步 std::fs 阻塞 tokio worker 线程
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

// ---- 文档 ----

/// 便携包/开发期均可用：优先读磁盘 `docs/guides/task-writing-guide.md`（便于热更），
/// 缺失时回退到编译期嵌入的 `GuideAsset`（release 包不含 `docs/` 目录时不 404）。
#[cfg(not(feature = "no-embed"))]
fn embedded_guide() -> Option<String> {
    crate::web::static_files::GuideAsset::get("task-writing-guide.md")
        .and_then(|a| String::from_utf8(a.data.into_owned()).ok())
}

#[cfg(feature = "no-embed")]
fn embedded_guide() -> Option<String> {
    None
}

/// 在候选目录中查找首个存在的任务编写指南
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

/// GET /api/docs/task-writing-guide — 任务编写指南
///
/// 优先从 `docs/guides/task-writing-guide.md` 读取并以 Markdown 文件形式返回；
/// 便携包缺 `docs/` 时回退到编译期嵌入的副本，避免 404。
/// 注意与 `Json` 信封区分：浏览器直接打开显示文本，`download` 链接触发下载。
pub async fn task_writing_guide(
    State(config): State<Arc<dyn crate::config::ConfigApi>>,
) -> Result<Response, ApiError> {
    let path = resolve_guide_path(&config.base_path());
    // tokio::fs 异步读取，避免同步 std::fs 阻塞 tokio worker 线程
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(markdown_response("task-writing-guide.md", content)),
        Err(e) => {
            if let Some(content) = embedded_guide() {
                tracing::debug!("任务编写指南回退到嵌入副本: {e}");
                return Ok(markdown_response("task-writing-guide.md", content));
            }
            tracing::warn!("任务编写指南加载失败 ({path:?}): {e}");
            Err(ApiError::NotFound(
                "任务编写指南文件缺失，可能需要重新安装或更新软件".to_string(),
            ))
        }
    }
}

/// Markdown 文本以文件形式返回（与 tools.rs 的脚本直返保持一致）：
/// `text/markdown` 让浏览器显示为文本，`download` 属性的链接触发下载。
fn markdown_response(filename: &str, content: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(content))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response())
}

/// GET /api/docs/task-manual — 任务使用手册
///
/// 与编写指南同策略：优先读磁盘 `docs/guides/task-manual.md`（便于热更），
/// 缺失时回退到编译期嵌入的副本，避免 404。
pub async fn task_manual(
    State(config): State<Arc<dyn crate::config::ConfigApi>>,
) -> Result<Response, ApiError> {
    let path = resolve_manual_path(&config.base_path());
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(markdown_response("task-manual.md", content)),
        Err(e) => {
            if let Some(content) = embedded_manual() {
                tracing::debug!("任务使用手册回退到嵌入副本: {e}");
                return Ok(markdown_response("task-manual.md", content));
            }
            tracing::warn!("任务使用手册加载失败 ({path:?}): {e}");
            Err(ApiError::NotFound(
                "任务使用手册文件缺失，可能需要重新安装或更新软件".to_string(),
            ))
        }
    }
}

/// 在候选目录中查找首个存在的任务使用手册（与编写指南同目录）
fn resolve_manual_path(base_path: &std::path::Path) -> std::path::PathBuf {
    let rel = std::path::Path::new("docs")
        .join("guides")
        .join("task-manual.md");
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

#[cfg(not(feature = "no-embed"))]
fn embedded_manual() -> Option<String> {
    crate::web::static_files::GuideAsset::get("task-manual.md")
        .and_then(|a| String::from_utf8(a.data.into_owned()).ok())
}

#[cfg(feature = "no-embed")]
fn embedded_manual() -> Option<String> {
    None
}

// ---- 工具函数 ----
// SSRF 私网判定与安全 GET（DNS 钉扎 + 逐跳重定向校验）已统一收敛至 crate::web::ssrf

/// POST /api/worker/stop — 手动关闭浏览器（优雅停止 Python Worker）
///
/// 停止当前运行的 Worker 进程及其浏览器实例。Supervisor 保持运行，
/// 下次任务到来时会自动重新启动 Worker。
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
    fn normalize_playwright_browser_defaults_and_validates() {
        assert_eq!(normalize_playwright_browser(None).unwrap(), "chromium");
        assert_eq!(
            normalize_playwright_browser(Some(" Firefox ")).unwrap(),
            "firefox"
        );
        assert_eq!(
            normalize_playwright_browser(Some("WEBKIT")).unwrap(),
            "webkit"
        );
        assert!(matches!(
            normalize_playwright_browser(Some("chrome")),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn playwright_browser_install_decision_avoids_first_chromium_duplicate() {
        // 引导刚装完 chromium（false→true）：跳过重复安装
        assert!(!should_explicitly_install_after_ensure(
            "chromium", false, true
        ));
        // 引导因系统浏览器跳过下载（false→false）：必须显式安装（此前此场景谎报成功）
        assert!(should_explicitly_install_after_ensure(
            "chromium", false, false
        ));
        // 引导前 chromium 已就绪（true→true）：显式安装保持幂等
        assert!(should_explicitly_install_after_ensure(
            "chromium", true, true
        ));
        // 非 chromium 引擎始终显式安装
        assert!(should_explicitly_install_after_ensure(
            "firefox", false, false
        ));
        assert!(should_explicitly_install_after_ensure(
            "firefox", false, true
        ));
        assert!(should_explicitly_install_after_ensure(
            "webkit", false, false
        ));
    }

    struct MockInstallEnvironment {
        ensure_fails: bool,
        install_fails: bool,
        /// ensure 前的 playwright_ready（引导前状态）
        playwright_ready_before: bool,
        /// ensure 后的 playwright_ready（模拟引导是否真的装了 chromium）
        playwright_ready_after: bool,
        ensured: std::sync::atomic::AtomicBool,
        ensure_calls: std::sync::atomic::AtomicUsize,
        install_calls: std::sync::atomic::AtomicUsize,
    }

    impl MockInstallEnvironment {
        fn new(ensure_fails: bool, install_fails: bool) -> Self {
            Self {
                ensure_fails,
                install_fails,
                playwright_ready_before: false,
                playwright_ready_after: true,
                ensured: std::sync::atomic::AtomicBool::new(false),
                ensure_calls: std::sync::atomic::AtomicUsize::new(0),
                install_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn with_playwright_ready(mut self, before: bool, after: bool) -> Self {
            self.playwright_ready_before = before;
            self.playwright_ready_after = after;
            self
        }
    }

    #[async_trait::async_trait]
    impl crate::environment::EnvironmentApi for MockInstallEnvironment {
        fn status(&self) -> crate::environment::EnvironmentStatus {
            let ready = if self.ensured.load(Ordering::SeqCst) {
                self.playwright_ready_after
            } else {
                self.playwright_ready_before
            };
            crate::environment::EnvironmentStatus {
                uv_ready: ready,
                python_ready: ready,
                playwright_ready: ready,
                capability_ready: ready,
                stage: crate::environment::BootstrapStage::Idle,
                progress: None,
                last_error: None,
            }
        }

        fn python_path(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }

        async fn ensure_capability(&self) -> Result<(), crate::environment::EnvironmentError> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            if self.ensure_fails {
                Err(crate::environment::EnvironmentError::Cancelled)
            } else {
                self.ensured.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        async fn install_playwright_browser(
            &self,
            _browser: &str,
        ) -> Result<(), crate::environment::EnvironmentError> {
            self.install_calls.fetch_add(1, Ordering::SeqCst);
            if self.install_fails {
                Err(crate::environment::EnvironmentError::Cancelled)
            } else {
                Ok(())
            }
        }

        async fn install_ocr_dep(&self) -> Result<(), crate::environment::EnvironmentError> {
            Ok(())
        }

        async fn remove_ocr_dep(&self) -> Result<(), crate::environment::EnvironmentError> {
            Ok(())
        }

        fn ocr_ready(&self) -> bool {
            false
        }

        fn ocr_declared(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn playwright_install_propagates_core_failure_without_explicit_install() {
        let env = MockInstallEnvironment::new(true, false);
        let result = perform_playwright_install(&env, "firefox").await;
        assert!(matches!(
            result,
            Err(crate::environment::EnvironmentError::Cancelled)
        ));
        assert_eq!(env.ensure_calls.load(Ordering::SeqCst), 1);
        assert_eq!(env.install_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn playwright_install_waits_for_requested_browser_and_propagates_failure() {
        let env = MockInstallEnvironment::new(false, true);
        let result = perform_playwright_install(&env, "firefox").await;
        assert!(matches!(
            result,
            Err(crate::environment::EnvironmentError::Cancelled)
        ));
        assert_eq!(env.ensure_calls.load(Ordering::SeqCst), 1);
        assert_eq!(env.install_calls.load(Ordering::SeqCst), 1);
    }

    /// 引导因系统浏览器存在而跳过 chromium 下载（false→false）时，
    /// 显式安装必须执行：此前该场景静默 no-op 却返回安装成功
    #[tokio::test]
    async fn playwright_install_installs_chromium_when_bootstrap_skipped_download() {
        let env = MockInstallEnvironment::new(false, false).with_playwright_ready(false, false);
        let result = perform_playwright_install(&env, "chromium").await;
        assert!(result.is_ok());
        assert_eq!(env.install_calls.load(Ordering::SeqCst), 1);
    }

    /// 引导刚装完 chromium（false→true）时跳过重复安装（防首次点击双下载）
    #[tokio::test]
    async fn playwright_install_skips_chromium_just_installed_by_bootstrap() {
        let env = MockInstallEnvironment::new(false, false).with_playwright_ready(false, true);
        let result = perform_playwright_install(&env, "chromium").await;
        assert!(result.is_ok());
        assert_eq!(env.install_calls.load(Ordering::SeqCst), 0);
    }

    // ============ tracing JSON 日志解析 ============

    #[test]
    fn parse_tracing_json_log_extracts_fields() {
        let line = r#"{"timestamp":"2026-08-14T01:02:03Z","level":"INFO","fields":{"message":"登录成功","task_id":"t1","attempt":2},"target":"campus_auth::login"}"#;
        let entry = parse_tracing_json_log(line).expect("应解析成功");
        assert_eq!(entry.level, "INFO");
        // 结构化字段拼接到消息尾部，与 WS 实时条目（BroadcastLayer）信息量对齐；
        // 经 JSON 往返后字段按字母序排列（serde_json 默认 BTreeMap），顺序不保证与发射一致
        assert_eq!(entry.message, "登录成功 attempt=2 task_id=t1");
        // source 经 normalize_source 归一化为五大域（login → auth）
        assert_eq!(entry.source, "auth");
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
        // 缺少 fields.message 时回退为空字符串而非报错
        let entry = parse_tracing_json_log(r#"{"level":"WARN"}"#).expect("结构不完整也应解析");
        assert_eq!(entry.message, "");
    }

    // ============ check_update handler 级单测（内存 MockUpdaterApi，M1） ============

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get as route_get;
    use tower::ServiceExt; // oneshot

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

        fn last_check_state(&self) -> Option<crate::updater::LastCheckState> {
            None
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

    /// 有新版本：UpdateInfo 原样透传
    #[tokio::test]
    async fn test_check_update_with_info() {
        let v = run_check(MockOutcome::Info(sample_info())).await;
        let d = &v["data"];
        assert_eq!(d["has_update"], true);
        assert_eq!(d["latest"], "1.1.0");
        assert_eq!(d["current"], "1.0.0");
    }

    /// 无更新：返回 has_update=false 与当前版本
    #[tokio::test]
    async fn test_check_update_no_update() {
        let v = run_check(MockOutcome::None).await;
        let d = &v["data"];
        assert_eq!(d["has_update"], false);
        assert_eq!(d["latest"], env!("CARGO_PKG_VERSION"));
        assert!(d.get("error").is_none());
    }

    /// 检查失败：200 + error 字段（前端提示，非 5xx）
    #[tokio::test]
    async fn test_check_update_error_field() {
        let v = run_check(MockOutcome::Err("网络超时".into()));
        let v = v.await;
        let d = &v["data"];
        assert_eq!(d["has_update"], false);
        assert!(d["error"].as_str().unwrap().contains("网络超时"));
    }

    // ============ GET /api/logs/export 日志导出包 ============

    use super::super::test_support::MockConfigApi;

    /// 双域 state：ConfigApi + EnvironmentApi 各自经 FromRef 委派提取
    #[derive(Clone)]
    struct ExportTestState {
        config: Arc<dyn crate::config::ConfigApi>,
        environment: Arc<dyn crate::environment::EnvironmentApi>,
    }

    impl axum::extract::FromRef<ExportTestState> for Arc<dyn crate::config::ConfigApi> {
        fn from_ref(state: &ExportTestState) -> Self {
            state.config.clone()
        }
    }

    impl axum::extract::FromRef<ExportTestState> for Arc<dyn crate::environment::EnvironmentApi> {
        fn from_ref(state: &ExportTestState) -> Self {
            state.environment.clone()
        }
    }

    fn export_app(base: std::path::PathBuf) -> axum::Router {
        let (config, cfg) = MockConfigApi::mocked();
        cfg.lock().unwrap().base_path = base;
        let environment: Arc<dyn crate::environment::EnvironmentApi> =
            Arc::new(MockInstallEnvironment::new(false, false));
        axum::Router::new()
            .route("/api/logs/export", route_get(export_logs))
            .with_state(ExportTestState {
                config,
                environment,
            })
    }

    async fn run_export(base: std::path::PathBuf) -> axum::http::Response<Body> {
        export_app(base)
            .oneshot(
                Request::builder()
                    .uri("/api/logs/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn zip_names(bytes: &[u8]) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// 导出包含 README/meta/全部日志轮转文件与登录历史，无关文件不进包
    #[tokio::test]
    async fn logs_export_contains_logs_history_meta_and_readme() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        let hist = logs.join("login_history");
        std::fs::create_dir_all(&hist).unwrap();
        std::fs::write(logs.join("app.log.2026-09-05"), "{\"level\":\"INFO\"}\n").unwrap();
        std::fs::write(logs.join("app.log.2026-09-06"), "{\"level\":\"WARN\"}\n").unwrap();
        // 无关文件不应进包
        std::fs::write(logs.join("not-a-log.txt"), "x").unwrap();
        std::fs::write(hist.join("2026-09-06.jsonl"), "{\"result\":\"success\"}\n").unwrap();

        let resp = run_export(tmp.path().to_path_buf()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "application/zip");
        assert!(
            resp.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment; filename=\"campus-auth-logs-")
        );

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut names = zip_names(&bytes);
        names.sort();
        assert_eq!(
            names,
            vec![
                "README.txt",
                "login_history/2026-09-06.jsonl",
                "logs/app.log.2026-09-05",
                "logs/app.log.2026-09-06",
                "meta.json",
            ]
        );

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut meta = String::new();
        use std::io::Read;
        zip.by_name("meta.json")
            .unwrap()
            .read_to_string(&mut meta)
            .unwrap();
        assert!(meta.contains("\"has_password\":false"), "{meta}");
    }

    /// logs 目录缺失：仍返回有效包（README + meta），不 500
    #[tokio::test]
    async fn logs_export_without_logs_dir_still_returns_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let resp = run_export(tmp.path().to_path_buf()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let names = zip_names(&bytes);
        assert!(names.contains(&"README.txt".to_string()), "{names:?}");
        assert!(names.contains(&"meta.json".to_string()), "{names:?}");
        assert_eq!(names.len(), 2);
    }

    /// 总量超上限：旧文件被跳过且记录说明，新文件保留（降序 = 新→旧优先）
    #[test]
    fn collect_log_export_files_respects_total_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("app.log.2026-09-05"), "0123456789".repeat(10)).unwrap();
        std::fs::write(logs.join("app.log.2026-09-06"), "new").unwrap();
        let (files, notes) = collect_log_export_files(tmp.path(), 50);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["logs/app.log.2026-09-06"]);
        assert!(
            notes.iter().any(|n| n.contains("app.log.2026-09-05")),
            "{notes:?}"
        );
    }
}
