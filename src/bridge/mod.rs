//! Python Bridge：BridgeSupervisor + 子进程管理
//!
//! 通过 NDJSON IPC 与 Python Worker 子进程通信。Supervisor 单 task 用 `select!` 监听：
//! 停止信号、外部命令（execute/cancel/shutdown/idle-timeout）、以及 Worker 回传的 IPC 消息。
//!
//! ## 锁中毒恢复策略（M5 审查结论）
//!
//! 内部状态锁统一以 `lock().unwrap_or_else(|e| e.into_inner())` 从中毒恢复。
//! 审查确认本模块生产路径无 `unwrap()/expect/panic!`（仅 `spawn` 与 `Arc::get_mut`
//! 两处构造期不变量断言），且 `execute_inner` 的原子临界区（注册 pending → 发送 →
//! 改动会话状态）全程无不可回滚的可失败操作，中毒实际无从触发。因此不引入
//! poison 后的不变量重建逻辑——为不存在的 panic 路径加重建反而引入新复杂度。
//! 若未来在持锁区间加入可 panic 操作，须同步评估恢复路径的状态一致性。

pub mod ipc;
#[cfg(windows)]
pub mod job;
pub mod orphan;
pub mod process;
pub mod session;
pub mod worker;

pub use ipc::{
    CancelNotification, IpcEvent, IpcRequest, IpcResponse, IpcResult, Outcome, StructuredResult,
};
pub use process::{spawn_worker, IpcMessage, ParsedMessage, ProcessHandles, WorkerProcess};
pub use session::{CancelRegistry, SessionGuard, SessionType};
pub use worker::{worker_state_to_status, WorkerState};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::ConfigService;
use crate::environment::{PYTHON_EXE_RELATIVE, WORKER_PROJECT_DIR};
use crate::status::{PartialSnapshot, StatusManager, WorkerStatus};
use crate::utils::metrics::Metrics;

/// Worker 空闲回收默认阈值（秒）
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// 发送 shutdown 命令后等待优雅退出的超时（秒）
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 5;
/// spawn 后等待 browser_health_check 通过的超时（秒）
pub const DEFAULT_WORKER_STARTUP_TIMEOUT_SECS: u64 = 30;
/// NDJSON 行分隔符
pub const IPC_DELIMITER: u8 = b'\n';
/// 单行最大长度（1MB）
pub const IPC_MAX_LINE_LEN: usize = 1_048_576;
/// stdin writer mpsc channel 容量
pub const IPC_WRITE_CHANNEL_CAP: usize = 16;
/// shutdown 命令使用的哨兵请求 id
///
/// 保留值：真实请求 id 从 1 开始递增，永不复用此值，避免 shutdown 哨兵与
/// 真实请求（如首次 browser_health_check）的 pending 槽位碰撞（历史遗留 F3）。
pub const SHUTDOWN_REQUEST_ID: u64 = 0;

