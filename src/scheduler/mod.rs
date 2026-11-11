//! 调度器服务模块。
//!
//! 负责 `SchedulerService` 的构造、定时任务的 CRUD 与持久化、配置热重载
//! 通知，以及调度主循环（`cron_loop`）的生命周期管理（启动/停止）。
//!
//! 构造函数 `new` 注入 `ConfigService` / `TaskManager` / `LoginOrchestrator` /
//! `TaskExecutor` / `StatusManager`，并通过 `ConfigReloadSignal` 流感知配置变更；
//! `start` 启动后台调度 task 并返回可停止的 `ServiceHandle`。

pub mod cron_loop;
pub mod task;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::ConfigService;
use crate::config::runtime::ConfigReloadSignal;
use crate::status::StatusManager;
use crate::tasks::TaskManager;
use crate::web::error::ApiError;

use self::cron_loop::RunSource;
use self::task::{
    CHANGE_CHANNEL_CAPACITY, MAX_CONCURRENT_SCHEDULED_TASKS, ScheduledTask, append_history,
    history_dir_of, map_history_records, next_success_count,
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
    /// 配置服务（任务通知开关 app.task_notification 读取）
    config: Arc<ConfigService>,
    /// 加载 browser/script 任务配置。
    task_manager: Arc<TaskManager>,
    /// 脚本/浏览器任务执行。
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
    /// 所有定时与手动执行任务的生命周期追踪器。
    task_tracker: TaskTracker,
    /// 服务关闭时协作取消所有在途任务。
    task_cancel: CancellationToken,
    /// 定时任务文件写入串行锁：`save_task` 与 `update_last_run` 均为读-改-写，
    /// 无锁并发会导致 cron/last_run 互相覆盖
    file_mutex: tokio::sync::Mutex<()>,
    /// login_once 模式标记：单次登录模式登录即退，启动触发的执行轮在延迟到期后
    /// 检查此标记并放弃执行（否则任务会被关闭取消半途而废）。由 Launcher 在
    /// 模式分发前设置；调度器自身无法从配置可靠区分 CLI 注入的 login_once。
    login_once: std::sync::atomic::AtomicBool,
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
        // 路径经 `utils::paths` 统一；启动已预建，此处保留幂等创建（测试直构兼容）。
        let scheduled_dir = crate::utils::paths::scheduled_dir(&base_path);
        std::fs::create_dir_all(&scheduled_dir).map_err(SchedulerError::IoError)?;
        let history_dir = crate::utils::paths::scheduled_history_dir(&base_path);
        std::fs::create_dir_all(&history_dir).map_err(SchedulerError::IoError)?;

        let (task_change_tx, task_change_rx) = mpsc::channel(CHANGE_CHANNEL_CAPACITY);

        Ok(Arc::new_cyclic(|weak| Self {
            config,
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
            task_tracker: TaskTracker::new(),
            task_cancel: CancellationToken::new(),
            file_mutex: tokio::sync::Mutex::new(()),
            login_once: std::sync::atomic::AtomicBool::new(false),
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

    /// 标记当前运行模式为 login_once（由 Launcher 在模式分发前调用）。
    pub fn set_login_once_mode(&self, login_once: bool) {
        self.login_once
            .store(login_once, std::sync::atomic::Ordering::Relaxed);
    }

    /// 当前是否处于 login_once 模式（启动触发执行轮据此放弃执行）。
    pub(crate) fn is_login_once_mode(&self) -> bool {
        self.login_once.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 启动调度循环，返回可停止的服务句柄。
    pub async fn start(self: Arc<Self>) -> ServiceHandle {
        let (stop_tx, stop_rx) = watch::channel(false);
        let task_change_rx = self
            .task_change_rx
            .lock()
            .await
            .take()
            .expect("SchedulerService::start() 只能调用一次");
        let reload_rx = self
            .reload_rx
            .lock()
            .await
            .take()
            .expect("SchedulerService::start() 只能调用一次");
        let svc = self.clone();
        let join_handle = tokio::spawn(async move {
            cron_loop::cron_loop(svc, stop_rx, Some(task_change_rx), Some(reload_rx)).await;
        });
        ServiceHandle {
            stop_tx,
            join_handle,
            name: "scheduler",
        }
    }

    /// 返回内存缓存中的任务列表副本。
    pub fn list_tasks(&self) -> Vec<ScheduledTask> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .clone()
    }

    /// 返回目标任务的类型（`browser`/`script`），供展示层补充类型标签。
    ///
    /// 类型权威来源为 [`crate::tasks::TaskKind`]（由 target_id 关联的任务推导），
    /// 定时任务存储模型本身不再冗余存类型。
    pub async fn task_type_of(&self, target_id: &str) -> Option<&'static str> {
        self.task_manager
            .load_task(target_id)
            .await
            .ok()
            .map(|task| task.type_name())
    }

    /// 查询单个任务。
    pub fn get_task(&self, id: &str) -> Option<ScheduledTask> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    /// 保存任务（创建或更新）到磁盘与缓存，并通知主循环重算。
    pub async fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError> {
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        let mut to_save = task.clone();
        to_save.id = id.to_string();
        // 触发方式归一化先行（钳制/清理字段），再按触发方式校验：
        // 仅 cron 触发需要 cron 表达式合法——此前非法表达式静默落盘、任务
        // 永不触发且 API 层返回 ok；启动触发不依赖 cron，跳过校验
        to_save.normalize_for_save();
        if to_save.trigger == crate::scheduler::task::TaskTrigger::Cron {
            crate::scheduler::cron_loop::parse_cron_expr(&to_save.cron)?;
        }
        // 校验关联目标任务存在
        if !self.task_manager.has_task(&to_save.target_id) {
            return Err(SchedulerError::TargetNotFound(to_save.target_id.clone()));
        }
        // 按绑定任务类型钳制 timeout 覆写（回写保存，见 clamp_scheduled_timeout）。
        // 判型失败（目标任务 JSON 损坏等）不阻断保存，维持原值由执行侧兜底钳制。
        if let Ok(kind) = self.task_manager.load_task(&to_save.target_id).await {
            clamp_scheduled_timeout(&mut to_save, kind.type_name());
        }

        // 文件写入串行化：与 update_last_run 同锁，防读-改-写互相覆盖
        let _file_guard = self.file_mutex.lock().await;
        self.save_task_locked(id, to_save).await
    }

    /// 锁内保存：落盘 + 更新缓存 + 通知主循环（A14 自 `save_task` 拆出）。
    ///
    /// 调用方必须已持有 `file_mutex`（tokio Mutex 不可重入，锁内不得再走
    /// `save_task` 公共入口）；校验/钳制由调用方在锁前完成。
    async fn save_task_locked(
        &self,
        id: &str,
        to_save: ScheduledTask,
    ) -> Result<(), SchedulerError> {
        // 同步 fs 写入放入 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let path = self.scheduled_dir.join(format!("{}.json", id));
        let to_save_for_blocking = to_save.clone();
        tokio::task::spawn_blocking(move || ScheduledTask::save_to(&path, &to_save_for_blocking))
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
        tracing::info!(task_id = %id, "定时任务已保存");
        Ok(())
    }

    /// 删除任务文件并更新缓存，通知主循环重算。
    ///
    /// 同时清掉该任务的执行历史（`history/{id}.json`）：只删任务会让历史文件残留成
    /// 孤儿——用户删掉任务后界面已无入口查看它，文件却永远留在 `tasks/scheduled/history/`
    /// 下；一旦 id 被复用（旧版本按序号命名）还会把上一次任务的历史当成自己的。
    /// 清理是**尽力而为**：历史删除失败不影响任务本身的删除结果。
    pub async fn delete_task(&self, id: &str) -> Result<(), SchedulerError> {
        // id 直接拼接路径，必须先校验防止路径穿越（如 `..%5C` 删除任意 .json）
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        let path = self.scheduled_dir.join(format!("{}.json", id));
        let history_path = history_dir_of(&self.scheduled_dir).join(format!("{id}.json"));
        // 与 save_task/update_last_run 串行化，防止 last_run 的读-改-写在删除后
        // 将同名任务文件重新写回磁盘。
        let _file_guard = self.file_mutex.lock().await;
        let path_for_blocking = path.clone();
        let id_for_blocking = id.to_string();
        // 存在性检查与删除均在阻塞线程完成（历史遗留 #12）
        tokio::task::spawn_blocking(move || -> Result<(), SchedulerError> {
            if !path_for_blocking.exists() {
                return Err(SchedulerError::TaskNotFound(id_for_blocking));
            }
            std::fs::remove_file(&path_for_blocking).map_err(SchedulerError::IoError)?;
            if let Err(error) = std::fs::remove_file(&history_path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    path = %history_path.display(),
                    "定时任务执行历史清理失败（任务本身已删除）: {error}"
                );
            }
            Ok(())
        })
        .await??;
        self.update_state(|s| s.tasks.retain(|t| t.id != id));
        self.notify_change();
        self.publish_status();
        tracing::info!(task_id = %id, "定时任务已删除");
        Ok(())
    }

    /// 启用/禁用任务（持久化与通知逻辑同 [`Self::save_task`]，见 `save_task_locked`）。
    ///
    /// A14：与 `delete_task` 的读-改-写全程串行化，并在锁内按**磁盘**复核存在性——
    /// 缓存读到 ≠ 磁盘还在（外部直接删除文件要等 5 分钟兜底扫描才从缓存移除），
    /// 若只信缓存，锁内 `save_task` 会把已删除的任务文件重建（下一轮 reload 复活）。
    pub async fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError> {
        // id 直接拼接路径，必须先校验防止路径穿越（与 delete_task 同口径）
        if !ScheduledTask::is_valid_id(id) {
            return Err(SchedulerError::InvalidTaskId(id.to_string()));
        }
        let _file_guard = self.file_mutex.lock().await;
        // 存在性检查在阻塞线程完成（历史遗留 #12）
        let path = self.scheduled_dir.join(format!("{id}.json"));
        let exists = tokio::task::spawn_blocking(move || path.exists())
            .await
            .map_err(SchedulerError::JoinError)?;
        if !exists {
            // 主动收敛内存缓存，不等 5 分钟兜底扫描
            self.update_state(|s| s.tasks.retain(|t| t.id != id));
            return Err(SchedulerError::TaskNotFound(id.to_string()));
        }
        let Some(mut task) = self.get_task(id) else {
            return Err(SchedulerError::TaskNotFound(id.to_string()));
        };
        task.enabled = enabled;
        self.save_task_locked(id, task).await
    }

    /// 向主循环发送 `TaskChange::Reload` 信号（buffer 满时静默丢弃，重载幂等）。
    pub fn notify_change(&self) {
        if let Err(e) = self.task_change_tx.try_send(TaskChange::Reload) {
            // 丢弃是可接受的（重载幂等，低频兜底扫描会补齐），仅 debug 留痕
            tracing::debug!("发送任务变更通知失败（channel 满或已关闭）: {e}");
        }
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
        let (running, next, count) = (
            s.running,
            s.next_fire_at.map(systemtime_to_iso),
            s.tasks.len(),
        );
        self.status_manager
            .merge(crate::status::PartialSnapshot::Scheduler {
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
        // 与 save_task 同锁串行化，防全量重写互相覆盖
        let _file_guard = self.file_mutex.lock().await;
        // 磁盘读写移至 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let path_for_blocking = path.clone();
        let now_for_blocking = now.clone();
        let result_for_blocking = result.clone();
        let task_id_for_blocking = task_id.to_string();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            if let Ok(mut task) = ScheduledTask::load_from(&path_for_blocking) {
                task.last_run = Some(now_for_blocking);
                task.last_result = Some(result_for_blocking);
                if let Err(e) = ScheduledTask::save_to(&path_for_blocking, &task) {
                    tracing::warn!(task_id = %task_id_for_blocking, "定时任务 last_run 持久化失败: {e}");
                }
            }
        })
        .await
        {
            // 阻塞任务 panic/被取消：磁盘更新已丢失，仅内存快照仍正确——必须留痕
            tracing::warn!(task_id = %task_id, "定时任务持久化的阻塞任务失败: {e}");
        }
        self.update_state(|s| {
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                t.last_run = Some(now);
                t.last_result = Some(result);
            }
        });
    }

    /// 启动触发任务成功执行后递增当日成功计数（内存 + 磁盘）。
    ///
    /// 仅成功计入（用户口径：失败不算执行次数）；读-改-写与 save_task /
    /// update_last_run 同锁串行化。磁盘与内存各按当前值独立推算下一笔
    /// （纯函数 [`next_success_count`]，跨天窗口键不匹配时重置为 1）。
    pub(crate) async fn record_startup_success(&self, task_id: &str) {
        let today = ScheduledTask::local_today();
        let path = self.scheduled_dir.join(format!("{}.json", task_id));
        // 与 save_task/update_last_run 同锁串行化，防读-改-写互相覆盖
        let _file_guard = self.file_mutex.lock().await;
        let path_for_blocking = path.clone();
        let today_for_blocking = today.clone();
        let task_id_for_blocking = task_id.to_string();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            if let Ok(mut task) = ScheduledTask::load_from(&path_for_blocking) {
                task.startup_success = Some(next_success_count(
                    task.startup_success.as_ref(),
                    &today_for_blocking,
                ));
                if let Err(e) = ScheduledTask::save_to(&path_for_blocking, &task) {
                    tracing::warn!(task_id = %task_id_for_blocking, "启动成功计数持久化失败: {e}");
                }
            }
        })
        .await
        {
            // 阻塞任务 panic/被取消：磁盘更新已丢失，仅内存快照仍正确——必须留痕
            tracing::warn!(task_id = %task_id, "启动成功计数持久化的阻塞任务失败: {e}");
        }
        self.update_state(|s| {
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                t.startup_success = Some(next_success_count(t.startup_success.as_ref(), &today));
            }
        });
    }

    /// 手动触发执行定时任务：与 cron 触发共用同一并发信号量闸。
    /// 手动触发与定时触发走同一执行路径（`execute_scheduled_task`），
    /// 保证 run_id 不被死数据浪费、手动与 cron 触发共享 concurrency 限制。
    /// 同一任务已在执行时拒绝再次触发（与 cron 防重叠规则一致）。
    ///
    /// 自身 Arc 经 `self_weak` 升级获取（服务由容器持有强引用，运行期必然可达）。
    pub fn spawn_manual_run(&self, task: crate::scheduler::task::ScheduledTask) -> bool {
        let Some(svc) = self.self_weak.upgrade() else {
            tracing::warn!(task_id = %task.id, "调度器已释放，忽略手动触发");
            return false;
        };
        if !svc.try_mark_running(&task.id) {
            tracing::warn!(task_id = %task.id, "任务正在执行中，拒绝手动重复触发");
            return false;
        }
        svc.spawn_tracked_run(task, RunSource::Manual);
        true
    }

    /// 将一次任务执行纳入服务生命周期，关闭时统一取消并等待清理完成。
    ///
    /// `source` 决定执行体与历史来源标注：Cron/Manual 为单次尝试，
    /// Startup 走启动执行轮（延迟 + 失败重试 + 成功计数）。
    pub(crate) fn spawn_tracked_run(self: Arc<Self>, task: ScheduledTask, source: RunSource) {
        let marked_id = task.id.clone();
        if self.task_cancel.is_cancelled() {
            self.clear_running(&marked_id);
            return;
        }

        let sem = self.concurrency.clone();
        let cancel = self.task_cancel.clone();
        let svc = self.clone();
        self.task_tracker.spawn(async move {
            let permit = tokio::select! {
                _ = cancel.cancelled() => {
                    svc.clear_running(&marked_id);
                    return;
                }
                permit = sem.acquire_owned() => permit,
            };
            let Ok(_permit) = permit else {
                svc.clear_running(&marked_id);
                return;
            };

            tokio::select! {
                _ = cancel.cancelled() => {
                    // 若执行 future 已启动，析构链会触发 RunningGuard 与子进程
                    // Job Object 守卫，分别清除运行标记与回收整棵进程树。
                    svc.clear_running(&marked_id);
                }
                _ = {
                    // 克隆后的 svc 移入 async block：第一个 select 分支仍需使用原值
                    let svc = svc.clone();
                    async move {
                        match source {
                            RunSource::Startup => {
                                crate::scheduler::cron_loop::execute_startup_round(task, svc)
                                    .await;
                            }
                            _ => {
                                crate::scheduler::cron_loop::execute_scheduled_task(
                                    task, svc, source,
                                )
                                .await;
                            }
                        }
                    }
                } => {}
            }
        });
    }

    /// 停止接收新执行并等待所有在途任务完成取消清理。
    pub(crate) async fn shutdown_tracked_runs(&self) {
        self.task_cancel.cancel();
        self.task_tracker.close();
        self.task_tracker.wait().await;
    }

    /// 读取定时任务执行历史并映射为前端期望的扁平数组（内聚 id 校验 + 读盘 +
    /// 字段映射；此前由 web 层 handler 直接读盘跨层实现，M11 收敛入调度域）。
    ///
    /// 历史文件不存在时返回空数组；JSON 损坏时按空历史容错（不因历史文件
    /// 损坏让 API 报错）。
    pub(crate) async fn read_history(&self, id: &str) -> Result<Vec<serde_json::Value>, ApiError> {
        // id 直接拼接路径，必须先校验防止路径穿越读取任意 .json
        if !ScheduledTask::is_valid_id(id) {
            return Err(ApiError::BadRequest(format!("非法任务 ID: {id}")));
        }
        let path = history_dir_of(&self.scheduled_dir).join(format!("{id}.json"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let raw: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::json!({"runs": []}));
        Ok(map_history_records(&raw))
    }

    /// 追加一条执行历史记录（`trigger` 为触发来源标识 cron/startup/manual）。
    pub(crate) async fn add_history_record(
        &self,
        task_id: &str,
        status: &str,
        message: &str,
        duration: std::time::Duration,
        trigger: &str,
    ) {
        let dir = history_dir_of(&self.scheduled_dir);
        // 历史追加移至 spawn_blocking，避免阻塞 tokio worker 线程（历史遗留 #12）
        let dir_for_blocking = dir.clone();
        let task_id_owned = task_id.to_string();
        let task_id_for_log = task_id_owned.clone();
        let status_owned = status.to_string();
        let message_owned = message.to_string();
        let trigger_owned = trigger.to_string();
        tokio::task::spawn_blocking(move || {
            append_history(
                &dir_for_blocking,
                &task_id_owned,
                &status_owned,
                &message_owned,
                duration,
                &trigger_owned,
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

/// 按绑定任务类型钳制定时任务的 timeout 覆写（单位：秒），并**回写**到任务上
///
/// 执行侧口径（`tasks::executor` / `tasks::models`）：浏览器任务超时最终钳制到
/// `[MIN_TASK_TIMEOUT_MS, MAX_TASK_TIMEOUT_MS]` 毫秒（即 [1, 600] 秒），脚本任务
/// 钳到 `[MIN_SCRIPT_TIMEOUT, MAX_SCRIPT_TIMEOUT]` 秒。保存时不钳会让越界值落盘、
/// 运行时被静默改写，界面显示与实际执行口径不一致——此处按同一常量钳制并写回，
/// 保证保存值所见即所执行。http 直连任务不支持超时覆写（`execute_with_timeout_override`
/// 对其拒绝执行），不处理。
pub(crate) fn clamp_scheduled_timeout(task: &mut ScheduledTask, type_name: &str) {
    let Some(timeout) = task.timeout.as_mut() else {
        return;
    };
    match type_name {
        "browser" => {
            *timeout = (*timeout).clamp(
                crate::tasks::MIN_TASK_TIMEOUT_MS / 1000,
                crate::tasks::MAX_TASK_TIMEOUT_MS / 1000,
            );
        }
        "script" => {
            *timeout = (*timeout).clamp(
                crate::tasks::MIN_SCRIPT_TIMEOUT,
                crate::tasks::MAX_SCRIPT_TIMEOUT,
            );
        }
        _ => {}
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
    /// 返回目标任务的类型（`browser`/`script`）。
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
    fn spawn_manual_run(&self, task: ScheduledTask) -> bool;
    /// 读取任务执行历史（内聚 id 校验 + 读盘 + 前端字段映射），
    /// 返回扁平数组 `[{ run_at, success, message }]`。
    async fn read_history(&self, id: &str) -> Result<Vec<serde_json::Value>, ApiError>;
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

    fn spawn_manual_run(&self, task: ScheduledTask) -> bool {
        SchedulerService::spawn_manual_run(self, task)
    }

    async fn read_history(&self, id: &str) -> Result<Vec<serde_json::Value>, ApiError> {
        SchedulerService::read_history(self, id).await
    }
}

/// `SystemTime` → ISO 8601 字符串（带本地时区偏移）。
///
/// 定时任务按本地时间触发，此前输出 UTC `Z` 后缀让"下次触发"看起来差一个时区
///（known-issues #3）；转 Local 后偏移显式可读，`new Date()` 解析兼容。
fn systemtime_to_iso(t: std::time::SystemTime) -> String {
    let local: chrono::DateTime<chrono::Local> = t.into();
    local.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按绑定任务类型钳制 timeout 覆写：浏览器 [1,600] 秒、脚本 [1,3600] 秒，
    /// 与执行侧钳制常量一致；钳制值回写，None 不处理
    #[test]
    fn test_clamp_scheduled_timeout_by_kind() {
        let mut t = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        // 浏览器：越界值钳回 [1, 600]
        t.timeout = Some(0);
        clamp_scheduled_timeout(&mut t, "browser");
        assert_eq!(t.timeout, Some(1));
        t.timeout = Some(100_000);
        clamp_scheduled_timeout(&mut t, "browser");
        assert_eq!(
            t.timeout,
            Some(crate::tasks::MAX_TASK_TIMEOUT_MS / 1000),
            "浏览器任务不应超过 MAX_TASK_TIMEOUT_MS 换算的秒数"
        );
        // 脚本：越界值钳回 [1, 3600]
        t.timeout = Some(99_999);
        clamp_scheduled_timeout(&mut t, "script");
        assert_eq!(t.timeout, Some(crate::tasks::MAX_SCRIPT_TIMEOUT));
        // 界内值原样保留
        t.timeout = Some(120);
        clamp_scheduled_timeout(&mut t, "script");
        assert_eq!(t.timeout, Some(120));
        // http 直连任务不支持超时覆写，不处理
        t.timeout = Some(99_999);
        clamp_scheduled_timeout(&mut t, "http");
        assert_eq!(t.timeout, Some(99_999));
        // timeout = None 不动
        t.timeout = None;
        clamp_scheduled_timeout(&mut t, "browser");
        assert_eq!(t.timeout, None);
    }

    /// next_fire_at 序列化带本地时区偏移（known-issues #3）：
    /// 定时任务按本地时间触发，不得再输出 UTC `Z` 后缀造成时区误读
    #[test]
    fn test_systemtime_to_iso_uses_local_offset() {
        let iso = systemtime_to_iso(std::time::SystemTime::now());
        assert!(!iso.ends_with('Z'), "不应输出 UTC Z 后缀: {iso}");
        // RFC3339 偏移形如 +08:00（UTC 时区的机器输出 +00:00 而非 Z）
        let offset = &iso[iso.len() - 6..];
        assert!(
            offset.starts_with('+') || offset.starts_with('-'),
            "缺少时区偏移: {iso}"
        );
    }

    /// 构造仅装配依赖的真实 `SchedulerService`（不起调度循环、不落任务）。
    /// 各依赖均为廉价的内存级构造，装配方式参考 `engine::run_loop` 测试。
    /// 返回 TempDir 一并交给调用方持有，避免目录被提前清理。
    async fn make_test_service() -> (Arc<SchedulerService>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, reload_rx) = tokio::sync::mpsc::channel(8);
        let config = crate::config::ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let status = Arc::new(crate::status::StatusManager::new());
        let bridge = crate::bridge::BridgeSupervisor::new(
            tmp.path().to_path_buf(),
            config.clone(),
            status.clone(),
            None,
        );
        let environment =
            crate::environment::EnvironmentManager::new(tmp.path().to_path_buf(), status.clone());
        let tasks = crate::tasks::TaskManager::new(tmp.path());
        let executor = crate::tasks::TaskExecutor::new(
            tmp.path(),
            status.clone(),
            bridge,
            environment,
            config.clone(),
        );
        let service = SchedulerService::new(config, tasks, executor, status, reload_rx).unwrap();
        (service, tmp)
    }

    /// A2 回归：`get_task` 与同文件其余方法同口径——锁中毒时恢复读取内存缓存，
    /// 而非静默返回 None（此前 `.ok()` 口径下调度会因此瘫痪且无任何留痕）。
    #[tokio::test]
    async fn test_get_task_recovers_from_poisoned_state() {
        let (service, _tmp) = make_test_service().await;
        let task = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        service.update_state(|s| s.tasks.push(task));

        // 持锁 panic 制造中毒（catch_unwind 吞掉 panic 本身，不波及测试进程）
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = service.state.lock().unwrap();
            panic!("poison state lock");
        }));

        assert!(
            service.get_task("t1").is_some(),
            "锁中毒后 get_task 必须恢复读取缓存，而非静默返回 None"
        );
    }

    // ============ A13：cron/手动触发执行前以内存缓存重验最新定义 ============

    /// A13：任务既不在内存缓存也不在磁盘时，`execute_scheduled_task` 应立即放弃——
    /// 不执行、不写历史、不落任何文件（此前直接按触发时的快照执行）。
    #[tokio::test]
    async fn test_execute_scheduled_task_skips_when_task_missing() {
        let (service, _tmp) = make_test_service().await;
        let task = ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        // 任务未落盘、不在缓存（复现"到期瞬间已被删除"）：执行应立即放弃
        cron_loop::execute_scheduled_task(task, service.clone(), RunSource::Cron).await;

        let task_path = service.scheduled_dir.join("t1.json");
        let history_path = history_dir_of(&service.scheduled_dir).join("t1.json");
        assert!(!history_path.exists(), "不应写入执行历史");
        assert!(!task_path.exists(), "不应写回任务文件（last_run 未写入）");
        assert!(service.get_task("t1").is_none(), "缓存不应出现该任务");
    }

    /// A13：任务在缓存中但已禁用时，`execute_scheduled_task` 应放弃触发
    ///（与 `fire_due_tasks` 的禁用跳过构成双检，覆盖两跳之间被禁用的窗口）。
    #[tokio::test]
    async fn test_execute_scheduled_task_skips_when_task_disabled() {
        let (service, _tmp) = make_test_service().await;
        let mut task =
            ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string());
        task.enabled = false;
        service.update_state(|s| s.tasks.push(task));
        cron_loop::execute_scheduled_task(
            ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string()),
            service.clone(),
            RunSource::Manual,
        )
        .await;

        let history_path = history_dir_of(&service.scheduled_dir).join("t1.json");
        assert!(!history_path.exists(), "禁用任务不应执行并写入历史");
    }

    /// A13 对照组：任务落盘且 enabled（经磁盘加载进缓存）→ 执行尝试发生，
    /// 历史文件与 last_run 均写入。目标绑定任务不存在时执行以失败收场，
    /// 但执行链路（last_run/历史落盘）必须完整走完，避免浏览器等重依赖。
    #[tokio::test]
    async fn test_execute_scheduled_task_runs_enabled_task_and_writes_history() {
        let (service, _tmp) = make_test_service().await;
        let task = ScheduledTask::new(
            "t1".to_string(),
            "0 8 * * *".to_string(),
            "missing_target".to_string(),
        );
        let task_path = service.scheduled_dir.join("t1.json");
        ScheduledTask::save_to(&task_path, &task).unwrap();
        // 从磁盘加载进内存缓存（与调度循环 reload 同口径）
        cron_loop::load_and_parse_all(&service);
        assert!(service.get_task("t1").is_some(), "前置：任务应在内存缓存中");

        cron_loop::execute_scheduled_task(task, service.clone(), RunSource::Cron).await;

        let history_path = history_dir_of(&service.scheduled_dir).join("t1.json");
        assert!(
            history_path.exists(),
            "enabled 任务应完成执行尝试并写入历史文件"
        );
        let on_disk = ScheduledTask::load_from(&task_path).unwrap();
        assert!(on_disk.last_run.is_some(), "执行后 last_run 应已写入磁盘");
    }

    // ============ A14：toggle_task 与 delete_task/外部删除的竞态 ============

    /// 构造一个通过 `TaskManager` 真实落盘的目标任务（browser 单步 goto），
    /// 使 `SchedulerService::save_task` 的关联校验可以通过。
    async fn seed_target_task(service: &SchedulerService, id: &str) {
        let kind: crate::tasks::TaskKind = serde_json::from_value(serde_json::json!({
            "type": "browser",
            "name": "目标任务",
            "steps": [{ "id": "go", "type": "goto", "selector": "https://example.com/login" }]
        }))
        .unwrap();
        service
            .task_manager
            .save_task(id, &kind)
            .await
            .expect("播种目标任务应成功");
    }

    /// A14：磁盘上已被外部删除（缓存尚未收敛）的任务，toggle 必须报
    /// `TaskNotFound` 且不得把任务文件重建（此前锁内 save_task 会复活已删任务），
    /// 同时主动收敛内存缓存。
    #[tokio::test]
    async fn test_toggle_deleted_task_returns_not_found_without_resurrecting() {
        let (service, _tmp) = make_test_service().await;
        seed_target_task(&service, "x").await;
        service
            .save_task(
                "t1",
                &ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string()),
            )
            .await
            .unwrap();
        let task_path = service.scheduled_dir.join("t1.json");
        assert!(
            task_path.exists() && service.get_task("t1").is_some(),
            "前置：任务应已落盘并入缓存"
        );

        // 模拟外部删除（绕过 delete_task，制造"磁盘已删、缓存仍在"的失步窗口）
        std::fs::remove_file(&task_path).unwrap();

        let err = service.toggle_task("t1", false).await.unwrap_err();
        assert!(
            matches!(err, SchedulerError::TaskNotFound(_)),
            "应报 TaskNotFound，实际 {err:?}"
        );
        assert!(!task_path.exists(), "toggle 不得复活已删除的任务文件");
        assert!(
            service.get_task("t1").is_none(),
            "缓存应同步收敛移除已删任务"
        );
    }

    /// A14 对照组：正常 toggle 翻转 enabled 且落盘文件同步（防 save_task 拆分
    /// 重构弄丢落盘），双向各翻一次。
    #[tokio::test]
    async fn test_toggle_existing_task_flips_enabled() {
        let (service, _tmp) = make_test_service().await;
        seed_target_task(&service, "x").await;
        service
            .save_task(
                "t1",
                &ScheduledTask::new("t1".to_string(), "0 8 * * *".to_string(), "x".to_string()),
            )
            .await
            .unwrap();
        let task_path = service.scheduled_dir.join("t1.json");

        let before = service.get_task("t1").unwrap().enabled;
        service.toggle_task("t1", !before).await.unwrap();
        assert_eq!(
            service.get_task("t1").unwrap().enabled,
            !before,
            "内存缓存的 enabled 应翻转"
        );
        let on_disk = ScheduledTask::load_from(&task_path).unwrap();
        assert_eq!(on_disk.enabled, !before, "落盘文件的 enabled 应同步翻转");

        service.toggle_task("t1", before).await.unwrap();
        assert_eq!(service.get_task("t1").unwrap().enabled, before);
        assert_eq!(
            ScheduledTask::load_from(&task_path).unwrap().enabled,
            before
        );
    }
}
