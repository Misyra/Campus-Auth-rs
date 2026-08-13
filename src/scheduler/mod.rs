//! 调度器服务模块。
//!
//! 负责 `SchedulerService` 的构造、定时任务的 CRUD 与持久化、配置热重载
//! 通知，以及调度主循环（`cron_loop`）的生命周期管理（启动/停止）。
//!
//! 构造函数 `new` 注入 `ConfigService` / `TaskManager` / `LoginOrchestrator` /
//! `TaskExecutor` / `StatusManager`，并通过 `ConfigReloadSignal` 流感知配置变更；
//! `start` 启动后台调度 task 并返回可停止的 `ServiceHandle`。

pub mod task;
pub mod cron_loop;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::config::runtime::ConfigReloadSignal;
use crate::config::ConfigService;
use crate::status::StatusManager;
use crate::tasks::TaskManager;

pub use self::cron_loop::execute_scheduled_task;
use self::task::{
    append_history, history_dir_of, ScheduledTask, SCHEDULED_DIR_NAME, HISTORY_DIR_NAME,
    CHANGE_CHANNEL_CAPACITY, MAX_CONCURRENT_SCHEDULED_TASKS,
};

/// 调度器错误类型。
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// cron 表达式解析失败。
    #[error("无效的 cron 表达式 '{0}': {1}")]
    InvalidCronExpr(String, String),
    /// 操作的定时任务不存在。
    #[error("定时任务不存在: {0}")]
    TaskNotFound(String),
    /// 任务 ID 不符合命名规则。
    #[error("无效的任务 ID: {0}")]
    InvalidTaskId(String),
    /// 关联的目标任务不存在。
    #[error("关联目标任务不存在: {0}")]
    TargetNotFound(String),
    /// 文件读写失败。
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),
    /// 序列化/反序列化失败。
    #[error("JSON 错误: {0}")]
    JsonError(#[from] serde_json::Error),
    /// LoginOrchestrator 因抢占/去重拒绝提交。
    #[error("任务提交被拒绝: {0}")]
    SubmitRejected(String),
    /// TaskExecutor 返回错误。
    #[error("执行器错误: {0}")]
    ExecutorError(String),
}

/// 内部任务变更通知（统一触发全量重算）。
pub(crate) enum TaskChange {
    /// 统一重载信号：TaskManager CRUD、ConfigService reload 都走此变体。
    Reload,
}

/// 调度器内部可变状态。
pub(crate) struct SchedulerState {
    /// 内存缓存（从磁盘加载）。
    tasks: Vec<ScheduledTask>,
    /// 调度器是否运行中。
    running: bool,
    /// 全局最近触发时间（供 API 查询）。
    next_fire_at: Option<std::time::SystemTime>,
}

/// 调度器服务主结构。
pub struct SchedulerService {
    /// `tasks/scheduled/` 目录。
    scheduled_dir: PathBuf,
    /// 加载 browser/script/shell 任务配置。
    task_manager: Arc<TaskManager>,
    /// 浏览器任务提交。
    orchestrator: Arc<crate::login::LoginOrchestrator>,
    /// 脚本/Shell 任务执行。
    executor: Arc<crate::tasks::TaskExecutor>,
    /// 状态广播。
    status_manager: Arc<StatusManager>,
    /// 外部发送变更通知的 sender。
    task_change_tx: mpsc::Sender<TaskChange>,
    /// 主循环持有的变更接收端（start 时取出）。
    task_change_rx: tokio::sync::Mutex<Option<mpsc::Receiver<TaskChange>>>,
    /// 配置重载信号接收端（start 时取出）。
    reload_rx: tokio::sync::Mutex<Option<mpsc::Receiver<ConfigReloadSignal>>>,
    /// 到期任务并发限制信号量（历史遗留 F10）。
    concurrency: Arc<tokio::sync::Semaphore>,
    /// 内部状态。
    state: std::sync::Mutex<SchedulerState>,
}