/// Bridge 相关错误
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Python 环境未安装
    #[error("Worker 环境未安装")]
    WorkerNotInstalled,

    /// Worker 启动超时
    #[error("Worker 启动超时")]
    WorkerStartupTimeout,

    /// Worker 进程崩溃
    #[error("Worker 进程崩溃: {reason}")]
    WorkerCrashed { reason: String },

    /// Worker 正忙（调试会话进行中）
    #[error("Worker 忙: 调试会话进行中")]
    WorkerBusy,

    /// IPC 写入失败
    #[error("IPC 写入失败: {0}")]
    IpcWriteError(#[source] std::io::Error),

    /// IPC 读取失败
    #[error("IPC 读取失败: {0}")]
    IpcReadError(#[source] std::io::Error),

    /// Python 侧返回错误结果
    #[error("Worker 执行错误: {message}")]
    ExecutionError {
        message: String,
        data: Option<Value>,
    },

    /// 请求超时
    #[error("请求超时")]
    Timeout,

    /// 请求被取消
    #[error("请求已取消")]
    Cancelled,

    /// 调试会话被强制关闭
    #[error("调试会话已关闭")]
    DebugSessionClosed,

    /// Supervisor 未启动
    #[error("Bridge Supervisor 未运行")]
    SupervisorNotRunning,

    /// 子进程 spawn 失败
    #[error("Worker 进程启动失败: {0}")]
    SpawnFailed(#[source] std::io::Error),

    /// 内部错误（不应发生）
    #[error("Bridge 内部错误: {0}")]
    Internal(String),

    /// Worker 连续启动失败熔断（B3）
    #[error("Worker 环境异常，请重新引导")]
    WorkerSpawnBlocked,
}

/// Supervisor 后台 task 处理的命令
enum SupervisorCommand {
    /// 执行命令，结果通过 oneshot 返回
    Execute {
        method: String,
        params: Value,
        response_tx: mpsc::Sender<Result<IpcResponse, BridgeError>>,
    },
    /// 发送取消通知
    Cancel { cancel_id: String },
    /// 优雅关闭 Worker
    Shutdown,
    /// 空闲计时器触发
    IdleTimeout,
}

/// Bridge 内部状态（Mutex 保护）
struct BridgeInner {
    worker_state: WorkerState,
    process: Option<WorkerProcess>,
    pending_requests: HashMap<u64, oneshot::Sender<Result<IpcResponse, BridgeError>>>,
    next_request_id: u64,
    last_activity: Instant,
    idle_timer: Option<JoinHandle<()>>,
    cancel_registry: CancelRegistry,
    current_session: Option<SessionType>,
    current_cancel_id: Option<String>,
    current_request_id: Option<u64>,
    /// Supervisor 主循环持有的 IPC 消息 Receiver 对应的 Sender
    ipc_tx: Option<mpsc::Sender<ParsedMessage>>,
    /// 连续 spawn/健康检查失败计数（B3 熔断）
    consecutive_spawn_failures: u32,
}

/// 连续 spawn 失败熔断阈值：达到后 ensure_worker 直接快速失败（不再 spawn）
const SPAWN_FAILURE_THRESHOLD: u32 = 3;

pub use crate::ServiceHandle;

/// Python Bridge 公共入口
pub struct BridgeSupervisor {
    inner: Mutex<BridgeInner>,
    config: Arc<ConfigService>,
    status: Arc<StatusManager>,
    base_path: PathBuf,
    cmd_tx: mpsc::Sender<SupervisorCommand>,
    cmd_rx: Mutex<Option<mpsc::Receiver<SupervisorCommand>>>,
    service_handle: Mutex<Option<watch::Sender<bool>>>,
    self_weak: Weak<BridgeSupervisor>,
    /// 运行指标（可选）
    metrics: Option<Arc<Metrics>>,
    /// WebSocket 事件广播通道（screenshot / step_progress 等）
    event_tx: Mutex<Option<broadcast::Sender<String>>>,
    /// 启动串行锁：保证最多一个协程执行 Worker spawn + 健康检查，避免重复 spawn
    /// 独立持有（tokio Mutex），不置于 std Mutex 保护的 BridgeInner 内，避免跨 await 持锁
    startup_lock: AsyncMutex<()>,
}

impl BridgeSupervisor {
    /// 构造 BridgeSupervisor（以 Arc 持有）
    pub fn new(
        base_path: PathBuf,
        config: Arc<ConfigService>,
        status: Arc<StatusManager>,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        Arc::new_cyclic(|weak| Self {
            inner: Mutex::new(BridgeInner {
                worker_state: WorkerState::NotInstalled,
                process: None,
                pending_requests: HashMap::new(),
                next_request_id: 1,
                last_activity: Instant::now(),
                idle_timer: None,
                cancel_registry: CancelRegistry::new(),
                current_session: None,
                current_cancel_id: None,
                current_request_id: None,
                ipc_tx: None,
                consecutive_spawn_failures: 0,
            }),
            config,
            status,
            base_path,
            cmd_tx,
            cmd_rx: Mutex::new(Some(cmd_rx)),
            service_handle: Mutex::new(None),
            self_weak: weak.clone(),
            metrics,
            event_tx: Mutex::new(None),
            startup_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// 统一执行入口，含懒加载、会话互斥检查、cancel_id 注册
    ///
    /// 默认超时 5 分钟（300s），适用于大多数登录流程。
    pub async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
        self.execute_with_timeout(method, params, std::time::Duration::from_secs(300))
            .await
    }

    /// 带自定义超时的执行入口
    pub async fn execute_with_timeout(
        &self,
        method: &str,
        mut params: Value,
        timeout: std::time::Duration,
    ) -> Result<IpcResponse, BridgeError> {
        // 自生成 cancel_id 并注入 params（调用方未提供时），使超时后能通过 Cancel 命令
        // 命中本地已注册的 CancellationToken（见下），立即唤醒转发 task 的 select 分支
        // → guard drop → 释放会话槽位与 pending（P1-7：超时不清理会导致槽位永久滞留）。
        let cancel_id = params
            .get("cancel_id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let id = Uuid::new_v4().to_string();
                if params.is_object() {
                    params["cancel_id"] = Value::String(id.clone());
                }
                // params 非 object 时无法注入，但 execute_inner 内部生成同名随机 id 的
                // 概率可忽略；此处返回生成的 id 仅用于超时后的 Cancel 命令（幂等 no-op）。
                id
            });
        let (tx, mut rx) = mpsc::channel(1);
        self.cmd_tx
            .send(SupervisorCommand::Execute {
                method: method.to_string(),
                params,
                response_tx: tx,
            })
            .await
            .map_err(|_| BridgeError::SupervisorNotRunning)?;
        match tokio::time::timeout(timeout, rx.recv()).await {
            // 超时返回前发送 Cancel：cancel_registry.trigger 立即唤醒本地 token →
            // 转发 task 提前返回并 drop guard → 会话槽位与 pending 被释放。
            // 幂等：若请求恰在超时瞬间已响应/已取消，trigger 与 stdin 发送均为 no-op。
            Err(_elapsed) => {
                self.cmd_tx
                    .send(SupervisorCommand::Cancel { cancel_id })
                    .await
                    .map_err(|_| BridgeError::SupervisorNotRunning)?;
                // A1 自愈兜底：Cancel 后给 Worker 一段宽限（Python 侧命令超时 + 关闭
                // 页面自愈），等待会话槽位释放；仍未释放说明自愈失败，强杀回收，
                // 避免挂起命令永久占用会话槽位导致死锁。
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let freed = {
                        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.current_request_id.is_none()
                    };
                    if freed || Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                let still_stuck = {
                    let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    inner.current_request_id.is_some()
                };
                if still_stuck {
                    warn!(target: "python_worker", "命令超时后 Worker 未自愈，强制回收");
                    kill_worker_now(self).await;
                }
                Err(BridgeError::Timeout)
            }
            Ok(Some(r)) => r,
            Ok(None) => Err(BridgeError::SupervisorNotRunning),
        }
    }

    /// 触发跨进程取消：向 Worker stdin 发送 {"cancel": cancel_id}
    pub fn cancel(&self, cancel_id: &str) {
        let _ = self.cmd_tx.try_send(SupervisorCommand::Cancel {
            cancel_id: cancel_id.to_string(),
        });
    }

    /// 注入 WebSocket 事件广播通道（由 app 层在构建 Router 时调用）
    pub fn set_event_tx(&self, tx: broadcast::Sender<String>) {
        *self.event_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    /// 优雅关闭 Worker（shutdown 命令 → 等超时 → kill）
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(SupervisorCommand::Shutdown).await;
    }

    /// 强制回收 Worker：立即强杀子进程并复位状态
    ///
    /// 供 [`crate::login::LoginSession`] 在 `NetworkError`/`UnknownError` 后调用，
    /// 强制回收可能已损坏的浏览器上下文。会清理会话与取消注册表。
    pub async fn force_recycle(&self) {
        // 清理会话与取消注册表
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.cancel_registry.trigger_all();
            inner.current_session = None;
            inner.current_cancel_id = None;
            inner.current_request_id = None;
            if let Some(h) = inner.idle_timer.take() {
                h.abort();
            }
        }
        // 强杀子进程并标记 Error
        kill_worker_now(self).await;
    }

    /// 启动 supervisor 后台 task（返回 ServiceHandle）
    pub fn spawn(&self) -> ServiceHandle {
        let cmd_rx = self
            .cmd_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("BridgeSupervisor::spawn 只能调用一次");
        // 创建 IPC 消息通道：Receiver 由主循环持有，Sender 随 Worker spawn 注入
        let (ipc_tx, ipc_rx) = mpsc::channel::<ParsedMessage>(64);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).ipc_tx = Some(ipc_tx);
        let (stop_tx, stop_rx) = watch::channel(false);
        let this = self
            .self_weak
            .upgrade()
            .expect("spawn 需以 Arc 持有 BridgeSupervisor");
        let join_handle = tokio::spawn(run_supervisor(this, cmd_rx, stop_rx, ipc_rx));
        let handle = ServiceHandle {
            stop_tx: stop_tx.clone(),
            join_handle,
        };
        *self.service_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        handle
    }

    /// 停止 supervisor task（ServiceHandle 模式）
    pub async fn stop(&self) {
        if let Some(tx) = self.service_handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(true);
        }
    }

    /// 查询当前外部状态
    pub fn worker_status(&self) -> WorkerStatus {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let alive = inner.process.is_some();
        worker_state_to_status(inner.worker_state, alive)
    }

    /// Worker 子进程是否存活
    ///
    /// 供登录会话终态收尾判断是否需要发送 `close_browser`：
    /// 进程已被 force_recycle / 空闲回收时跳过，避免仅为关浏览器而重新 spawn。
    pub fn has_live_worker(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).process.is_some()
    }

    /// 复位连续 spawn 失败计数（B3）
    ///
    /// 供 EnvironmentManager 成功重建环境后调用，解除熔断；
    /// worker_state 若为 Error 且无进程则复位为 Idle，允许重新 spawn。
    pub fn reset_spawn_failures(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.consecutive_spawn_failures = 0;
        if inner.process.is_none() && inner.worker_state == WorkerState::Error {
            inner.worker_state = WorkerState::Idle;
            merge_worker_status(&inner, &self.status);
        }
    }
}

