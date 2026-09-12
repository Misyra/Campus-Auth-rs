//! 定时任务数据模型与持久化模块。
//!
//! 定义 `ScheduledTask` 数据模型、cron 5→7 字段转换常量、JSON 原子写入，
//! 以及执行历史的追加与容量裁剪逻辑。磁盘路径约定为 `tasks/scheduled/{id}.json`，
//! 执行历史位于 `tasks/scheduled/history/{id}.json`。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::scheduler::SchedulerError;

// 运行时目录布局单一事实源（见 `utils::paths`）：`HISTORY_DIR_NAME` re-export 保持调用路径稳定；
// `SCHEDULED_DIR_NAME` 已由调用方直引 `utils::paths`，此处不再转出口以避免未使用告警。
pub(crate) use crate::utils::paths::HISTORY_DIR_NAME;
/// 每个任务的历史记录上限（与 Python 版 `MAX_HISTORY_SIZE` 一致）。
pub(crate) const MAX_HISTORY_RECORDS: usize = 50;
/// 任务变更 mpsc channel 容量。
pub(crate) const CHANGE_CHANNEL_CAPACITY: usize = 16;
/// 定时任务默认超时秒数（浏览器/脚本任务）。
pub(crate) const DEFAULT_SCHEDULED_TIMEOUT: u64 = 300;
/// 到期定时任务的最大并发执行数。
///
/// 避免同一调度周期内大量到期任务无上限 spawn 压垮系统（历史遗留 F10）。
/// 超出上限的任务在信号量上排队，按序执行。
pub(crate) const MAX_CONCURRENT_SCHEDULED_TASKS: usize = 4;
/// 5→7 字段转换：前缀秒字段。
pub(crate) const CRON_PARSE_PREFIX: &str = "0 ";
/// 5→7 字段转换：后缀年字段。
pub(crate) const CRON_PARSE_SUFFIX: &str = " *";
/// 启动触发默认延迟秒数（等网络/校园网登录稳定后再执行）。
pub(crate) const DEFAULT_STARTUP_DELAY_SECS: u64 = 30;
/// 启动触发延迟秒数上限。
pub(crate) const MAX_STARTUP_DELAY_SECS: u64 = 86_400;
/// 启动触发失败重试默认次数。
pub(crate) const DEFAULT_STARTUP_MAX_RETRIES: u32 = 2;
/// 启动触发失败重试次数上限。
pub(crate) const MAX_STARTUP_RETRIES: u32 = 10;
/// 启动触发重试间隔秒数（固定值，不作为配置暴露）。
pub(crate) const STARTUP_RETRY_INTERVAL_SECS: u64 = 60;
/// 启动触发每日成功次数缺省上限。
pub(crate) const DEFAULT_STARTUP_MAX_RUNS_PER_DAY: u32 = 1;
/// 启动触发每日成功次数上限的钳制上限。
pub(crate) const MAX_STARTUP_RUNS_PER_DAY: u32 = 99;

