//! 日志子系统：初始化、动态级别热更新、WebSocket 广播层与日志条目类型
//!
//! 从 launcher.rs 迁出（A-1）。核心改进：广播层由「fmt 层格式化文本 →
//! 正则反解析」改为真正的 `Layer` 实现——`on_event` 直接从 metadata 与
//! 字段 visitor 构造 [`LogEntry`]，消除时间戳伪造（原实现取 chrono::now
//! 而非事件时间）、非标准行静默降级 INFO 这一类正确性缺陷，且每条日志
//! 省一次序列化 + 解析。

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

pub use tracing_appender::non_blocking::WorkerGuard;

/// WebSocket 日志条目（由内部事件推入广播通道，供 /ws/logs 订阅）
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    /// 全局单调递增序号（进程生命周期内唯一）
    ///
    /// 用途：前端 v-for 稳定 key（index key 在缓冲裁剪后导致整列表重建）、
    /// 实时日志去重（同毫秒同文案的两条日志不再被误判为重复）、
    /// 自动滚动触发依据（watch 长度在缓冲满员后不再变化）
    pub seq: u64,
    /// 日志级别（INFO/WARN/ERROR…）
    pub level: String,
    /// 日志消息
    pub message: String,
    /// ISO8601 时间戳
    pub timestamp: String,
    /// 日志来源（归一化后的短模块名，如 `launcher`/`scheduler`，由 tracing target 派生）
    #[serde(default)]
    pub source: String,
}

/// 日志序号发生器（全局单调递增）
static NEXT_LOG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LogEntry {
    /// 构造日志条目并分配单调序号（所有构造路径统一走此入口）
    pub fn new(level: String, message: String, timestamp: String, source: String) -> Self {
        Self {
            seq: NEXT_LOG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            level,
            message,
            timestamp,
            source,
        }
    }
}

/// 将 tracing target 归一化为面向用户的粗粒度来源，供前端来源过滤与展示
///
/// 五大域映射（与前端 `LOG_SOURCE_LABELS` 一一对应，新增模块时两处同步维护）：
/// - 系统 `app`：应用骨架（launcher/container/tray/config/updater/web 及 crate 根）
/// - 认证 `auth`：网络与认证链路（engine/login/monitor/network）
/// - 任务 `task`：定时任务与通知（scheduler/tasks/notification/ai）
/// - 执行器 `worker`：Python 执行侧（bridge/environment/python_worker）
/// - 界面 `frontend`：前端日志回流（target 即 `frontend`）
///
/// 未识别的首段（第三方 crate，如 `hyper_util::client`）保留原样展示。
/// 文件日志 JSON 始终保留完整 target，本映射仅影响 WS 面板与历史接口的 source 字段。
pub fn normalize_source(target: &str) -> String {
    let t = target.trim();
    if t.is_empty() {
        return String::new();
    }
    // 去掉 crate 前缀 `campus_auth::`
    let rest = t.strip_prefix("campus_auth::").unwrap_or(t);
    let first = rest
        .split("::")
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if first.is_empty() || first == "campus_auth" {
        // crate 根模块（target 恰好为 `campus_auth`）
        return "app".to_string();
    }
    match first.as_str() {
        "app" | "launcher" | "container" | "tray" | "config" | "updater" | "web" => "app",
        "engine" | "login" | "monitor" | "network" => "auth",
        "scheduler" | "tasks" | "notification" | "ai" => "task",
        "bridge" | "environment" | "python_worker" => "worker",
        "frontend" => "frontend",
        _ => first.as_str(),
    }
    .to_string()
}

// ============================================================
// 动态 filter（多 layer 共享，热更新）
// ============================================================

/// 自定义日志计时器：YYYY-MM-DD HH:MM:SS 本地时间
#[derive(Clone)]
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))
    }
}

/// 全局日志 filter（`SharedTargets` 让多个 layer 共享同一份可变配置，支持热更新）
static LOG_TARGETS: OnceLock<SharedTargets> = OnceLock::new();

