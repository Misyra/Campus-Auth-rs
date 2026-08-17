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
    /// 后台阻塞任务（spawn_blocking）失败。
    #[error("后台任务失败: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    /// 序列化/反序列化失败。
    #[error("JSON 错误: {0}")]
    JsonError(#[from] serde_json::Error),
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
    /// cron 表达式解析失败的任务 ID 集合（enabled 但永不触发）。
    /// 暴露给 API 层（`is_cron_invalid`），前端据此显示"表达式无效"，
    /// 避免任务看似已启用却永远不触发的静默失效（M7）
    invalid_cron_ids: std::collections::HashSet<String>,
}

/// 调度器服务主结构。
pub struct SchedulerService {
    /// `tasks/scheduled/` 目录。
    scheduled_dir: PathBuf,
    /// 自引用弱句柄：`spawn_manual_run` 需要在 spawned task 中克隆自身 `Arc`，
    /// 经此获取而非 `self: &Arc<Self>` 接收者（消除 trait 化的结构摩擦，M1）。
    self_weak: std::sync::Weak<Self>,
    /// 加载 browser/script/shell 任务配置。
    task_manager: Arc<TaskManager>,
    /// 脚本/Shell/浏览器任务执行。
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
    /// 正在执行的定时任务 ID 集合：同一任务上一轮未结束前跳过下一轮触发，
    /// 防止执行时间长于 cron 周期的任务重叠运行（如重复操作同一浏览器实例）
    running_ids: std::sync::Mutex<std::collections::HashSet<String>>,
    /// 内部状态。
    state: std::sync::Mutex<SchedulerState>,
}

pub use crate::ServiceHandle;