/// 定时任务数据模型（对应 `tasks/scheduled/{id}.json`）。
///
/// `id` 由文件名推导，不参与 JSON 序列化。
///
/// 任务类型（浏览器/脚本）**不在此冗余存储**：由 `target_id` 关联的目标任务
/// 通过 [`crate::tasks::TaskKind`] 权威推导，避免与任务定义出现双份类型枚举。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    /// 任务 ID（从文件名 stem 推导，不参与 JSON）。
    #[serde(skip)]
    pub id: String,
    /// 显示名称。
    #[serde(default = "default_name")]
    pub name: String,
    /// 任务描述。
    #[serde(default)]
    pub description: String,
    /// cron 表达式（用户侧 5 字段，存储亦是 5 字段）。
    pub cron: String,
    /// 关联目标任务 ID（浏览器/脚本任务）。
    pub target_id: String,
    /// 预留字段：未来如需「定时登录」可指定凭据 Profile。
    /// 当前定时任务统一走通用执行（打卡），不注入账号密码，故此字段暂不生效。
    #[serde(default)]
    pub profile_id: Option<String>,
    /// 执行超时秒数（None = 使用全局默认值）。
    #[serde(default)]
    pub timeout: Option<u64>,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 上次执行时间（ISO 8601 字符串，持久化恢复）。
    #[serde(default)]
    pub last_run: Option<String>,
    /// 上次执行结果（持久化恢复）。
    #[serde(default)]
    pub last_result: Option<String>,
    /// 触发方式（缺省 cron，兼容存量任务文件）。
    #[serde(default)]
    pub trigger: TaskTrigger,
    /// 启动触发：每日成功执行次数上限（None = [`DEFAULT_STARTUP_MAX_RUNS_PER_DAY`]）。
    #[serde(default)]
    pub max_runs_per_day: Option<u32>,
    /// 启动触发：单轮失败后最大重试次数（None = [`DEFAULT_STARTUP_MAX_RETRIES`]）。
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// 启动触发：延迟执行秒数（None = [`DEFAULT_STARTUP_DELAY_SECS`]）。
    #[serde(default)]
    pub startup_delay_secs: Option<u64>,
    /// 启动触发：当日成功次数簿记（跨重启判断"今天是否已成功过"）。
    #[serde(default)]
    pub startup_success: Option<DailySuccessCount>,
}

fn default_name() -> String {
    "未命名定时任务".to_string()
}

fn default_true() -> bool {
    true
}

/// 定时任务触发方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskTrigger {
    /// cron 定时触发（默认，兼容存量任务文件）。
    #[default]
    Cron,
    /// 应用启动后触发（受每日成功次数上限约束，支持失败重试）。
    Startup,
}

/// 启动触发的当日成功计数簿记（随任务文件持久化，跨重启去重）。
///
/// 仅成功执行计入（用户口径：失败不算执行次数）；窗口键为本地日期，
/// 日期不匹配即视为 0，跨天自动归零。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DailySuccessCount {
    /// 本地日期（ISO 格式 YYYY-MM-DD）。
    pub date: String,
    /// 当日成功执行次数。
    pub count: u32,
}

/// 成功执行后的下一笔当日计数（窗口键不匹配时重置为 1）。
pub(crate) fn next_success_count(
    existing: Option<&DailySuccessCount>,
    today: &str,
) -> DailySuccessCount {
    let count = match existing {
        Some(c) if c.date == today => c.count + 1,
        _ => 1,
    };
    DailySuccessCount {
        date: today.to_string(),
        count,
    }
}

impl ScheduledTask {
    /// 构造一个最小可用任务（含默认值）。
    #[cfg(test)]
    pub(crate) fn new(id: String, cron: String, target_id: String) -> Self {
        Self {
            id,
            cron,
            target_id,
            name: default_name(),
            description: String::new(),
            profile_id: None,
            timeout: None,
            enabled: true,
            last_run: None,
            last_result: None,
            trigger: TaskTrigger::Cron,
            max_runs_per_day: None,
            max_retries: None,
            startup_delay_secs: None,
            startup_success: None,
        }
    }

    /// 本地今日日期（ISO YYYY-MM-DD），作为启动成功计数的窗口键。
    pub(crate) fn local_today() -> String {
        chrono::Local::now().date_naive().to_string()
    }

    /// 启动触发的当日成功次数（簿记日期与 `today` 不匹配视为 0）。
    pub(crate) fn startup_success_today(&self, today: &str) -> u32 {
        match &self.startup_success {
            Some(c) if c.date == today => c.count,
            _ => 0,
        }
    }

    /// 启动触发的有效每日成功次数上限。
    pub(crate) fn effective_max_runs_per_day(&self) -> u32 {
        self.max_runs_per_day
            .unwrap_or(DEFAULT_STARTUP_MAX_RUNS_PER_DAY)
    }

    /// 启动触发的有效失败重试次数（单轮总尝试 = 重试次数 + 1）。
    pub(crate) fn effective_max_retries(&self) -> u32 {
        self.max_retries.unwrap_or(DEFAULT_STARTUP_MAX_RETRIES)
    }