/// 共享动态日志 filter：多 layer 共享同一份可变 `Targets`
///
/// `Targets` 是纯值类型：旧实现 `targets.clone().with_target(...)` 只修改被
/// 丢弃的副本（且各 layer 持有独立 filter 副本），热更新从不生效。
/// 此包装让三个 fmt layer 持有同一 `Arc<Mutex<Targets>>`，
/// `reload_log_level` 整体替换内部值即可对所有层即时生效。
#[derive(Clone, Default)]
struct SharedTargets(Arc<Mutex<tracing_subscriber::filter::Targets>>);

/// 构建动态 filter 规则：第三方库保持 WARN，本项目 target 指定级别
///
/// `python_worker` 是 Worker stderr 转发使用的 target（bridge/process.rs），与
/// `campus_auth`/`frontend` 同级对待——漏配会落到默认 WARN，Worker 的 INFO 及
/// 以下日志在控制台/文件/WS 三路同时丢失。`build` 与 `reload_log_level` 共用
/// 本函数，保证启动与热更新的规则永远一致。
fn build_targets(
    lf: tracing_subscriber::filter::LevelFilter,
) -> tracing_subscriber::filter::Targets {
    tracing_subscriber::filter::Targets::new()
        .with_default(tracing_subscriber::filter::LevelFilter::WARN)
        .with_target("campus_auth", lf)
        .with_target("frontend", lf)
        .with_target("python_worker", lf)
}

impl SharedTargets {
    /// 构造默认规则（见 [`build_targets`]）
    fn build(lf: tracing_subscriber::filter::LevelFilter) -> Self {
        Self::new(build_targets(lf))
    }

    fn new(targets: tracing_subscriber::filter::Targets) -> Self {
        Self(Arc::new(Mutex::new(targets)))
    }

    /// 热更新：整体替换内部 Targets（各层下次判定即读到新值）
    fn replace(&self, targets: tracing_subscriber::filter::Targets) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = targets;
    }
}

impl<S> tracing_subscriber::layer::Filter<S> for SharedTargets {
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // UFCS 消歧：Targets 同时实现了 Layer 与 Filter 两个 trait
        tracing_subscriber::layer::Filter::enabled(&*guard, metadata, cx)
    }

    fn callsite_enabled(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        // filter 会动态变化，必须禁用 Interest 缓存：
        // 否则级别调整前判为 never 的 callsite 会被缓存结果永久拦截
        tracing::subscriber::Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        tracing_subscriber::layer::Filter::<()>::max_level_hint(&*guard)
    }
}

/// 解析日志级别字符串（无效值回退 INFO 并告警）
pub(crate) fn parse_level(level: &str) -> tracing_subscriber::filter::LevelFilter {
    use tracing_subscriber::filter::LevelFilter;
    match level.to_ascii_uppercase().as_str() {
        "TRACE" => LevelFilter::TRACE,
        "DEBUG" => LevelFilter::DEBUG,
        "WARN" | "WARNING" => LevelFilter::WARN,
        "ERROR" => LevelFilter::ERROR,
        // 无效级别静默回退会让"配置了却不生效"无从排查，至少 warn 一次
        _ => {
            tracing::warn!(raw = %level, "无效的日志级别配置，回退 INFO");
            LevelFilter::INFO
        }
    }
}

