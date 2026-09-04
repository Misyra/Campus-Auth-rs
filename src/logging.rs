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

/// 将 tracing target 归一化为短模块名，供前端来源过滤与展示
///
/// - `campus_auth::scheduler::cron_loop` → `scheduler`
/// - `campus_auth::launcher` → `launcher`
/// - `campus_auth`（crate 根） → `app`
/// - 外部 crate（如 `hyper_util::client`）→ 取首段 `hyper_util`
///
/// 归一化后前后端来源过滤（精确匹配短名）与徽章展示才能一致工作。
pub fn normalize_source(target: &str) -> String {
    let t = target.trim();
    if t.is_empty() {
        return String::new();
    }
    // 去掉 crate 前缀 `campus_auth::`
    let rest = t.strip_prefix("campus_auth::").unwrap_or(t);
    let first = rest.split("::").next().unwrap_or("").trim();
    if first.is_empty() || first == "campus_auth" {
        // crate 根模块（target 恰好为 `campus_auth`）
        return "app".to_string();
    }
    first.to_ascii_lowercase()
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

impl SharedTargets {
    /// 构造默认规则：第三方库 WARN，本项目 target（campus_auth/frontend）指定级别
    fn build(lf: tracing_subscriber::filter::LevelFilter) -> Self {
        Self::new(
            tracing_subscriber::filter::Targets::new()
                .with_default(tracing_subscriber::filter::LevelFilter::WARN)
                .with_target("campus_auth", lf)
                .with_target("frontend", lf),
        )
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

/// 从已解析的 settings.json `Value` 一次性提取日志配置（级别 + 保留天数）
///
/// 由 `launcher` 在单次文件读取后调用，避免启动期对同一文件三次读解析
///（启动字段 / 日志级别 / 保留天数各读一次的历史包袱）。缺失/非法时回退 INFO / 7 天。
pub(crate) fn logging_config_from_value(
    value: &serde_json::Value,
) -> (tracing_subscriber::filter::LevelFilter, u32) {
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
    (level, retention)
}

/// 热更新全局日志级别（由 `set_log_level` 调用）
///
/// 第三方库保持 WARN，本项目 target（campus_auth/frontend）设为指定级别。无效级别回退 INFO。
pub fn reload_log_level(level: &str) {
    let lf = parse_level(level);
    let Some(shared) = LOG_TARGETS.get() else {
        tracing::warn!(level = %level, "日志 filter 未初始化，忽略级别切换");
        return;
    };
    shared.replace(
        tracing_subscriber::filter::Targets::new()
            .with_default(tracing_subscriber::filter::LevelFilter::WARN)
            .with_target("campus_auth", lf)
            .with_target("frontend", lf),
    );
    tracing::info!(level = %lf, "日志级别已热更新");
}

// ============================================================
// 文件保留清理
// ============================================================

/// 删除 logs/ 目录下超过保留天数的旧日志文件
///
/// 仅删除修改时间早于 cutoff 的 `.log` 文件，跳过当前正在写入的 `app.log`
/// （`tracing_appender::rolling::daily` 生成 `app.log.YYYY-MM-DD` 轮转文件）。
fn cleanup_old_logs(logs_dir: &Path, retention_days: u32) {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        tracing::warn!("读取日志目录失败，跳过过期日志清理");
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
                if modified < cutoff {
                    match std::fs::remove_file(&path) {
                        Ok(()) => removed += 1,
                        Err(e) => tracing::debug!("删除过期日志文件失败 {}: {e}", path.display()),
                    }
                }
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            "清理过期日志文件 {} 个（保留 {} 天）",
            removed,
            retention_days
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
        let timestamp = chrono::Local::now().to_rfc3339();
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

/// 初始化日志系统：控制台层 + 文件层（按日期轮转）+ 广播层（WebSocket 推送）
///
/// 全局 subscriber 只能 init 一次，所有层在此统一注册。
/// 日志级别与保留天数由 `launcher` 单次解析 settings.json 后传入，
/// 本函数不再重复读文件（历史三读：启动字段 / 级别 / 保留天数各一次）。
pub fn init_logging(
    base_path: &Path,
    log_tx: tokio::sync::broadcast::Sender<LogEntry>,
    log_level: tracing_subscriber::filter::LevelFilter,
    retention_days: u32,
) -> WorkerGuard {
    let logs_dir = crate::utils::paths::logs_dir(base_path);
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        tracing::warn!("创建日志目录失败: {e}");
    }

    // 会话起始时间：与 LocalTimer 同格式，供 /api/logs 过滤历史运行日志
    let _ = SESSION_STARTED_AT.set(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());

    // 启动时清理过期日志：按传入的 logging.retention_days 保留，
    // 删除超过保留天数的旧轮转文件，避免日志无限累积（对齐原项目 loguru retention）。
    cleanup_old_logs(&logs_dir, retention_days);

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

    // 文件层：JSON 格式按日轮转
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "app.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_timer(local_timer)
        .json()
        .with_filter(shared.clone());

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

    guard
}