impl SchedulerService {
    /// 构造调度器服务（直接返回 `Arc<Self>`）。
    ///
    /// 从 `ConfigService` 推导 `tasks/scheduled/` 目录并确保其存在；
    /// 创建容量 `CHANGE_CHANNEL_CAPACITY` 的内部变更 channel。
    /// 经 `Arc::new_cyclic` 初始化自引用弱句柄，使 `spawn_manual_run` 等
    /// 需要 clone 自身 Arc 的方法可用普通 `&self` 接收者表达（M1）。
    pub fn new(
        config: Arc<ConfigService>,
        tasks: Arc<TaskManager>,
        executor: Arc<crate::tasks::TaskExecutor>,
        status: Arc<StatusManager>,
        reload_rx: mpsc::Receiver<ConfigReloadSignal>,
    ) -> Result<Arc<Self>, SchedulerError> {
        let base_path = config.base_path();
        let scheduled_dir = base_path.join("tasks").join(SCHEDULED_DIR_NAME);
        std::fs::create_dir_all(&scheduled_dir).map_err(SchedulerError::IoError)?;
        let history_dir = scheduled_dir.join(HISTORY_DIR_NAME);
        std::fs::create_dir_all(&history_dir).map_err(SchedulerError::IoError)?;

        let (task_change_tx, task_change_rx) = mpsc::channel(CHANGE_CHANNEL_CAPACITY);

        Ok(Arc::new_cyclic(|weak| Self {
            scheduled_dir,
            self_weak: weak.clone(),
            task_manager: tasks,
            executor,
            status_manager: status,
            task_change_tx,
            task_change_rx: tokio::sync::Mutex::new(Some(task_change_rx)),
            reload_rx: tokio::sync::Mutex::new(Some(reload_rx)),
            concurrency: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SCHEDULED_TASKS)),
            running_ids: std::sync::Mutex::new(std::collections::HashSet::new()),
            state: std::sync::Mutex::new(SchedulerState {
                tasks: Vec::new(),
                running: true,
                next_fire_at: None,
                invalid_cron_ids: std::collections::HashSet::new(),
            }),
        }))
    }

    /// 尝试标记任务为"执行中"：已在执行则返回 false（跳过本轮触发）
    pub(crate) fn try_mark_running(&self, task_id: &str) -> bool {
        self.running_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id.to_string())
    }

    /// 清除"执行中"标记（任务结束，含异常路径，由 RAII 守卫调用）
    pub(crate) fn clear_running(&self, task_id: &str) {
        self.running_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
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
        self.state.lock().unwrap_or_else(|e| e.into_inner()).tasks.clone()
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
    pub async fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError> {
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        // 校验 cron 表达式：此前仅在加载时解析，非法表达式静默落盘、
        // 任务永不触发且 API 层返回 ok，用户无从得知
        crate::scheduler::cron_loop::parse_cron_expr(&task.cron)?;
        // 校验关联目标任务存在
        if !self.task_manager.has_task(&task.target_id) {
            return Err(SchedulerError::TargetNotFound(task.target_id.clone()));
        }

        let path = self.scheduled_dir.join(format!("{}.json", id));
        let mut to_save = task.clone();
        to_save.id = id.to_string();
        // 同步 fs 写入放入 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let path_for_blocking = path.clone();
        let to_save_for_blocking = to_save.clone();
        tokio::task::spawn_blocking(move || ScheduledTask::save_to(&path_for_blocking, &to_save_for_blocking))
            .await??;

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
    pub async fn delete_task(&self, id: &str) -> Result<(), SchedulerError> {
        // id 直接拼接路径，必须先校验防止路径穿越（如 `..%5C` 删除任意 .json）
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        let path = self.scheduled_dir.join(format!("{}.json", id));
        let path_for_blocking = path.clone();
        let id_for_blocking = id.to_string();
        // 存在性检查与删除均在阻塞线程完成（历史遗留 #12）
        tokio::task::spawn_blocking(move || -> Result<(), SchedulerError> {
            if !path_for_blocking.exists() {
                return Err(SchedulerError::TaskNotFound(id_for_blocking));
            }
            std::fs::remove_file(&path_for_blocking).map_err(SchedulerError::IoError)
        })
        .await??;
        self.update_state(|s| s.tasks.retain(|t| t.id != id));
        self.notify_change();
        self.publish_status();
        Ok(())
    }

    /// 启用/禁用任务（复用 save_task 的持久化与通知逻辑）。
    pub async fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError> {
        let mut task = self
            .get_task(id)
            .ok_or_else(|| SchedulerError::TaskNotFound(id.to_string()))?;
        task.enabled = enabled;
        self.save_task(id, &task).await
    }

    /// 向主循环发送 `TaskChange::Reload` 信号（buffer 满时静默丢弃，重载幂等）。
    pub fn notify_change(&self) {
        let _ = self.task_change_tx.try_send(TaskChange::Reload);
    }

    /// 更新内部状态。
    pub(crate) fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut SchedulerState),
    {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
    }

    /// 查询指定任务的 cron 表达式是否解析失败（enabled 但永不触发）
    pub fn is_cron_invalid(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalid_cron_ids
            .contains(id)
    }

    /// 将当前运行状态广播到 StatusManager。
    fn publish_status(&self) {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (running, next, count) = (s.running, s.next_fire_at.map(systemtime_to_iso), s.tasks.len());
        self.status_manager.merge(crate::status::PartialSnapshot::Scheduler {
            running,
            next_fire_at: next,
            task_count: count,
        });
    }

    /// 任务执行后更新 `last_run` / `last_result`（内存 + 磁盘）。
    pub(crate) async fn update_last_run(&self, task_id: &str, status: &str, message: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let result = format!("[{}] {}", status, message);
        let path = self.scheduled_dir.join(format!("{}.json", task_id));
        // 磁盘读写移至 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let path_for_blocking = path.clone();
        let now_for_blocking = now.clone();
        let result_for_blocking = result.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(mut task) = ScheduledTask::load_from(&path_for_blocking) {
                task.last_run = Some(now_for_blocking);
                task.last_result = Some(result_for_blocking);
                let _ = ScheduledTask::save_to(&path_for_blocking, &task);
            }
        })
        .await;
        self.update_state(|s| {
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                t.last_run = Some(now);
                t.last_result = Some(result);
            }
        });
    }

    /// 手动触发执行定时任务：与 cron 触发共用同一并发信号量闸。
    /// 手动触发与定时触发走同一执行路径（`execute_scheduled_task`），
    /// 保证 run_id 不被死数据浪费、手动与 cron 触发共享 concurrency 限制。
    /// 同一任务已在执行时拒绝再次触发（与 cron 防重叠规则一致）。
    ///
    /// 自身 Arc 经 `self_weak` 升级获取（服务由容器持有强引用，运行期必然可达）。
    pub fn spawn_manual_run(&self, task: crate::scheduler::task::ScheduledTask) {
        let Some(svc) = self.self_weak.upgrade() else {
            tracing::warn!(task_id = %task.id, "调度器已释放，忽略手动触发");
            return;
        };
        if !svc.try_mark_running(&task.id) {
            tracing::warn!(task_id = %task.id, "任务正在执行中，拒绝手动重复触发");
            return;
        }
        let sem = svc.concurrency.clone();
        let marked_id = task.id.clone();
        tokio::spawn(async move {
            if let Ok(_permit) = sem.acquire_owned().await {
                crate::scheduler::cron_loop::execute_scheduled_task(task, svc).await;
            } else {
                svc.clear_running(&marked_id);
            }
        });
    }

    /// 返回定时任务历史目录路径（供 API 层读取历史文件）。
    pub fn history_dir(&self) -> PathBuf {
        history_dir_of(&self.scheduled_dir)
    }

    /// 追加一条执行历史记录。
    pub(crate) async fn add_history_record(
        &self,
        task_id: &str,
        status: &str,
        message: &str,
        duration: std::time::Duration,
    ) {
        let dir = history_dir_of(&self.scheduled_dir);
        // 历史追加移至 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let dir_for_blocking = dir.clone();
        let task_id_owned = task_id.to_string();
        let task_id_for_log = task_id_owned.clone();
        let status_owned = status.to_string();
        let message_owned = message.to_string();
        tokio::task::spawn_blocking(move || {
            append_history(
                &dir_for_blocking,
                &task_id_owned,
                &status_owned,
                &message_owned,
                duration,
            )
        })
        .await
        .map(|r| {
            if let Err(e) = r {
                tracing::warn!("写入执行历史失败 ({}): {}", task_id_for_log, e);
            }
        })
        .unwrap_or_else(|e| {
            tracing::warn!("写入执行历史任务失败: {e}");
        });
    }
}