/// 调度器运行时状态（供 API 查询）。
#[derive(Debug, Clone)]
pub struct SchedulerStatus {
    /// 是否运行中。
    pub running: bool,
    /// 任务数量。
    pub task_count: usize,
    /// 下次触发时间（ISO 8601 字符串）。
    pub next_fire_at: Option<String>,
}

pub use crate::ServiceHandle;

impl SchedulerService {
    /// 构造调度器服务。
    ///
    /// 从 `ConfigService` 推导 `tasks/scheduled/` 目录并确保其存在；
    /// 创建容量 `CHANGE_CHANNEL_CAPACITY` 的内部变更 channel。
    pub fn new(
        config: Arc<ConfigService>,
        tasks: Arc<TaskManager>,
        orchestrator: Arc<crate::login::LoginOrchestrator>,
        executor: Arc<crate::tasks::TaskExecutor>,
        status: Arc<StatusManager>,
        reload_rx: mpsc::Receiver<ConfigReloadSignal>,
    ) -> Result<Self, SchedulerError> {
        let base_path = config.base_path();
        let scheduled_dir = base_path.join("tasks").join(SCHEDULED_DIR_NAME);
        std::fs::create_dir_all(&scheduled_dir).map_err(SchedulerError::IoError)?;
        let history_dir = scheduled_dir.join(HISTORY_DIR_NAME);
        std::fs::create_dir_all(&history_dir).map_err(SchedulerError::IoError)?;

        let (task_change_tx, task_change_rx) = mpsc::channel(CHANGE_CHANNEL_CAPACITY);

        Ok(Self {
            scheduled_dir,
            task_manager: tasks,
            orchestrator,
            executor,
            status_manager: status,
            task_change_tx,
            task_change_rx: tokio::sync::Mutex::new(Some(task_change_rx)),
            reload_rx: tokio::sync::Mutex::new(Some(reload_rx)),
            concurrency: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SCHEDULED_TASKS)),
            state: std::sync::Mutex::new(SchedulerState {
                tasks: Vec::new(),
                running: true,
                next_fire_at: None,
            }),
        })
    }

    /// 启动调度循环，返回可停止的服务句柄。
    pub async fn start(self: Arc<Self>) -> ServiceHandle {
        let (stop_tx, stop_rx) = watch::channel(false);
        let task_change_rx = self.task_change_rx.lock().await.take()
            .expect("SchedulerService::start() 只能调用一次");
        let reload_rx = self.reload_rx.lock().await.take()
            .expect("SchedulerService::start() 只能调用一次");
        let svc = self.clone();
        let join_handle = tokio::spawn(async move {
            cron_loop::cron_loop(svc, stop_rx, Some(task_change_rx), Some(reload_rx)).await;
        });
        ServiceHandle {
            stop_tx,
            join_handle,
        }
    }

    /// 返回内存缓存中的任务列表副本。
    pub fn list_tasks(&self) -> Vec<ScheduledTask> {
        self.state
            .lock()
            .map(|s| s.tasks.clone())
            .unwrap_or_default()
    }

    /// 返回目标任务的类型（`browser`/`script`/`shell`），供展示层补充类型标签。
    ///
    /// 类型权威来源为 [`crate::tasks::TaskKind`]（由 target_id 关联的任务推导），
    /// 定时任务存储模型本身不再冗余存类型。
    pub async fn task_type_of(&self, target_id: &str) -> Option<&'static str> {
        match self.task_manager.load_task(target_id).await.ok()? {
            crate::tasks::TaskKind::Browser(_) => Some("browser"),
            crate::tasks::TaskKind::Script(_) => Some("script"),
            crate::tasks::TaskKind::Shell(_) => Some("shell"),
        }
    }

    /// 查询单个任务。
    pub fn get_task(&self, id: &str) -> Option<ScheduledTask> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.tasks.iter().find(|t| t.id == id).cloned())
    }

    /// 保存任务（创建或更新）到磁盘与缓存，并通知主循环重算。
    pub fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError> {
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        // 校验关联目标任务存在
        if !self.task_manager.has_task(&task.target_id) {
            return Err(SchedulerError::TargetNotFound(task.target_id.clone()));
        }

        let path = self.scheduled_dir.join(format!("{}.json", id));
        let mut to_save = task.clone();
        to_save.id = id.to_string();
        ScheduledTask::save_to(&path, &to_save)?;

        self.update_state(|s| {
            if let Some(existing) = s.tasks.iter_mut().find(|t| t.id == id) {
                *existing = to_save.clone();
            } else {
                s.tasks.push(to_save);
            }
        });
        self.notify_change();
        self.publish_status();
        Ok(())
    }

    /// 删除任务文件并更新缓存，通知主循环重算。
    pub fn delete_task(&self, id: &str) -> Result<(), SchedulerError> {
        let path = self.scheduled_dir.join(format!("{}.json", id));
        if !path.exists() {
            return Err(SchedulerError::TaskNotFound(id.to_string()));
        }
        std::fs::remove_file(&path).map_err(SchedulerError::IoError)?;
        self.update_state(|s| s.tasks.retain(|t| t.id != id));
        self.notify_change();
        self.publish_status();
        Ok(())
    }

    /// 启用/禁用任务（复用 save_task 的持久化与通知逻辑）。
    pub fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError> {
        let mut task = self
            .get_task(id)
            .ok_or_else(|| SchedulerError::TaskNotFound(id.to_string()))?;
        task.enabled = enabled;
        self.save_task(id, &task)
    }

    /// 向主循环发送 `TaskChange::Reload` 信号（buffer 满时静默丢弃，重载幂等）。
    pub fn notify_change(&self) {
        let _ = self.task_change_tx.try_send(TaskChange::Reload);
    }

    /// 查询调度器运行状态。
    pub fn status(&self) -> SchedulerStatus {
        match self.state.lock() {
            Ok(s) => {
                let next = s.next_fire_at.map(systemtime_to_iso);
                SchedulerStatus {
                    running: s.running,
                    task_count: s.tasks.len(),
                    next_fire_at: next,
                }
            }
            Err(_) => SchedulerStatus {
                running: false,
                task_count: 0,
                next_fire_at: None,
            },
        }
    }

    /// 更新内部状态（加锁失败则跳过）。
    pub(crate) fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut SchedulerState),
    {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }

    /// 将当前运行状态广播到 StatusManager。
    fn publish_status(&self) {
        let (running, next, count) = match self.state.lock() {
            Ok(s) => (s.running, s.next_fire_at.map(systemtime_to_iso), s.tasks.len()),
            Err(_) => return,
        };
        self.status_manager.merge(crate::status::PartialSnapshot::Scheduler {
            running,
            next_fire_at: next,
            task_count: count,
        });
    }

    /// 任务执行后更新 `last_run` / `last_result`（内存 + 磁盘）。
    pub(crate) fn update_last_run(&self, task_id: &str, status: &str, message: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let result = format!("[{}] {}", status, message);
        let path = self.scheduled_dir.join(format!("{}.json", task_id));
        if let Ok(mut task) = ScheduledTask::load_from(&path) {
            task.last_run = Some(now.clone());
            task.last_result = Some(result.clone());
            let _ = ScheduledTask::save_to(&path, &task);
        }
        self.update_state(|s| {
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                t.last_run = Some(now);
                t.last_result = Some(result);
            }
        });
    }

    /// 返回定时任务历史目录路径（供 API 层读取历史文件）。
    pub fn history_dir(&self) -> PathBuf {
        history_dir_of(&self.scheduled_dir)
    }

    /// 追加一条执行历史记录。
    pub(crate) fn add_history_record(
        &self,
        task_id: &str,
        status: &str,
        message: &str,
        duration: std::time::Duration,
    ) {
        let dir = history_dir_of(&self.scheduled_dir);
        if let Err(e) = append_history(&dir, task_id, status, message, duration) {
            tracing::warn!("写入执行历史失败 ({}): {}", task_id, e);
        }
    }
}

/// `SystemTime` → ISO 8601 字符串（经 UTC 中转）。
fn systemtime_to_iso(t: std::time::SystemTime) -> String {
    let utc: chrono::DateTime<chrono::Utc> = t.into();
    utc.to_rfc3339()
}