/// Supervisor 后台主循环
async fn run_supervisor(
    this: Arc<BridgeSupervisor>,
    mut cmd_rx: mpsc::Receiver<SupervisorCommand>,
    mut stop_rx: watch::Receiver<bool>,
    mut ipc_rx: mpsc::Receiver<ParsedMessage>,
) {
    loop {
        tokio::select! {
            // biased：确保停止信号优先处理，避免关闭时仍接收新命令/消息
            biased;
            // 优先级 1：停止信号
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }
            // 优先级 2：外部命令
            Some(cmd) = cmd_rx.recv() => {
                handle_supervisor_command(&this, cmd).await;
            }
            // 优先级 3：Worker 回传的 IPC 消息（响应/事件/退出通知）
            Some(msg) = ipc_rx.recv() => {
                handle_ipc_message(&this, msg).await;
            }
        }
    }
    // 主循环退出：优雅回收 Worker，避免残留子进程与后台 task（历史遗留 F4）
    handle_shutdown(&this).await;
}

/// 处理 supervisor 命令
async fn handle_supervisor_command(this: &Arc<BridgeSupervisor>, cmd: SupervisorCommand) {
    match cmd {
        SupervisorCommand::Execute {
            method,
            params,
            response_tx,
        } => {
            // 整个 execute_inner 在独立 task 中执行：ensure_worker 内的健康检查
            // 需 await Worker 回传的响应，而响应必须经 supervisor 主循环路由。若在主循环内
            // 直接 await，主循环被阻塞、无法处理 IPC 消息，健康检查将永远超时（死锁）。
            // 因此此处把 execute_inner 整体 spawn，主循环保持空闲以接收响应。
            let sup = Arc::clone(this);
            tokio::spawn(async move {
                match execute_inner(&sup, &method, params).await {
                    Ok((rx, _guard, token)) => {
                        // 等待响应期间监听取消令牌：CancelRegistry.trigger 触发
                        // token.cancel() 时立即返回 Cancelled，无需等待 Worker 自行退出；
                        // Cancel 命令分支仍会向 Worker 发送 IPC Cancel 消息协同取消。
                        let result = tokio::select! {
                            r = rx => r.unwrap_or(Err(BridgeError::SupervisorNotRunning)),
                            _ = token.cancelled() => Err(BridgeError::Cancelled),
                        };
                        let _ = response_tx.send(result).await;
                        // _guard drop 触发 reset_session 清理
                    }
                    Err(e) => {
                        let _ = response_tx.send(Err(e)).await;
                    }
                }
            });
        }
        SupervisorCommand::Cancel { cancel_id } => {
            // 触发取消令牌（同步），并克隆 stdin sender 以便释放锁后可靠发送。
            // 克隆 sender 而非在锁内 try_send，既消除 TOCTOU 竞态（process 可能在 check 与
            // use 之间被置空），又用 await send 替代 try_send，避免 channel 满时静默丢弃
            // 取消通知（历史遗留 F2）。
            let stdin_tx = {
                let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.cancel_registry.trigger(&cancel_id);
                inner.process.as_ref().map(|p| p.stdin_tx.clone())
            };
            if let Some(stdin_tx) = stdin_tx {
                // 独立 task 中可靠发送，避免阻塞 supervisor 主循环
                tokio::spawn(async move {
                    if let Err(e) = stdin_tx
                        .send(IpcMessage::Cancel(CancelNotification { cancel: cancel_id }))
                        .await
                    {
                        tracing::warn!("发送取消通知失败: {e}");
                    }
                });
            }
        }
        SupervisorCommand::Shutdown => {
            handle_shutdown(this).await;
        }
        SupervisorCommand::IdleTimeout => {
            handle_idle_timeout(this).await;
        }
    }
}

