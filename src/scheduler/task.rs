//! 定时任务数据模型与持久化模块。
//!
//! 定义 `ScheduledTask` 数据模型、cron 5→7 字段转换常量、JSON 原子写入，
//! 以及执行历史的追加与容量裁剪逻辑。磁盘路径约定为 `tasks/scheduled/{id}.json`，
//! 执行历史位于 `tasks/scheduled/history/{id}.json`。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::scheduler::SchedulerError;

/// 定时任务目录名（位于 `tasks/` 下）。
pub(crate) const SCHEDULED_DIR_NAME: &str = "scheduled";
/// 执行历史子目录名。
pub(crate) const HISTORY_DIR_NAME: &str = "history";
/// 每个任务的历史记录上限（与 Python 版 `MAX_HISTORY_SIZE` 一致）。
pub(crate) const MAX_HISTORY_RECORDS: usize = 50;
/// 任务变更 mpsc channel 容量。
pub(crate) const CHANGE_CHANNEL_CAPACITY: usize = 16;
/// 定时任务默认超时秒数（浏览器/脚本/Shell 任务）。
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

/// 定时任务数据模型（对应 `tasks/scheduled/{id}.json`）。
///
/// `id` 由文件名推导，不参与 JSON 序列化。
///
/// 任务类型（浏览器/脚本/Shell）**不在此冗余存储**：由 `target_id` 关联的目标任务
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
    /// 关联目标任务 ID（浏览器/脚本/Shell 任务）。
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
}

fn default_name() -> String {
    "未命名定时任务".to_string()
}

fn default_true() -> bool {
    true
}

impl ScheduledTask {
    /// 构造一个最小可用任务（含默认值）。
    #[allow(dead_code)]
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
        }
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
            serde_json::json!({
                "run_at": run_at,
                "success": success,
                "message": message
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
}