    /// 启动触发的有效延迟执行秒数。
    pub(crate) fn effective_startup_delay_secs(&self) -> u64 {
        self.startup_delay_secs
            .unwrap_or(DEFAULT_STARTUP_DELAY_SECS)
    }

    /// 启动触发当日成功额度是否已用尽。
    pub(crate) fn startup_cap_reached(&self, today: &str) -> bool {
        self.startup_success_today(today) >= self.effective_max_runs_per_day()
    }

    /// 保存前归一化：按触发方式清理/钳制字段。
    ///
    /// - Cron：清空启动触发专属字段（切回定时执行不残留启动配置与计数）；
    /// - Startup：将上限/重试/延迟钳制到合法区间（缺省值显式落盘）；
    ///   `startup_success` 保留（窗口键跨天自动归零，无需清理）。
    pub(crate) fn normalize_for_save(&mut self) {
        if self.trigger == TaskTrigger::Cron {
            self.max_runs_per_day = None;
            self.max_retries = None;
            self.startup_delay_secs = None;
            self.startup_success = None;
            return;
        }
        self.max_runs_per_day = Some(
            self.effective_max_runs_per_day()
                .clamp(1, MAX_STARTUP_RUNS_PER_DAY),
        );
        self.max_retries = Some(self.effective_max_retries().min(MAX_STARTUP_RETRIES));
        self.startup_delay_secs = Some(
            self.effective_startup_delay_secs()
                .min(MAX_STARTUP_DELAY_SECS),
        );
    }

    /// 从磁盘文件加载任务，并以文件名 stem 作为 `id`。
    pub(crate) fn load_from(path: &Path) -> Result<Self, SchedulerError> {
        let content = std::fs::read_to_string(path).map_err(SchedulerError::IoError)?;
        let mut task: Self = serde_json::from_str(&content).map_err(SchedulerError::JsonError)?;
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            task.id = stem.to_string();
        }
        Ok(task)
    }

    /// 原子写入任务到磁盘。
    pub(crate) fn save_to(path: &Path, task: &Self) -> Result<(), SchedulerError> {
        atomic_write_json(path, task)
    }

    /// 校验任务 id 是否符合命名规则（字母数字、下划线、连字符，且不以 `.` 开头）。
    pub(crate) fn is_valid_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && !id.starts_with('.')
    }
}

/// 原子写入 JSON（委托给 utils::io::atomic_write_json）
pub(crate) fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), SchedulerError> {
    crate::utils::atomic_write_json(path, value).map_err(SchedulerError::IoError)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryRecord {
    timestamp: String,
    status: String,
    message: String,
    duration: f64,
    /// 触发来源（cron/startup/manual；存量记录缺省为空串）。
    #[serde(default)]
    trigger: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct HistoryFile {
    #[serde(default)]
    runs: Vec<HistoryRecord>,
}

/// 追加一条执行历史，超出 `MAX_HISTORY_RECORDS` 时裁剪最旧的记录。
pub(crate) fn append_history(
    history_dir: &Path,
    task_id: &str,
    status: &str,
    message: &str,
    duration: std::time::Duration,
    trigger: &str,
) -> Result<(), SchedulerError> {
    let path = history_dir.join(format!("{}.json", task_id));
    let mut file: HistoryFile = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        HistoryFile::default()
    };
    file.runs.push(HistoryRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        status: status.to_string(),
        message: message.to_string(),
        duration: duration.as_secs_f64(),
        trigger: trigger.to_string(),
    });
    if file.runs.len() > MAX_HISTORY_RECORDS {
        let excess = file.runs.len() - MAX_HISTORY_RECORDS;
        file.runs.drain(0..excess);
    }
    atomic_write_json(&path, &file)
}

/// 返回某任务目录对应的历史目录。
pub(crate) fn history_dir_of(scheduled_dir: &Path) -> PathBuf {
    scheduled_dir.join(HISTORY_DIR_NAME)
}