/// 处理 Worker 回传的 IPC 消息
async fn handle_ipc_message(this: &Arc<BridgeSupervisor>, msg: ParsedMessage) {
    match msg {
        ParsedMessage::Response(resp) => {
            let tx = this.inner.lock().unwrap_or_else(|e| e.into_inner()).pending_requests.remove(&resp.id);
            if let Some(tx) = tx {
                // oneshot::send 是同步操作，不会阻塞 supervisor 主循环
                let _ = tx.send(Ok(resp));
            } else {
                tracing::warn!(target: "python_worker", "收到过期/未知响应 id={}", resp.id);
            }
        }
        ParsedMessage::ResponseError { id, error } => {
            // 带有效 id 但反序列化失败：以内部错误回收对应在途请求，防止永久泄漏
            // 与调试会话槽位卡死（历史遗留 F1）
            let tx = this
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_requests
                .remove(&id);
            if let Some(tx) = tx {
                let _ = tx.send(Err(BridgeError::Internal(format!(
                    "IPC 响应解析失败: {error}"
                ))));
            } else {
                tracing::warn!(
                    target: "python_worker",
                    "收到无法解析的响应且无对应在途请求 id={id}: {error}"
                );
            }
        }
        ParsedMessage::Event(ev) => {
            // 事件（step_progress/screenshot/ocr_result/dialog）转发到 WebSocket 日志流
            debug!(target: "python_worker", "event {}: {:?}", ev.event, ev.data);
            if matches!(
                ev.event.as_str(),
                "screenshot" | "step_progress" | "ocr_result" | "dialog"
            ) {
                if let Some(tx) = this.event_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    let payload = json!({ "type": ev.event, "data": ev.data });
                    if let Ok(s) = serde_json::to_string(&payload) {
                        let _ = tx.send(s);
                    }
                }
            }
        }
        ParsedMessage::InvalidLine(s) => {
            tracing::warn!(target: "python_worker", "IPC 非法行: {s}");
        }
        ParsedMessage::WorkerExited(code) => {
            handle_worker_exited(this, code).await;
        }
    }
}

/// execute 核心逻辑：确保 Worker 就绪 → 注册 cancel → 发送请求
///
/// 返回 `oneshot::Receiver` 供调用方等待响应、`SessionGuard` 用于 RAII 清理，
/// 以及 `CancellationToken` 供调用方在等待响应时监听取消（与 CancelRegistry 共享同一令牌）。
/// 不在 supervisor 主循环内阻塞等待 IPC 响应，避免死锁。
async fn execute_inner(
    this: &Arc<BridgeSupervisor>,
    method: &str,
    params: Value,
) -> Result<
    (
        oneshot::Receiver<Result<IpcResponse, BridgeError>>,
        SessionGuard,
        CancellationToken,
    ),
    BridgeError,
>
{
    // 1. 懒加载 Worker（环境就绪则 spawn）
    ensure_worker(this).await?;

    // 2. 会话类型（debug_* 为调试会话，其余为登录/浏览器任务）
    let session = if method.starts_with("debug_") {
        SessionType::Debug
    } else {
        SessionType::Login
    };
    // OCR 轻量请求：与任意会话并发（见 check_session_compat），不占用单会话槽位，
    // 也不触碰 current_session / worker_state / 空闲计时器（P1-6）。
    let is_ocr = method == "ocr_recognize";

    // 3. 生成 cancel_id + CancellationToken
    // 优先使用调用方传入的 cancel_id（如 LoginSession / OCR 通过 params["cancel_id"] 传入），
    // 保证 CancelRegistry.trigger(调用方 cancel_id) 能命中本请求注册的 token；
    // params 中无 cancel_id 时自行生成（保持向后兼容）。
    let cancel_id = params
        .get("cancel_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let token = CancellationToken::new();

    // 4. 单一原子临界区：互斥检查 → 分配 id → 注册 pending → 发送请求 → 注册 cancel/设置会话。
    // 全程同步操作，一次加锁完成，消除各步骤之间的窗口。原实现分 6 次独立加解锁，
    // “compat 检查”与“会话赋值”不原子，两个并发请求可能都通过互斥检查后各自赋值会话；
    // Cancel / reset_session / handle_ipc_message 均锁同一 Mutex，故本临界区与它们严格串行。
    let (resp_rx, request_id) = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());

        // 4.1 互斥检查（失败立即返回，此时尚未分配任何资源，无泄漏）
        check_session_compat(inner.current_session, method)?;

        // 4.2 分配 request id 并注册 pending 响应通道
        let request_id = inner.next_request_id;
        inner.next_request_id += 1;
        let (resp_tx, resp_rx) = oneshot::channel::<Result<IpcResponse, BridgeError>>();
        inner.pending_requests.insert(request_id, resp_tx);

        // 4.3 发送请求（可失败）。失败则回滚 pending；此时会话/取消状态尚未改动，无需额外回滚。
        match &inner.process {
            Some(proc) => {
                if let Err(e) = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
                    id: request_id,
                    method: method.to_string(),
                    params,
                })) {
                    inner.pending_requests.remove(&request_id);
                    return Err(BridgeError::IpcWriteError(std::io::Error::other(
                        format!("IPC channel send failed: {e}"),
                    )));
                }
            }
            None => {
                inner.pending_requests.remove(&request_id);
                return Err(BridgeError::SupervisorNotRunning);
            }
        }

        // 4.4 发送成功后再改动会话/取消状态（此后不再有可失败操作）。
        // 先注册新 cancel token，再清理旧会话残留的 cancel_id（如 InLogin 时 debug_start
        // 覆盖 current_session，旧 Login 的 cancel_id 不再被追踪）。仅当新旧不同才移除，
        // 避免调用方复用同一 cancel_id 时误删刚注册的 token。
        inner.cancel_registry.register(cancel_id.clone(), token.clone());
        if is_ocr {
            // OCR 轻量旁路：仅注册 cancel，不触碰会话槽位 / worker_state / 空闲计时器，
            // 也不移除旧会话的 cancel_id（OCR 与任意会话并发，绝不清他人注册）。
            // pending 与 cancel 的清理交给 guard drop 的轻量回调（lightweight_cleanup）。
        } else {
            if let Some(old_cancel_id) = inner.current_cancel_id.take() {
                if old_cancel_id != cancel_id {
                    inner.cancel_registry.remove(&old_cancel_id);
                }
            }
            inner.worker_state = if session == SessionType::Debug {
                WorkerState::InDebug
            } else {
                WorkerState::InLogin
            };
            inner.current_session = Some(session);
            inner.current_cancel_id = Some(cancel_id.clone());
            inner.current_request_id = Some(request_id);
            inner.last_activity = Instant::now();
            if let Some(h) = inner.idle_timer.take() {
                h.abort();
            }
            merge_worker_status(&inner, &this.status);
        }
        (resp_rx, request_id)
    };

    // RAII 守卫：drop 时复位会话状态并启动空闲计时器。drop 会再次加锁，故在临界区外创建。
    // 携带 request_id：reset_session 仅在当前会话仍为本请求时才复位，避免已结束会话的
    // 延迟 drop 误清刚启动的同类型新会话的 pending/cancel。
    // OCR 请求使用轻量守卫：drop 只清自身 pending 与 cancel 注册，绝不 reset_session
    // （否则会把并发登录会话的槽位复位为 Idle 并提前启动空闲计时器，见 5.1）。
    let guard = if is_ocr {
        SessionGuard::new({
            let weak = this.self_weak.clone();
            let cancel_id = cancel_id.clone();
            move || {
                if let Some(sup) = weak.upgrade() {
                    lightweight_cleanup(&sup, request_id, &cancel_id);
                }
            }
        })
    } else {
        SessionGuard::new({
            let weak = this.self_weak.clone();
            move || {
                if let Some(sup) = weak.upgrade() {
                    reset_session(&sup, session, request_id);
                }
            }
        })
    };

    // 8. 返回 oneshot::Receiver、SessionGuard 与 CancellationToken，由调用方（转发 task）等待响应
    Ok((resp_rx, guard, token))
}