/// 从已解析的 settings.json `Value` 一次性提取日志配置（级别 + 保留天数 + 文件开关）
///
/// 由 `launcher` 在单次文件读取后调用，避免启动期对同一文件三次读解析
///（启动字段 / 日志级别 / 保留天数各读一次的历史包袱）。缺失/非法时回退 INFO / 7 天 / 写文件。
pub(crate) fn logging_config_from_value(
    value: &serde_json::Value,
) -> (tracing_subscriber::filter::LevelFilter, u32, bool) {
    let level = value
        .get("global")
        .and_then(|g| g.get("logging"))
        .and_then(|l| l.get("level"))
        .and_then(|v| v.as_str())
        .map(parse_level)
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    let retention = value
        .get("global")
        .and_then(|g| g.get("logging"))
        .and_then(|l| l.get("retention_days"))
        .and_then(|v| v.as_u64())
        .map(|d| d as u32)
        .unwrap_or(7);
    let file_enabled = value
        .get("global")
        .and_then(|g| g.get("logging"))
        .and_then(|l| l.get("file_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    (level, retention, file_enabled)
}

/// 热更新全局日志级别（由 `set_log_level` 调用）
///
/// 项目 target 白名单与启动时保持一致（见 [`build_targets`]）。无效级别回退 INFO。
pub fn reload_log_level(level: &str) {
    let lf = parse_level(level);
    let Some(shared) = LOG_TARGETS.get() else {
        tracing::warn!(level = %level, "日志 filter 未初始化，忽略级别切换");
        return;
    };
    shared.replace(build_targets(lf));
    tracing::info!(level = %lf, "日志级别已热更新");
}

// ============================================================
// 文件保留清理
// ============================================================

/// 清理过期日志文件（每日兜底任务的入口；启动时的首次清理在 `init_logging` 内）
pub fn cleanup_expired_logs(base_path: &Path, retention_days: u32) {
    let logs_dir = crate::utils::paths::logs_dir(base_path);
    cleanup_old_logs(&logs_dir, retention_days, LOG_TOTAL_QUOTA_BYTES);
}

/// 日志总配额兜底（COR-9，软上限）：`app.log*` 文件总字节数超过该值时
/// 从最旧的轮转文件删起，尽量压回预算。
///
/// 软配额而非硬上限：当日活跃 `app.log` 被 writer 持有，Windows 下无法
/// 删除或截断，极端日志量单日仍可能突破预算；隔天轮转后由本配额回收。
const LOG_TOTAL_QUOTA_BYTES: u64 = 200 * 1024 * 1024;

/// 删除 logs/ 目录下超过保留天数或超出总配额的旧日志文件
///
/// 保留天数优先：仅删除修改时间早于 cutoff 的 `app.log*` 轮转文件，跳过当前
/// 正在写入的 `app.log`（`tracing_appender::rolling::daily` 生成
/// `app.log.YYYY-MM-DD`）；随后执行总配额兜底，从最旧文件删起，删除失败
/// （Windows 句柄锁等）跳过继续。`quota_bytes` 参数化以便测试注入小配额。
fn cleanup_old_logs(logs_dir: &Path, retention_days: u32, quota_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        tracing::warn!("读取日志目录失败，跳过过期日志清理");
        return;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::from(retention_days) * 86_400);
    // 一次目录扫描同时服务保留天数与总配额两条清理路径
    let mut rotated: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut active_size = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("app.log") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if name == "app.log" {
            // 当前活跃文件被 writer 持有，只参与配额计算，不参与删除
            active_size = meta.len();
        } else {
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            rotated.push((path, meta.len(), modified));
        }
    }

    // 第一步：保留天数清理
    let mut removed = 0usize;
    rotated.retain(|(path, _, modified)| {
        if *modified < cutoff {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    removed += 1;
                    false
                }
                Err(e) => {
                    tracing::debug!("删除过期日志文件失败 {}: {e}", path.display());
                    true
                }
            }
        } else {
            true
        }
    });
    if removed > 0 {
        tracing::info!(
            "清理过期日志文件 {} 个（保留 {} 天）",
            removed,
            retention_days
        );
    }

    // 第二步：总配额兜底（COR-9）。从最旧（mtime 最小）的轮转文件删起，
    // 单个删除失败跳过继续，尽力压回预算
    let total: u64 = active_size + rotated.iter().map(|(_, size, _)| size).sum::<u64>();
    if total <= quota_bytes {
        return;
    }
    let mut total = total;
    let mut quota_removed = 0usize;
    rotated.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in &rotated {
        if total <= quota_bytes {
            break;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                total = total.saturating_sub(*size);
                quota_removed += 1;
            }
            Err(e) => tracing::debug!("配额清理删除日志文件失败 {}: {e}", path.display()),
        }
    }
    if quota_removed > 0 {
        tracing::info!(
            "日志总配额超限，已清理最旧轮转文件 {} 个（软上限 {} MiB，当日活跃文件不受影响）",
            quota_removed,
            LOG_TOTAL_QUOTA_BYTES / (1024 * 1024)
        );
    }
}

// ============================================================
// WebSocket 广播层
// ============================================================