/// 将磁盘历史 `{ "runs": [{ timestamp, status, message, ... }] }` 映射为前端期望的
/// 扁平数组 `[{ run_at, success, message }]`。`success` 由 `status == "success"` 推导。
///
/// 纯函数：无 I/O，供 [`crate::scheduler::SchedulerService::read_history`]（原
/// web/routes/scheduler.rs 的 job_history handler）复用；字段缺失时按 null/false 容错。
pub(crate) fn map_history_records(raw: &serde_json::Value) -> Vec<serde_json::Value> {
    use serde_json::Value;
    let runs = raw
        .get("runs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    runs.into_iter()
        .map(|record| {
            let run_at = record.get("timestamp").cloned().unwrap_or(Value::Null);
            let success = record
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "success")
                .unwrap_or(false);
            let message = record.get("message").cloned().unwrap_or(Value::Null);
            let trigger = record.get("trigger").cloned().unwrap_or(Value::Null);
            serde_json::json!({
                "run_at": run_at,
                "success": success,
                "message": message,
                "trigger": trigger
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::OrderData;

    #[test]
    fn test_default_task() {
        let t = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        assert_eq!(t.name, "未命名定时任务");
        assert!(t.enabled);
        assert_eq!(t.profile_id, None);
        assert_eq!(t.timeout, None);
    }

    #[test]
    fn test_is_valid_id() {
        assert!(ScheduledTask::is_valid_id("abc-123"));
        assert!(!ScheduledTask::is_valid_id(".hidden"));
        assert!(!ScheduledTask::is_valid_id(""));
    }

    #[test]
    fn test_serde_roundtrip() {
        let t = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        let json = serde_json::to_string(&t).unwrap();
        // id 被 skip，不应出现在 JSON 中
        assert!(!json.contains("\"id\""));
        let back: ScheduledTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cron, t.cron);
        assert_eq!(back.target_id, t.target_id);
    }

    // ============ is_valid_id 扩展测试 ============

    #[test]
    fn test_is_valid_id_alphanumeric() {
        // 纯字母数字
        assert!(ScheduledTask::is_valid_id("abc123"));
    }

    #[test]
    fn test_is_valid_id_underscore_hyphen() {
        // 含下划线和连字符
        assert!(ScheduledTask::is_valid_id("my_task-01"));
    }

    #[test]
    fn test_is_valid_id_starts_with_dot_rejected() {
        // 以点开头的 ID 无效
        assert!(!ScheduledTask::is_valid_id(".hidden_task"));
    }

    #[test]
    fn test_is_valid_id_special_chars_rejected() {
        // 特殊字符无效
        assert!(!ScheduledTask::is_valid_id("task/id"));
        assert!(!ScheduledTask::is_valid_id("task id"));
        assert!(!ScheduledTask::is_valid_id("task@home"));
        assert!(!ScheduledTask::is_valid_id("task.dot"));
    }

    #[test]
    fn test_is_valid_id_chinese_rejected() {
        assert!(!ScheduledTask::is_valid_id("定时任务"));
    }

    // ============ atomic_write_json 往返 ============

    #[test]
    fn test_atomic_write_json_roundtrip() {
        // 测试原子写入 JSON 后可正确读回
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.json");

        let task = ScheduledTask::new(
            "test".to_string(),
            "0 9 * * 1-5".to_string(),
            "browser_default".to_string(),
        );
        atomic_write_json(&path, &task).unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: ScheduledTask = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.cron, "0 9 * * 1-5");
        assert_eq!(loaded.target_id, "browser_default");
        assert!(loaded.enabled);
    }

    #[test]
    fn test_atomic_write_json_creates_parent_dir() {
        // 即使父目录不存在也应成功
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("subdir").join("nested.json");

        let data = OrderData::default();
        // 注意：atomic_write_json 委托给 utils::atomic_write_json，
        // 需要父目录存在。这里我们先创建。
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::utils::atomic_write_json(&path, &data).unwrap();
        assert!(path.exists());
    }

    // ============ ScheduledTask load_from / save_to ============

    #[test]
    fn test_scheduled_task_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("task1.json");

        let mut task = ScheduledTask::new(
            "task1".to_string(),
            "*/5 * * * *".to_string(),
            "shell1".to_string(),
        );
        task.name = "每5分钟".to_string();
        task.timeout = Some(120);

        ScheduledTask::save_to(&path, &task).unwrap();
        let loaded = ScheduledTask::load_from(&path).unwrap();

        // id 从文件名推导
        assert_eq!(loaded.id, "task1");
        assert_eq!(loaded.name, "每5分钟");
        assert_eq!(loaded.cron, "*/5 * * * *");
        assert_eq!(loaded.timeout, Some(120));
    }

    #[test]
    fn test_scheduled_task_default_name() {
        // name 缺失时应使用默认值
        let json = r#"{"cron": "0 * * * *", "target_id": "t1"}"#;
        let task: ScheduledTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.name, "未命名定时任务");
    }

    #[test]
    fn test_scheduled_task_default_enabled() {
        // enabled 缺失时默认为 true
        let json = r#"{"cron": "0 * * * *", "target_id": "t1"}"#;
        let task: ScheduledTask = serde_json::from_str(json).unwrap();
        assert!(task.enabled);
    }

    // ============ append_history 测试 ============

    #[test]
    fn test_append_history_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let history_dir = tmp.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        append_history(
            &history_dir,
            "task1",
            "success",
            "执行成功",
            std::time::Duration::from_secs(5),
            "cron",
        )
        .unwrap();

        let path = history_dir.join("task1.json");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let file: HistoryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(file.runs.len(), 1);
        assert_eq!(file.runs[0].status, "success");
        assert_eq!(file.runs[0].message, "执行成功");
    }

    #[test]
    fn test_append_history_trims_to_max() {
        let tmp = tempfile::tempdir().unwrap();
        let history_dir = tmp.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        // 写入 MAX_HISTORY_RECORDS + 5 条记录
        let total = MAX_HISTORY_RECORDS + 5;
        for i in 0..total {
            append_history(
                &history_dir,
                "task1",
                "success",
                &format!("run {i}"),
                std::time::Duration::from_secs(1),
                "cron",
            )
            .unwrap();
        }

        let path = history_dir.join("task1.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let file: HistoryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(file.runs.len(), MAX_HISTORY_RECORDS);
        // 最旧的记录应被裁剪，保留的最后一条应是 "run {total-1}"
        assert_eq!(
            file.runs.last().unwrap().message,
            format!("run {}", total - 1)
        );
    }

    #[test]
    fn test_append_history_accumulates() {
        // 多次追加应累加记录
        let tmp = tempfile::tempdir().unwrap();
        let history_dir = tmp.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        for i in 0..3 {
            append_history(
                &history_dir,
                "task2",
                if i % 2 == 0 { "success" } else { "failure" },
                "msg",
                std::time::Duration::from_secs(i as u64),
                "manual",
            )
            .unwrap();
        }

        let path = history_dir.join("task2.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let file: HistoryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(file.runs.len(), 3);
    }

    // ============ history_dir_of 测试 ============

    #[test]
    fn test_history_dir_of() {
        let scheduled = PathBuf::from("/tasks/scheduled");
        let history = history_dir_of(&scheduled);
        assert_eq!(history, PathBuf::from("/tasks/scheduled/history"));
    }

    // ============ map_history_records 测试（自 web/routes/scheduler.rs 随迁） ============

    #[test]
    fn map_history_lossy_mapping() {
        let raw = serde_json::json!({
            "runs": [
                { "timestamp": "2026-08-14T01:00:00Z", "status": "success", "message": "完成", "duration": 1.2 },
                { "timestamp": "2026-08-14T02:00:00Z", "status": "error", "message": "失败" },
                { "status": "success" },
                { "message": "无状态" },
            ]
        });
        let mapped = map_history_records(&raw);
        assert_eq!(mapped.len(), 4);
        // success 由 status == "success" 推导
        assert_eq!(mapped[0]["success"], serde_json::json!(true));
        assert_eq!(mapped[1]["success"], serde_json::json!(false));
        // 无 status 时 success 为 false；无 timestamp 时为 null
        assert_eq!(mapped[2]["success"], serde_json::json!(true));
        assert_eq!(mapped[2]["run_at"], serde_json::Value::Null);
        assert_eq!(mapped[3]["success"], serde_json::json!(false));
        assert_eq!(mapped[3]["message"], serde_json::json!("无状态"));
    }

    #[test]
    fn map_history_missing_or_empty_runs() {
        // 无 runs 字段 → 空数组
        assert_eq!(
            map_history_records(&serde_json::json!({})),
            Vec::<serde_json::Value>::new()
        );
        // runs 为空数组 → 空数组
        assert_eq!(
            map_history_records(&serde_json::json!({"runs": []})),
            Vec::<serde_json::Value>::new()
        );
        // runs 非数组 → 空数组
        assert_eq!(
            map_history_records(&serde_json::json!({"runs": "x"})),
            Vec::<serde_json::Value>::new()
        );
    }

    // ============ 常量测试 ============

    #[test]
    fn test_constants() {
        assert_eq!(MAX_HISTORY_RECORDS, 50);
        assert_eq!(CRON_PARSE_PREFIX, "0 ");
        assert_eq!(CRON_PARSE_SUFFIX, " *");
        assert_eq!(DEFAULT_SCHEDULED_TIMEOUT, 300);
    }

    // ============ 启动触发：模型序列化与缺省兼容 ============

    #[test]
    fn test_startup_fields_serde_roundtrip() {
        let mut t = ScheduledTask::new("t1".to_string(), String::new(), "x".to_string());
        t.trigger = TaskTrigger::Startup;
        t.max_runs_per_day = Some(3);
        t.max_retries = Some(1);
        t.startup_delay_secs = Some(45);
        t.startup_success = Some(DailySuccessCount {
            date: "2026-09-12".to_string(),
            count: 2,
        });
        let json = serde_json::to_string(&t).unwrap();
        let back: ScheduledTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.trigger, TaskTrigger::Startup);
        assert_eq!(back.max_runs_per_day, Some(3));
        assert_eq!(back.max_retries, Some(1));
        assert_eq!(back.startup_delay_secs, Some(45));
        assert_eq!(
            back.startup_success,
            Some(DailySuccessCount {
                date: "2026-09-12".to_string(),
                count: 2
            })
        );
    }

    #[test]
    fn test_old_task_file_without_trigger_loads_as_cron() {
        // 存量任务文件没有 trigger 字段：反序列化为 Cron，启动字段为 None
        let task: ScheduledTask =
            serde_json::from_str(r#"{"cron": "0 8 * * *", "target_id": "t1"}"#).unwrap();
        assert_eq!(task.trigger, TaskTrigger::Cron);
        assert_eq!(task.max_runs_per_day, None);
        assert_eq!(task.max_retries, None);
        assert_eq!(task.startup_delay_secs, None);
        assert_eq!(task.startup_success, None);
        assert!(!task.startup_cap_reached("2026-09-12"));
    }

    // ============ 启动触发：成功计数窗口 ============

    #[test]
    fn test_next_success_count_same_date_increments() {
        let existing = DailySuccessCount {
            date: "2026-09-12".to_string(),
            count: 2,
        };
        let next = next_success_count(Some(&existing), "2026-09-12");
        assert_eq!(next.count, 3);
        assert_eq!(next.date, "2026-09-12");
    }

    #[test]
    fn test_next_success_count_date_mismatch_resets() {
        // 跨天：旧簿记日期与今日不一致时重置为 1
        let existing = DailySuccessCount {
            date: "2026-09-11".to_string(),
            count: 5,
        };
        let next = next_success_count(Some(&existing), "2026-09-12");
        assert_eq!(next.count, 1);
        // 无簿记同样从 1 起算
        let first = next_success_count(None, "2026-09-12");
        assert_eq!(first.count, 1);
    }

    #[test]
    fn test_startup_cap_reached() {
        let mut t = ScheduledTask::new("t1".to_string(), String::new(), "x".to_string());
        t.trigger = TaskTrigger::Startup;
        t.startup_success = Some(DailySuccessCount {
            date: "2026-09-12".to_string(),
            count: 1,
        });
        // 缺省上限 1：当日已成功 1 次 → 额度用尽
        assert!(t.startup_cap_reached("2026-09-12"));
        // 昨日的成功不占用今日额度
        assert!(!t.startup_cap_reached("2026-09-13"));
        // 上限提到 2 后未用尽
        t.max_runs_per_day = Some(2);
        assert!(!t.startup_cap_reached("2026-09-12"));
    }

    // ============ 启动触发：保存归一化 ============

    #[test]
    fn test_normalize_for_save_cron_clears_startup_fields() {
        let mut t = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        t.trigger = TaskTrigger::Cron;
        t.max_runs_per_day = Some(5);
        t.max_retries = Some(3);
        t.startup_delay_secs = Some(60);
        t.startup_success = Some(DailySuccessCount {
            date: "2026-09-12".to_string(),
            count: 1,
        });
        t.normalize_for_save();
        assert_eq!(t.max_runs_per_day, None);
        assert_eq!(t.max_retries, None);
        assert_eq!(t.startup_delay_secs, None);
        assert_eq!(t.startup_success, None);
    }

    #[test]
    fn test_normalize_for_save_startup_clamps() {
        let mut t = ScheduledTask::new("t1".to_string(), String::new(), "x".to_string());
        t.trigger = TaskTrigger::Startup;
        t.max_runs_per_day = Some(0);
        t.max_retries = Some(999);
        t.startup_delay_secs = Some(999_999);
        t.normalize_for_save();
        // 上下限钳制
        assert_eq!(t.max_runs_per_day, Some(1));
        assert_eq!(t.max_retries, Some(MAX_STARTUP_RETRIES));
        assert_eq!(t.startup_delay_secs, Some(MAX_STARTUP_DELAY_SECS));

        // None 补默认值
        let mut t2 = ScheduledTask::new("t2".to_string(), String::new(), "x".to_string());
        t2.trigger = TaskTrigger::Startup;
        t2.normalize_for_save();
        assert_eq!(t2.max_runs_per_day, Some(DEFAULT_STARTUP_MAX_RUNS_PER_DAY));
        assert_eq!(t2.max_retries, Some(DEFAULT_STARTUP_MAX_RETRIES));
        assert_eq!(t2.startup_delay_secs, Some(DEFAULT_STARTUP_DELAY_SECS));
    }

    // ============ 执行历史：触发来源字段 ============

    #[test]
    fn test_history_trigger_roundtrip_and_mapping() {
        let tmp = tempfile::tempdir().unwrap();
        let history_dir = tmp.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();
        append_history(
            &history_dir,
            "task1",
            "success",
            "完成",
            std::time::Duration::from_secs(1),
            "startup",
        )
        .unwrap();
        let content = std::fs::read_to_string(history_dir.join("task1.json")).unwrap();
        let file: HistoryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(file.runs[0].trigger, "startup");

        let mapped = map_history_records(&serde_json::from_str(&content).unwrap());
        assert_eq!(mapped[0]["trigger"], serde_json::json!("startup"));

        // 存量记录无 trigger 字段 → 映射为 null（前端容错）
        let legacy = serde_json::json!({
            "runs": [{ "timestamp": "2026-08-14T01:00:00Z", "status": "success", "message": "旧", "duration": 1.0 }]
        });
        let mapped_legacy = map_history_records(&legacy);
        assert_eq!(mapped_legacy[0]["trigger"], serde_json::Value::Null);
    }
}