/// 会话守卫 drop 时复位状态
///
/// 仅当当前会话仍为本守卫拥有的那个（`session` 与 `request_id` 双重匹配）时才复位。
/// 若已被更新的同类型会话取代（request_id 不同），则跳过，避免误清新会话的
/// pending/cancel（历史遗留：快速连续 execute 时旧会话延迟 drop 会话竞态）。
fn reset_session(this: &Arc<BridgeSupervisor>, session: SessionType, request_id: u64) {
    let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
    if inner.current_session != Some(session) || inner.current_request_id != Some(request_id) {
        return;
    }
    inner.current_session = None;
    // 清理 CancelRegistry 防止内存泄漏
    if let Some(cancel_id) = inner.current_cancel_id.take() {
        inner.cancel_registry.remove(&cancel_id);
    }
    if let Some(id) = inner.current_request_id.take() {
        inner.pending_requests.remove(&id);
    }
    inner.worker_state = WorkerState::Idle;
    inner.last_activity = Instant::now();
    merge_worker_status(&inner, &this.status);
    // 启动空闲回收计时器
    start_idle_timer(this, &mut inner);
}

/// OCR 轻量请求的守卫 drop 清理：只移除自身 pending 与 cancel 注册，**不**复位会话槽位 /
/// 启动空闲计时器 / 改动 worker_state。仅当 pending 仍为本请求时移除（幂等：响应到达后
/// handle_ipc_message 已移除，此处为 no-op；取消/Cancel 路径则在此清理残留）。
fn lightweight_cleanup(this: &BridgeSupervisor, request_id: u64, cancel_id: &str) {
    let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
    inner.pending_requests.remove(&request_id);
    inner.cancel_registry.remove(cancel_id);
}

/// 启动空闲计时器：超时后发送 IdleTimeout
///
/// `keep_alive=true` 时跳过计时器（Worker 常驻）；否则使用配置 `worker.idle_timeout_seconds`。
fn start_idle_timer(this: &BridgeSupervisor, inner: &mut BridgeInner) {
    // keep_alive 策略：保持 Worker 存活，不启动空闲回收计时器
    let cfg = this.config.runtime().load();
    if cfg.worker.keep_alive {
        debug!("worker.keep_alive 启用，跳过空闲回收计时器");
        return;
    }
    let idle = (cfg.worker.idle_timeout_seconds as u64).max(1);
    // 从真实最后活动时刻起算剩余空闲时间：计时器启动若有延迟（如调用方在锁外排队），
    // 仍保证总共空闲满 `idle` 秒，避免实际空闲被压缩（last_activity 的唯一读取点）。
    let elapsed = inner.last_activity.elapsed().as_secs();
    let remaining = idle.saturating_sub(elapsed).max(1);
    let cmd_tx = this.cmd_tx.clone();
    // 订阅停止信号，确保 supervisor 关闭时计时器能及时退出，而非继续 sleep
    let stop_rx = this
        .service_handle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|tx| tx.subscribe());
    let handle = tokio::spawn(async move {
        let sleep = tokio::time::sleep(Duration::from_secs(remaining));
        tokio::pin!(sleep);
        match stop_rx {
            Some(mut rx) => {
                tokio::select! {
                    _ = &mut sleep => {
                        let _ = cmd_tx.send(SupervisorCommand::IdleTimeout).await;
                    }
                    _ = rx.changed() => {
                        // 收到停止信号（或 sender drop），提前退出计时器
                    }
                }
            }
            None => {
                // spawn 尚未调用（无 stop sender），回退为纯 sleep
                sleep.await;
                let _ = cmd_tx.send(SupervisorCommand::IdleTimeout).await;
            }
        }
    });
    inner.idle_timer = Some(handle);
}