/// 字段 visitor：提取 `message` 字段并收集其余结构化字段
#[derive(Default)]
struct EventFields {
    message: Option<String>,
    /// 其余字段按出现顺序拼接为 ` key=value` 后缀（对齐旧 fmt 层渲染语义）
    extras: Vec<(String, String)>,
}

impl tracing::field::Visit for EventFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            // message 字段是 tracing 宏的格式化主体；重复出现时追加而非覆盖
            match &mut self.message {
                Some(existing) => existing.push_str(&rendered),
                None => self.message = Some(rendered),
            }
        } else {
            self.extras.push((field.name().to_string(), rendered));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            match &mut self.message {
                Some(existing) => existing.push_str(value),
                None => self.message = Some(value.to_string()),
            }
        } else {
            self.extras
                .push((field.name().to_string(), value.to_string()));
        }
    }
}

/// WebSocket 广播层：直接从 tracing 事件构造 [`LogEntry`] 推入 broadcast channel
///
/// 取代旧的「fmt 层写文本 → parse_log_line 反解析」路径。级别与 target 来自
/// metadata（权威来源），消息来自字段 visitor，无任何文本解析。
struct BroadcastLayer {
    tx: tokio::sync::broadcast::Sender<LogEntry>,
}

impl<S: Subscriber> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let level = event.metadata().level().to_string();
        let source = normalize_source(event.metadata().target());
        // 结构化字段拼接到消息尾部（key=value），保持旧输出的信息量
        let mut message = fields.message.unwrap_or_default();
        for (key, value) in &fields.extras {
            message.push_str(&format!(" {key}={value}"));
        }
        // 与文件层（%Y-%m-%d %H:%M:%S）统一格式：RFC3339 会让 WS 实时日志与
        // 历史日志/文件日志呈现两套时间样式；前端 formatTimestamp 对两种格式
        // 截断结果一致，此变更对展示无回归风险
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = self
            .tx
            .send(LogEntry::new(level, message, timestamp, source));
    }
}

/// 创建日志广播通道的发送端
pub fn log_broadcast_tx() -> tokio::sync::broadcast::Sender<LogEntry> {
    let (tx, _) = tokio::sync::broadcast::channel(1024);
    tx
}

// ============================================================
// 初始化
// ============================================================

/// 本次会话的起始时间（日志子系统初始化时刻，格式与文件日志行一致）。
/// `/api/logs` 据此只返回本次启动后的日志，面板不回显历史运行的旧内容。
static SESSION_STARTED_AT: OnceLock<String> = OnceLock::new();

/// 会话起始时间戳（`%Y-%m-%d %H:%M:%S`）；日志系统未初始化时返回 None
pub fn session_started_at() -> Option<&'static str> {
    SESSION_STARTED_AT.get().map(String::as_str)
}

/// 日志系统未初始化时返回 true 判定辅助（panic hook 据此选择输出通道：
/// subscriber 就绪前 tracing 宏是 no-op，panic 不能无声丢失）
pub fn logging_initialized() -> bool {
    LOG_TARGETS.get().is_some()
}