/// Web 层消费的调度器抽象（M1 细粒度 state：scheduler 域）。
///
/// handler 通过 `State<Arc<dyn SchedulerApi>>` 提取依赖，不再触达
/// `state.container`，测试可注入内存实现（见 `web/routes/scheduler.rs` 模块测试）。
#[async_trait::async_trait]
pub trait SchedulerApi: Send + Sync {
    /// 返回内存缓存中的任务列表副本。
    fn list_tasks(&self) -> Vec<ScheduledTask>;
    /// 查询单个任务。
    fn get_task(&self, id: &str) -> Option<ScheduledTask>;
    /// 返回目标任务的类型（`browser`/`script`/`shell`）。
    async fn task_type_of(&self, target_id: &str) -> Option<&'static str>;
    /// 保存任务（创建或更新）。
    async fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError>;
    /// 删除任务。
    async fn delete_task(&self, id: &str) -> Result<(), SchedulerError>;
    /// 启用/禁用任务。
    async fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError>;
    /// 通知主循环重算。
    fn notify_change(&self);
    /// 查询指定任务的 cron 表达式是否解析失败。
    fn is_cron_invalid(&self, id: &str) -> bool;
    /// 手动触发执行定时任务。
    fn spawn_manual_run(&self, task: ScheduledTask);
    /// 返回定时任务历史目录路径。
    fn history_dir(&self) -> PathBuf;
}

#[async_trait::async_trait]
impl SchedulerApi for SchedulerService {
    fn list_tasks(&self) -> Vec<ScheduledTask> {
        SchedulerService::list_tasks(self)
    }

    fn get_task(&self, id: &str) -> Option<ScheduledTask> {
        SchedulerService::get_task(self, id)
    }

    async fn task_type_of(&self, target_id: &str) -> Option<&'static str> {
        SchedulerService::task_type_of(self, target_id).await
    }

    async fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError> {
        SchedulerService::save_task(self, id, task).await
    }

    async fn delete_task(&self, id: &str) -> Result<(), SchedulerError> {
        SchedulerService::delete_task(self, id).await
    }

    async fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError> {
        SchedulerService::toggle_task(self, id, enabled).await
    }

    fn notify_change(&self) {
        SchedulerService::notify_change(self);
    }

    fn is_cron_invalid(&self, id: &str) -> bool {
        SchedulerService::is_cron_invalid(self, id)
    }

    fn spawn_manual_run(&self, task: ScheduledTask) {
        SchedulerService::spawn_manual_run(self, task);
    }

    fn history_dir(&self) -> PathBuf {
        SchedulerService::history_dir(self)
    }
}

/// `SystemTime` → ISO 8601 字符串（经 UTC 中转）。
fn systemtime_to_iso(t: std::time::SystemTime) -> String {
    let utc: chrono::DateTime<chrono::Utc> = t.into();
    utc.to_rfc3339()
}