/// 确保 Worker 就绪：按需懒加载 spawn + 健康检查
///
/// 串行化启动过程（同一时刻仅一个协程执行 spawn+健康检查），避免并发重复 spawn。
/// spawn 成功后发送 `browser_health_check` 并等待就绪，超时则强杀子进程返回
/// [`BridgeError::WorkerStartupTimeout`]。启动时与崩溃恢复时均清理孤儿浏览器进程。
/// 连续失败次数超阈值（B3）后直接返回 [`BridgeError::WorkerSpawnBlocked`]。
async fn ensure_worker(this: &Arc<BridgeSupervisor>) -> Result<(), BridgeError> {
    // 快速路径：已就绪
    if is_worker_ready(this) {
        return Ok(());
    }
    // 熔断检查：连续 spawn 失败 ≥3 次，快速失败（B3）
    {
        let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.consecutive_spawn_failures >= SPAWN_FAILURE_THRESHOLD {
            tracing::warn!(target: "python_worker",
                "Worker 连续 {} 次启动失败，触发熔断",
                inner.consecutive_spawn_failures
            );
            return Err(BridgeError::WorkerSpawnBlocked);
        }
    }
    // 串行化启动：持有 startup_lock 期间其他调用方阻塞，解锁后重新检查快速路径
    let _startup_guard = this.startup_lock.lock().await;
    // 双重检查（获取锁后可能已被其他协程启动完成）
    if is_worker_ready(this) {
        return Ok(());
    }
    // 进程已存在（InLogin / InDebug 等活跃会话），无需重复 spawn
    {
        let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.process.is_some() {
            return Ok(());
        }
    }
    // 校验 Python 解释器是否存在
    let python_exe = this.base_path.join(WORKER_PROJECT_DIR).join(PYTHON_EXE_RELATIVE);
    let worker_main = this
        .base_path
        .join(WORKER_PROJECT_DIR)
        .join("worker_main.py");
    if !python_exe.exists() {
        return Err(BridgeError::WorkerNotInstalled);
    }
    // 清理上次崩溃残留的孤儿浏览器进程
    // 同步 /proc 或 powershell 进程枚举，用 spawn_blocking 避免阻塞 async 运行时
    if let Err(e) = tokio::task::spawn_blocking(orphan::cleanup_orphan_browsers).await {
        tracing::warn!("孤儿浏览器清理任务失败: {e}");
    }
    // spawn 子进程 + 四个后台 task
    let ipc_tx = this
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ipc_tx
        .clone()
        .ok_or(BridgeError::WorkerStartupTimeout)?;
    let process = spawn_worker(&python_exe, &worker_main, &this.base_path, ipc_tx).await?;
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.worker_state = WorkerState::Starting;
        inner.process = Some(process);
        merge_worker_status(&inner, &this.status);
    }
    // Worker spawn 成功，递增指标
    if let Some(m) = &this.metrics {
        m.inc_worker_spawn();
    }
    // 发送 browser_health_check 验证就绪，超时则强杀并标记错误
    match send_health_check(this).await {
        Ok(true) => {
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            // 成功启动后复位连续失败计数（B3）
            inner.consecutive_spawn_failures = 0;
            merge_worker_status(&inner, &this.status);
            info!(target: "python_worker", "Worker 健康检查通过，已就绪");
            Ok(())
        }
        _ => {
            warn!(target: "python_worker", "Worker 健康检查失败或超时");
            kill_worker_now(this).await;
            // 递增连续失败计数（B3）
            {
                let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.consecutive_spawn_failures += 1;
                merge_worker_status(&inner, &this.status);
            }
            Err(BridgeError::WorkerStartupTimeout)
        }
    }
}

/// 判断 Worker 是否就绪（Idle 且子进程存活）
fn is_worker_ready(this: &BridgeSupervisor) -> bool {
    let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
    matches!(inner.worker_state, WorkerState::Idle) && inner.process.is_some()
}

/// 发送 `browser_health_check` 并等待响应（带启动超时）
///
/// 复用 `pending_requests` 的 oneshot 通道路由 Worker 响应；本函数不阻塞 supervisor
/// 主循环（由调用方在独立 task 中 await），响应经主循环 `handle_ipc_message` 投递。
async fn send_health_check(this: &BridgeSupervisor) -> Result<bool, BridgeError> {
    let request_id = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = inner.next_request_id;
        inner.next_request_id += 1;
        id
    };
    let (resp_tx, resp_rx) = oneshot::channel::<Result<IpcResponse, BridgeError>>();
    this.inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_requests
        .insert(request_id, resp_tx);
    // 发送健康检查请求（携带 browser_settings 供 Worker 预初始化）
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &inner.process {
            Some(proc) => {
                let cfg = this.config.runtime().load();
                let mut params = Value::Null;
                if let Ok(bs) = serde_json::to_value(&cfg.browser) {
                    params = json!({ "browser_settings": bs });
                }
                if let Err(e) = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
                    id: request_id,
                    method: "browser_health_check".to_string(),
                    params,
                })) {
                    inner.pending_requests.remove(&request_id);
                    return Err(BridgeError::IpcWriteError(std::io::Error::other(
                        format!("IPC channel send failed: {e}"),
                    )));
                }
            }
            None => {
                inner.pending_requests.remove(&request_id);
                return Err(BridgeError::SupervisorNotRunning);
            }
        }
    }
    // 等待响应（带启动超时）
    // recv 返回 Result<Result<IpcResponse, BridgeError>, RecvError>，
    // 叠加 timeout 的 Elapsed，共三层 Result，逐层归一为 WorkerStartupTimeout
    let resp = match timeout(
        Duration::from_secs(DEFAULT_WORKER_STARTUP_TIMEOUT_SECS),
        resp_rx,
    )
    .await
    {
        Err(_) | Ok(Err(_)) => return Err(BridgeError::WorkerStartupTimeout),
        Ok(Ok(r)) => r,
    };
    let resp = resp.map_err(|_| BridgeError::WorkerStartupTimeout)?;
    // 健康检查成功且 Worker 报告浏览器可用
    Ok(resp.result.success
        && resp
            .result
            .data
            .get("healthy")
            .and_then(Value::as_bool)
            .unwrap_or(false))
}

/// 强杀当前 Worker 子进程并标记 Error（不清理 cancel 注册表/会话）
async fn kill_worker_now(this: &BridgeSupervisor) {
    let proc = this.inner.lock().unwrap_or_else(|e| e.into_inner()).process.take();
    if let Some(p) = proc {
        // 先尝试优雅关闭，超时则由 shutdown 内部强杀
        let _ = p.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
            id: SHUTDOWN_REQUEST_ID,
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        // 仅依赖 p.shutdown 内部的 timeout，避免外层再包 timeout 导致可达 2 倍超时
        p.shutdown(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)).await;
    }
    // 进程已终止，在途请求不可能再得到响应：立即 drain 并结算，
    // 否则对应 execute_inner 要挂满 300s 超时才返回（且只能拿到 Timeout 错误）
    drain_pending_requests(this, "worker killed by supervisor");
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.worker_state = WorkerState::Error;
        merge_worker_status(&inner, &this.status);
    }
}

/// 优雅关闭 Worker（shutdown 命令 → 等超时 → kill）
async fn handle_shutdown(this: &Arc<BridgeSupervisor>) {
    let process = this.inner.lock().unwrap_or_else(|e| e.into_inner()).process.take();
    if let Some(proc) = process {
        // 先发送 shutdown 命令，等待 Worker 自行退出
        let _ = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
            id: SHUTDOWN_REQUEST_ID,
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        // 仅依赖 proc.shutdown 内部的 timeout，避免外层再包 timeout 导致可达 2 倍超时
        proc.shutdown(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)).await;
    }
    // 与 kill_worker_now 同理：进程已回收，drain 在途请求避免悬挂至超时
    drain_pending_requests(this, "worker shut down");
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.worker_state = WorkerState::Idle;
        merge_worker_status(&inner, &this.status);
    }
}