/// 初始化日志系统：控制台层 + 文件层（按日期轮转）+ 广播层（WebSocket 推送）
///
/// 全局 subscriber 只能 init 一次，所有层在此统一注册。
/// 日志级别、保留天数与文件开关由 `launcher` 单次解析 settings.json 后传入，
/// 本函数不再重复读文件（历史三读：启动字段 / 级别 / 保留天数各一次）。
/// `file_enabled = false` 时跳过文件层，不创建 appender 也不落盘，
/// 返回 `None`（历史接口 /api/logs 在无日志文件时返回空列表）。
pub fn init_logging(
    base_path: &Path,
    log_tx: tokio::sync::broadcast::Sender<LogEntry>,
    log_level: tracing_subscriber::filter::LevelFilter,
    retention_days: u32,
    file_enabled: bool,
) -> Option<WorkerGuard> {
    let logs_dir = crate::utils::paths::logs_dir(base_path);
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        tracing::warn!("创建日志目录失败: {e}");
    }

    // 会话起始时间：与 LocalTimer 同格式，供 /api/logs 过滤历史运行日志
    let _ = SESSION_STARTED_AT.set(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());

    // 启动时清理过期日志：按传入的 logging.retention_days 保留，
    // 删除超过保留天数的旧轮转文件，避免日志无限累积（对齐原项目 loguru retention）。
    cleanup_old_logs(&logs_dir, retention_days, LOG_TOTAL_QUOTA_BYTES);

    // 动态 filter：三个 layer 共享同一 SharedTargets（热更新入口见 reload_log_level）。
    let shared = SharedTargets::build(log_level);
    let _ = LOG_TARGETS.set(shared.clone());

    // 本地时区计时器：YYYY-MM-DD HH:MM:SS 格式
    let local_timer = LocalTimer;

    // 控制台层：人类可读格式输出到 stderr
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr)
        .with_timer(local_timer.clone())
        .with_filter(shared.clone());

    // 文件层：JSON 格式按日轮转；关闭时整体跳过（`registry().with(Option<Layer>)` 合法）
    let file_appender = file_enabled
        .then(|| tracing_appender::rolling::daily(&logs_dir, "app.log"))
        .map(tracing_appender::non_blocking);
    let file_layer = file_appender.as_ref().map(|(writer, _)| {
        tracing_subscriber::fmt::layer()
            .with_writer(writer.clone())
            .with_ansi(false)
            .with_target(true)
            .with_timer(local_timer)
            .json()
            .with_filter(shared.clone())
    });

    // 广播层：真实 Layer，on_event 直接构造 LogEntry（无文本中转）
    let broadcast_layer = BroadcastLayer { tx: log_tx }.with_filter(shared);

    if let Err(e) = tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .with(broadcast_layer)
        .try_init()
    {
        eprintln!("日志层注册失败（可能已初始化）: {e}");
    }

    file_appender.map(|(_, guard)| guard)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 来源归一化：前后端来源过滤与徽章展示依赖五大域映射一致
    #[test]
    fn test_normalize_source_mapping() {
        // 任务域
        assert_eq!(
            normalize_source("campus_auth::scheduler::cron_loop"),
            "task"
        );
        assert_eq!(normalize_source("notification"), "task");
        assert_eq!(normalize_source("campus_auth::tasks::loader"), "task");
        assert_eq!(normalize_source("campus_auth::ai::llm"), "task");
        // 系统域
        assert_eq!(normalize_source("campus_auth"), "app");
        assert_eq!(normalize_source("campus_auth::launcher"), "app");
        assert_eq!(normalize_source("campus_auth::web::routes::config"), "app");
        assert_eq!(normalize_source("campus_auth::updater"), "app");
        // 认证域
        assert_eq!(normalize_source("campus_auth::login"), "auth");
        assert_eq!(normalize_source("campus_auth::monitor::probe"), "auth");
        // 执行器域
        assert_eq!(normalize_source("python_worker"), "worker");
        assert_eq!(normalize_source("campus_auth::bridge"), "worker");
        assert_eq!(normalize_source("campus_auth::environment::uv"), "worker");
        // 界面域
        assert_eq!(normalize_source("frontend"), "frontend");
        // 大小写归一后再映射
        assert_eq!(normalize_source("campus_auth::Scheduler"), "task");
        // 未识别首段（第三方 crate）保留原样
        assert_eq!(normalize_source("hyper_util::client"), "hyper_util");
        assert_eq!(normalize_source(""), "");
    }

    /// 动态 filter 白名单：python_worker 必须与项目 target 同级，否则其 INFO
    /// 及以下落到默认 WARN，三路（控制台/文件/WS）同时丢失。
    /// 以真实事件发射验证 enabled 判定（Targets 无静态查询接口）。
    #[test]
    fn test_build_targets_whitelist() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::filter::LevelFilter;
        use tracing_subscriber::layer::Layer as _;

        // 记录穿透 filter 的事件 target:level
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        struct Collect(Arc<Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Collect {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0.lock().unwrap().push(format!(
                    "{}:{}",
                    event.metadata().target(),
                    event.metadata().level()
                ));
            }
        }

        let layer = Collect(seen.clone()).with_filter(build_targets(LevelFilter::INFO));
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            tracing::info!(target: "campus_auth::engine", "a");
            tracing::info!(target: "frontend", "b");
            tracing::info!(target: "python_worker", "c");
            // 第三方库默认 WARN：INFO 被丢弃，WARN 放行
            tracing::info!(target: "hyper", "d");
            tracing::warn!(target: "hyper", "e");
        });

        let seen = seen.lock().unwrap();
        assert!(seen.contains(&"campus_auth::engine:INFO".to_string()));
        assert!(seen.contains(&"frontend:INFO".to_string()));
        assert!(seen.contains(&"python_worker:INFO".to_string()));
        assert!(!seen.contains(&"hyper:INFO".to_string()));
        assert!(seen.contains(&"hyper:WARN".to_string()));
    }

    /// 级别解析：大小写不敏感，无效回退 INFO（配置笔误不静默关闭日志）
    #[test]
    fn test_parse_level_branches() {
        use tracing_subscriber::filter::LevelFilter;
        assert_eq!(parse_level("TRACE"), LevelFilter::TRACE);
        assert_eq!(parse_level("debug"), LevelFilter::DEBUG);
        assert_eq!(parse_level("WARNING"), LevelFilter::WARN);
        assert_eq!(parse_level("ERROR"), LevelFilter::ERROR);
        assert_eq!(parse_level("INFO"), LevelFilter::INFO);
        assert_eq!(parse_level("verbose"), LevelFilter::INFO);
        assert_eq!(parse_level(""), LevelFilter::INFO);
    }

    /// 日志配置提取：缺失/非法回退 INFO + 7 天 + 写文件（启动期单次解析语义）
    #[test]
    fn test_logging_config_from_value_fallbacks() {
        use serde_json::json;
        use tracing_subscriber::filter::LevelFilter;
        let (level, retention, file_enabled) = logging_config_from_value(&json!({}));
        assert_eq!(level, LevelFilter::INFO);
        assert_eq!(retention, 7);
        assert!(file_enabled);

        let (level, retention, file_enabled) = logging_config_from_value(&json!({
            "global": {"logging": {"level": "DEBUG", "retention_days": 30, "file_enabled": false}}
        }));
        assert_eq!(level, LevelFilter::DEBUG);
        assert_eq!(retention, 30);
        assert!(!file_enabled);

        let (level, _, _) = logging_config_from_value(&json!({
            "global": {"logging": {"level": "nope"}}
        }));
        assert_eq!(level, LevelFilter::INFO);
    }

    /// COR-9：总配额兜底从最旧的轮转文件删起，活跃 app.log 不动
    #[test]
    fn test_log_quota_reclaims_oldest_rotated_files() {
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("app.log"), vec![b'x'; 100]).unwrap();
        // 三个轮转文件，mtime 从旧到新
        let rotated = [
            ("app.log.2026-01-01", 30_000u64),
            ("app.log.2026-01-02", 20_000),
            ("app.log.2026-01-03", 10_000),
        ];
        for (name, age_secs) in rotated {
            let p = dir.join(name);
            std::fs::write(&p, vec![b'a'; 100]).unwrap();
            // std File::set_modified 设置 mtime，供「最旧优先」排序判定
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
                .unwrap();
        }

        // 配额 250B：总量 400B → 删最旧两个（200B）→ 剩 200B ≤ 250B 停止
        cleanup_old_logs(dir, 365, 250);

        assert!(!dir.join("app.log.2026-01-01").exists(), "最旧的应先删");
        assert!(!dir.join("app.log.2026-01-02").exists(), "次旧的应删除");
        assert!(dir.join("app.log.2026-01-03").exists(), "最新的应保留");
        assert!(dir.join("app.log").exists(), "活跃文件不应被删除");

        // 保留天数优先路径不受影响：retention=1 + 巨大配额 → 只按 mtime 删
        let p = dir.join("app.log.2026-02-01");
        std::fs::write(&p, b"y").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(2 * 86_400))
            .unwrap();
        cleanup_old_logs(dir, 1, u64::MAX);
        assert!(!p.exists(), "超期文件按天数删除");
        assert!(dir.join("app.log.2026-01-03").exists(), "未过期文件不动");
    }
}