/// Drain 所有在途请求并以 `WorkerCrashed` 结算
///
/// 进程已终止（正常退出/主动关闭/强杀）时，pending 请求永无响应，
/// 必须主动结算，否则调用方要挂满 `execute_with_timeout` 的超时才返回
fn drain_pending_requests(this: &BridgeSupervisor, reason: &str) {
    let pending: Vec<_> = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.pending_requests.drain().map(|(_, tx)| tx).collect()
    };
    for tx in pending {
        let _ = tx.send(Err(BridgeError::WorkerCrashed {
            reason: reason.to_string(),
        }));
    }
}

/// 空闲计时器触发：若仍处于 Idle 则回收 Worker
async fn handle_idle_timeout(this: &Arc<BridgeSupervisor>) {
    let should_shutdown = {
        let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        matches!(inner.worker_state, WorkerState::Idle) && inner.process.is_some()
    };
    if should_shutdown {
        handle_shutdown(this).await;
    }
}

/// Worker 退出处理：区分正常退出（exit_code=0）与崩溃
async fn handle_worker_exited(this: &Arc<BridgeSupervisor>, code: i32) {
    // 汇总本 Worker 生命周期内的非 JSON IPC 行（stdout 被意外 print 污染的信号）
    let invalid_lines = process::take_invalid_ipc_line_count();
    if invalid_lines > 0 {
        warn!(
            target: "python_worker",
            "Worker 运行期间累计 {invalid_lines} 行非 JSON IPC 输出（stdout 疑似被第三方库污染），相关请求可能已超时"
        );
    }
    // 正常退出（空闲回收 / 用户主动停止 / shutdown）：不记为崩溃，不计入指标，
    // 不触发孤儿清理，也不置 Error。仅 drain pending 作为防御（正常退出时不应有在途请求）。
    if code == 0 {
        info!(target: "python_worker", "Worker 正常退出，exit_code=0");
        drain_pending_requests(this, "worker exited (code 0)");
        {
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            merge_worker_status(&inner, &this.status);
        }
        return;
    }
    warn!(target: "python_worker", "Worker 进程退出（崩溃），exit_code={code}");
    // 崩溃恢复时清理可能残留的孤儿浏览器进程
    // 同步 /proc 或 powershell 进程枚举，用 spawn_blocking 避免阻塞 async 运行时
    if let Err(e) = tokio::task::spawn_blocking(orphan::cleanup_orphan_browsers).await {
        tracing::warn!("孤儿浏览器清理任务失败: {e}");
    }
    // Worker 崩溃，递增指标
    if let Some(m) = &this.metrics {
        m.inc_worker_crash();
    }
    let (pending, handles, crashed_session) = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pending: Vec<_> = inner.pending_requests.drain().map(|(_, tx)| tx).collect();
        // 崩溃时以 pending 通道向在途请求送达定性错误（WorkerCrashed / DebugSessionClosed），
        // 此处仅清空 token 注册表而不触发取消：避免与 pending 送达形成 select! 竞态导致崩溃
        // 请求非确定地报 Cancelled（Cancelled 语义保留给显式 cancel）。
        inner.cancel_registry.clear();
        // 捕获崩溃时所在的会话类型，用于区分 DebugSessionClosed / WorkerCrashed
        let crashed_session = inner.current_session.take();
        inner.current_cancel_id = None;
        inner.current_request_id = None;
        let handles = inner.process.take().map(|p| p.handles);
        inner.worker_state = WorkerState::Error;
        merge_worker_status(&inner, &this.status);
        (pending, handles, crashed_session)
    };
    for tx in pending {
        // 调试会话期间崩溃 → 关联调用方收到 DebugSessionClosed；其余收到 WorkerCrashed
        let err = match crashed_session {
            Some(SessionType::Debug) => BridgeError::DebugSessionClosed,
            _ => BridgeError::WorkerCrashed {
                reason: format!("exit_code={code}"),
            },
        };
        // oneshot::send 是同步操作
        let _ = tx.send(Err(err));
    }
    if let Some(h) = handles {
        h.stdin_task.abort();
        h.stdout_task.abort();
        h.stderr_task.abort();
        h.health_task.abort();
    }
    // 调试会话因崩溃被强制终止：通知 WebSocket 日志流
    if crashed_session == Some(SessionType::Debug) {
        if let Some(tx) = this.event_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let payload = json!({ "type": "debug_session_closed", "data": { "reason": "worker_crashed" } });
            if let Ok(s) = serde_json::to_string(&payload) {
                let _ = tx.send(s);
            }
        }
        warn!(target: "python_worker", "Worker 崩溃，调试会话被强制终止");
    }
}

/// 将当前 WorkerState 合并到 StatusManager 快照
///
/// 注意：调用方必须**已持有** `inner` 锁（或确保无锁竞争），本函数不再自行加锁。
/// 原实现内部 `this.inner.lock()` 会在调用方已持锁时导致 std::sync::Mutex 死锁。
fn merge_worker_status(inner: &BridgeInner, status: &StatusManager) {
    let state = worker_state_to_status(inner.worker_state, inner.process.is_some());
    status.merge(PartialSnapshot::Worker { state });
}

/// 会话互斥矩阵：判断新请求 `method` 与当前活跃会话 `current` 是否兼容。
///
/// 兼容（允许继续）：
/// - 无活跃会话：任意方法
/// - InLogin + (execute_login/browser_task/debug_start)：FIFO 排队
/// - InDebug + (debug_step/debug_stop/debug_run_all)：FIFO 排队
///
/// 不兼容（快速失败 [`BridgeError::WorkerBusy`]）：
/// - InLogin + (debug_step/debug_stop)
/// - InDebug + (execute_login/browser_task)：登录请求快速失败
/// - InDebug + debug_start：已有调试会话
///
/// `ocr_recognize` 轻量且单线程串行，允许与任意会话并发。
fn check_session_compat(current: Option<SessionType>, method: &str) -> Result<(), BridgeError> {
    if method == "ocr_recognize" {
        return Ok(());
    }
    match current {
        None => Ok(()),
        Some(SessionType::Login) => match method {
            "debug_step" | "debug_stop" => Err(BridgeError::WorkerBusy),
            _ => Ok(()),
        },
        Some(SessionType::Debug) => {
            if method.starts_with("debug_") {
                match method {
                    "debug_step" | "debug_stop" | "debug_run_all" => Ok(()),
                    _ => Err(BridgeError::WorkerBusy),
                }
            } else {
                Err(BridgeError::WorkerBusy)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言 `check_session_compat` 在给定会话与方法下返回兼容（Ok）
    fn assert_ok(current: Option<SessionType>, method: &str) {
        assert!(
            check_session_compat(current, method).is_ok(),
            "{current:?} + {method} 应兼容",
        );
    }

    /// 断言 `check_session_compat` 在给定会话与方法下返回 [`BridgeError::WorkerBusy`]
    fn assert_busy(current: Option<SessionType>, method: &str) {
        assert!(
            matches!(
                check_session_compat(current, method),
                Err(BridgeError::WorkerBusy)
            ),
            "{current:?} + {method} 应忙碌",
        );
    }

    #[test]
    fn 无会话_任意方法均兼容() {
        for m in [
            "execute_login",
            "browser_task",
            "debug_start",
            "debug_step",
            "debug_stop",
            "ocr_recognize",
        ] {
            assert_ok(None, m);
        }
    }

    #[test]
    fn 登录会话_允许登录与浏览器任务() {
        assert_ok(Some(SessionType::Login), "execute_login");
        assert_ok(Some(SessionType::Login), "browser_task");
    }

    #[test]
    fn 登录会话_允许启动调试会话() {
        // 计划矩阵：InLogin + debug_start 走 FIFO 排队
        assert_ok(Some(SessionType::Login), "debug_start");
    }

    #[test]
    fn 登录会话_调试步进与停止应忙碌() {
        assert_busy(Some(SessionType::Login), "debug_step");
        assert_busy(Some(SessionType::Login), "debug_stop");
    }

    #[test]
    fn 调试会话_允许步进与停止() {
        assert_ok(Some(SessionType::Debug), "debug_step");
        assert_ok(Some(SessionType::Debug), "debug_stop");
        assert_ok(Some(SessionType::Debug), "debug_run_all");
    }

    #[test]
    fn 调试会话_再次启动调试应忙碌() {
        assert_busy(Some(SessionType::Debug), "debug_start");
    }

    #[test]
    fn 调试会话_登录与浏览器任务应忙碌() {
        assert_busy(Some(SessionType::Debug), "execute_login");
        assert_busy(Some(SessionType::Debug), "browser_task");
    }

    #[test]
    fn ocr识别_与任意会话并发兼容() {
        assert_ok(None, "ocr_recognize");
        assert_ok(Some(SessionType::Login), "ocr_recognize");
        assert_ok(Some(SessionType::Debug), "ocr_recognize");
    }

    /// 5.1：OCR 轻量守卫 drop 只清理自身，保留并发登录会话槽位。
    ///
    /// 模拟：登录会话占住单槽位时，一个 OCR 请求注册了自己的 pending 与 cancel；
    /// OCR 结束（guard drop）后，会话槽位 / worker_state 应保持不变，登录会话的
    /// pending 与 cancel 注册项应原样保留（旧实现会走 reset_session 把登录会话复位为
    /// Idle 并提前启动空闲计时器，导致在途登录以 WorkerCrashed 失败）。
    #[tokio::test]
    async fn ocr_轻量守卫drop_保留并发登录槽位() {
        use tokio::sync::oneshot;

        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = crate::config::ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        let status = Arc::new(crate::status::StatusManager::new());
        let bridge = BridgeSupervisor::new(dir.path().to_path_buf(), config, status, None);

        // 预置并发登录会话 + OCR 请求自身的注册项
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.current_session = Some(SessionType::Login);
            inner.worker_state = WorkerState::InLogin;
            inner.current_cancel_id = Some("login".to_string());
            inner.current_request_id = Some(1);
            let (tx, _rx) = oneshot::channel();
            inner.pending_requests.insert(1, tx);
            inner
                .cancel_registry
                .register("login".to_string(), CancellationToken::new());
            let (tx, _rx) = oneshot::channel();
            inner.pending_requests.insert(100, tx);
            inner
                .cancel_registry
                .register("ocr".to_string(), CancellationToken::new());
        }

        // 构造 OCR 轻量守卫并 drop（模拟 OCR 请求结束）
        let weak = bridge.self_weak.clone();
        let guard = SessionGuard::new({
            let weak = weak.clone();
            let cancel_id = "ocr".to_string();
            move || {
                if let Some(sup) = weak.upgrade() {
                    lightweight_cleanup(&sup, 100, &cancel_id);
                }
            }
        });
        drop(guard);

        let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
        // 会话槽位与 worker_state 保持登录态不变
        assert_eq!(inner.current_session, Some(SessionType::Login));
        assert_eq!(inner.current_request_id, Some(1));
        assert_eq!(inner.worker_state, WorkerState::InLogin);
        // OCR 自身 pending 与 cancel 已清理；登录会话的注册项保留
        assert!(!inner.pending_requests.contains_key(&100));
        assert!(inner.pending_requests.contains_key(&1));
        assert!(!inner.cancel_registry.contains("ocr"));
        assert!(inner.cancel_registry.contains("login"));
    }

    /// B3：连续 spawn 失败熔断——达到阈值后 ensure_worker 快速失败而非再等 30s。
    #[tokio::test]
    async fn test_ensure_worker_blocks_after_spawn_failures() {
        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = crate::config::ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        let status = Arc::new(crate::status::StatusManager::new());
        let bridge = BridgeSupervisor::new(dir.path().to_path_buf(), config, status, None);
        // 模拟已连续失败 3 次（达到熔断阈值）
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.consecutive_spawn_failures = SPAWN_FAILURE_THRESHOLD;
        }
        // 第 4 次调用快速返回 WorkerSpawnBlocked，而非再等 30s 健康检查超时
        let start = Instant::now();
        let result = ensure_worker(&bridge).await;
        assert!(
            matches!(result, Err(BridgeError::WorkerSpawnBlocked)),
            "熔断后应快速失败，得到 {result:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "熔断快速失败应在 5s 内返回"
        );

        // reset_spawn_failures 后计数复位
        bridge.reset_spawn_failures();
        {
            let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(inner.consecutive_spawn_failures, 0);
        }
    }
}
