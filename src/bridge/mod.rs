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
//!
//! ## 子模块划分
//!
//! - [`error`]：`BridgeError` 错误类型；
//! - [`state`]：Supervisor 内部状态（`BridgeInner`）与后台命令（`SupervisorCommand`）；
//! - [`api`]：Web 层消费的 `BridgeApi` trait 及 Supervisor 实现；
//! - [`debug`]：调试会话开合结算、守卫清理与孤儿 TTL；
//! - [`lifecycle`]：Worker 进程生命周期（spawn / 健康检查 / 强杀 / 关闭 / 退出 / 空闲）；
//! - [`session`]：会话互斥矩阵 + cancel_id 注册表 + 会话守卫；
//! - [`process`] / [`ipc`] / [`orphan`] / [`job`] / [`worker`]：子进程与 IPC 执行侧。
//!
//! 本文件保留 Supervisor 主体与编排主流程（主循环 / 命令分发 / IPC 路由 /
//! execute 临界区）。

pub mod ipc;
#[cfg(windows)]
pub mod job;
pub mod orphan;
pub mod process;
pub mod session;
pub mod worker;

mod api;
mod debug;
mod error;
mod lifecycle;
mod state;
#[cfg(test)]
mod test_support;

pub use api::BridgeApi;
pub use error::BridgeError;
pub use ipc::{
    CancelNotification, IpcEvent, IpcRequest, IpcResponse, IpcResult, Outcome, StructuredResult,
};
pub use process::{IpcMessage, ParsedMessage, ProcessHandles, WorkerProcess, spawn_worker};
pub use session::{
    CancelRegistry, ConfigSessionOwner, ConfigSessionRegistry, SessionGuard, SessionType,
};
pub use worker::{WorkerState, worker_state_to_status};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::config::ConfigService;
use crate::environment::EnvironmentManager;
use crate::status::{PartialSnapshot, StatusManager, WorkerStatus};
use crate::utils::metrics::Metrics;
use crate::utils::paths::worker_project_dir;

use debug::{DebugSettle, debug_guard_cleanup, debug_session_stale, execute_debug_settle};
use lifecycle::{
    ensure_worker, handle_idle_timeout, handle_shutdown, handle_worker_exited, kill_worker_now,
    start_idle_timer,
};
use session::{check_session_compat, is_lightweight_method};
use state::{BridgeInner, SupervisorCommand};

/// Worker 空闲回收默认阈值（秒）
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// 发送 shutdown 命令后等待优雅退出的超时（秒）
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 5;
/// spawn 后等待 browser_health_check 通过的超时（秒）
pub const DEFAULT_WORKER_STARTUP_TIMEOUT_SECS: u64 = 30;
/// 取消通知发出后等待 Worker 确认收敛的最长时间；超时则回收进程。
const CANCEL_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// 持久自动登录命令（`profile_auto_login`）的命令级超时预算。
///
/// Worker 契约要求 `rust_timeout_ms` ≥45s（回填沉降 ~6.5s + 逐动作回放，
/// 每步预算为 `browser_settings.timeout`，编排侧已抬到 ≥45s 下限）；此处取
/// 登录流程既有默认预算 300s 对齐，实际外界由会话总超时（`login_timeout`，
/// 默认 120s）经 cancel_id 取消兜底，命令级预算只防 Worker 侧自愈失灵。
pub const PERSISTENT_AUTO_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 配置会话启动命令（`profile_session_start`）的命令级超时预算。
///
/// 覆盖「确保 Worker → headed 持久浏览器真实拉起 → 门户页导航 → 录制器注入」
/// 整段：冷启动含 Worker spawn + 健康检查 + Playwright/浏览器启动，取 90s
/// 宽裕值。会话**存续期**不在此预算内（Worker 契约：对齐 debug session，
/// start 返回后用户手动登录可停留任意久，stop 命令另有预算）。
pub const CONFIG_SESSION_START_TIMEOUT: Duration = Duration::from_secs(90);
/// 配置会话停止命令（`profile_session_stop`）的命令级超时预算：关闭持久浏览器
/// 与独占 Playwright 连接 + 汇聚录制结果，纯收尾操作取 30s。
pub const CONFIG_SESSION_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// 命令级超时后，等待 Worker 自愈（取消任务 + 关页打断挂起的 CDP await）的宽限期。
///
/// 仍未释放会话槽位则判定卡死并强杀回收。该值与 `login::PREEMPT_WAIT_BUDGET`
/// 的推导直接相关（后者须覆盖 close_browser 超时 + 本宽限期），改动时须同步。
pub const GRACE_WAIT_DURATION: Duration = Duration::from_secs(10);
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

pub use crate::ServiceHandle;

/// Python Bridge 公共入口
pub struct BridgeSupervisor {
    /// 浏览器当前出口租约；跨 IPC 命令保活，禁止保留无主转发监听器。
    browser_egress: Mutex<Option<Arc<crate::network::egress::PreparedEgress>>>,
    inner: Mutex<BridgeInner>,
    config: Arc<ConfigService>,
    status: Arc<StatusManager>,
    base_path: PathBuf,
    /// python_worker 工程目录（与 EnvironmentManager 同一解析，含 dev 模式仓库根回退）
    worker_project_dir: PathBuf,
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
    /// 环境管理器弱引用：Worker spawn 前做真实门禁，失败时在同一请求内自愈。
    environment: RwLock<Option<Weak<EnvironmentManager>>>,
    /// 孤儿清理已执行标记（A-4 降频）：PowerShell/CIM 进程枚举冷启动可达秒级，
    /// 仅在 Supervisor 生命周期首次 spawn 前执行一次；崩溃路径不受此门控
    orphan_cleanup_done: std::sync::atomic::AtomicBool,
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
        // 与 EnvironmentManager 共用 `utils::paths::worker_project_dir`（dev 模式回退仓库根），
        // 避免 spawn 检查与环境就绪判定各说各话（曾导致 cargo run 下误报"Worker 环境未安装"）
        let worker_project_dir = worker_project_dir(&base_path);
        Arc::new_cyclic(|weak| Self {
            browser_egress: Mutex::new(None),
            orphan_cleanup_done: std::sync::atomic::AtomicBool::new(false),
            inner: Mutex::new(BridgeInner {
                debug_session_open: false,
                debug_last_activity: None,
                last_screenshot_url: None,
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
                worker_capabilities: None,
                pending_starts: HashMap::new(),
            }),
            config,
            status,
            base_path,
            worker_project_dir,
            cmd_tx,
            cmd_rx: Mutex::new(Some(cmd_rx)),
            service_handle: Mutex::new(None),
            self_weak: weak.clone(),
            metrics,
            event_tx: Mutex::new(None),
            startup_lock: tokio::sync::Mutex::new(()),
            environment: RwLock::new(None),
        })
    }

    /// 接入环境管理器；使用弱引用避免 ServiceContainer 形成强引用环。
    pub fn set_environment(&self, environment: &Arc<EnvironmentManager>) {
        *self
            .environment
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::downgrade(environment));
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
        // 命中本地已注册的 CancellationToken（见下），唤醒转发 task 并等待 Worker
        // 确认停止；无确认则强制回收，之后才 drop guard 释放槽位与 pending。
        let cancel_id = params
            .get("cancel_id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let id = Uuid::new_v4().to_string();
                if let Some(map) = params.as_object_mut() {
                    map.insert("cancel_id".to_string(), Value::String(id.clone()));
                } else {
                    // 非 Object 入参（如 Value::Null 健康检查）：包装为 Object 以承载 cancel_id，
                    // 保证内外 cancel_id 一致，超时 Cancel 才能命中已注册的 token
                    params =
                        serde_json::json!({ "cancel_id": id.clone(), "value": params.clone() });
                }
                id
            });
        // BRG-1：下传本次请求的超时预算（毫秒），Python 侧按 0.9 倍设置命令级
        // 自愈超时，保证先于 Rust 超时触发（否则长任务被 Python 固定地板提前
        // 中断，或预算平齐时走到 Rust 超时 → Cancel → 强杀的重路径）。旧 Worker
        // 忽略该字段按原公式兜底，双向向后兼容。
        if let Some(map) = params.as_object_mut() {
            map.insert(
                "rust_timeout_ms".to_string(),
                Value::from(timeout.as_millis() as u64),
            );
        }
        let (tx, rx) = mpsc::channel(1);
        let start_notify = Arc::new(tokio::sync::Notify::new());
        self.cmd_tx
            .send(SupervisorCommand::Execute {
                method: method.to_string(),
                params,
                response_tx: tx,
                start_notify: start_notify.clone(),
            })
            .await
            .map_err(|_| BridgeError::SupervisorNotRunning)?;
        // BRG-1b：截止时刻循环——截止时刻自「发送时刻」起算，但收到 Python 的
        // command_started 事件（该命令已出队并真正开始执行）后重置为
        // 「now + 完整预算」。修复：FIFO 排队时间此前会吃掉预算，排队命令超时
        // 后经 Cancel → 归属校验（发送时已 supersede 槽位，必然命中）→ 强杀，
        // 连带摧毁 Python 侧实际正在执行的前序长命令。旧 Worker 不发该事件 →
        // 截止时刻永不重置 → 行为与固定超时完全一致（双向向后兼容）。
        let result = recv_with_start_deadline(rx, start_notify, timeout).await;
        match result {
            // 超时返回前发送 Cancel：cancel_registry.trigger 立即唤醒本地 token，
            // 转发 task 等 Worker 确认停止或强制回收后再 drop guard，释放槽位与 pending。
            // 幂等：若请求恰在超时瞬间已响应/已取消，trigger 与 stdin 发送均为 no-op。
            Err(BridgeError::Timeout) => {
                self.cmd_tx
                    .send(SupervisorCommand::Cancel {
                        cancel_id: cancel_id.clone(),
                    })
                    .await
                    .map_err(|_| BridgeError::SupervisorNotRunning)?;
                // A1 自愈兜底：Cancel 后给 Worker 一段宽限（Python 侧命令超时 + 关闭
                // 页面自愈），等待会话槽位释放；仍未释放说明自愈失败，强杀回收，
                // 避免挂起命令永久占用会话槽位导致死锁。
                //
                // 归属校验（历史遗留 F2）：仅当超时时刻会话槽位确实由**本请求**
                // （current_cancel_id 与本请求 cancel_id 一致）占有时才进入宽限等待。
                // 槽位空闲（本请求已自愈，或本请求是 OCR 轻量旁路从不占槽位）或
                // 被其他请求持有（并发登录）时，本请求与槽位滞留无关，直接返回超时。
                let stuck_request_id = {
                    let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    (inner.current_cancel_id.as_deref() == Some(cancel_id.as_str()))
                        .then(|| inner.current_request_id)
                        .flatten()
                };
                if let Some(request_id) = stuck_request_id {
                    grace_wait_slot_release(self, request_id, GRACE_WAIT_DURATION).await;
                }
                Err(BridgeError::Timeout)
            }
            other => other,
        }
    }

    /// 触发跨进程取消：向 Worker stdin 发送 {"cancel": cancel_id}
    pub fn cancel(&self, cancel_id: &str) {
        let msg = SupervisorCommand::Cancel {
            cancel_id: cancel_id.to_string(),
        };
        match self.cmd_tx.try_send(msg) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(msg)) => {
                // 通道满（64）：取消通知不得静默丢弃，独立 task 阻塞投递。
                // 仅在 tokio 运行时内可 spawn；所有现行调用方均在 async 上下文。
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let cmd_tx = self.cmd_tx.clone();
                    handle.spawn(async move {
                        if let Err(e) = cmd_tx.send(msg).await {
                            debug!("发送取消命令失败（supervisor 可能已停止）: {e}");
                        }
                    });
                } else {
                    debug!("发送取消命令失败：无 tokio 运行时且命令通道已满");
                }
            }
            Err(e) => {
                debug!("发送取消命令失败（supervisor 可能已停止）: {e}");
            }
        }
    }

    /// 注入 WebSocket 事件广播通道（由 app 层在构建 Router 时调用）
    pub fn set_event_tx(&self, tx: broadcast::Sender<String>) {
        *self.event_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }
    /// 优雅关闭 Worker（shutdown 命令 → 等超时 → kill）
    pub async fn shutdown(&self) {
        if let Err(e) = self.cmd_tx.send(SupervisorCommand::Shutdown).await {
            debug!("发送 shutdown 命令失败（supervisor 可能已停止）: {e}");
        }
    }

    /// 强制回收 Worker：立即强杀子进程并复位状态
    ///
    /// 供 [`crate::login::LoginSession`] 在可重试结果触发 `should_force_recycle`
    /// （当前仅 `NetworkError`）时调用，强制回收可能已损坏的浏览器上下文。
    /// 注意 `UnknownError` 不走此路径：`classify` 将其归为终态失败，在
    /// `try_retry` 之前即 return，不触发回收。会清理会话与取消注册表。
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
        self.browser_egress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    /// 归属感知的强制回收：仅当会话槽位空闲或属于调用方时才强杀 Worker。
    ///
    /// 返回 `true` 表示已执行回收，`false` 表示因槽位被**其他**请求持有而跳过。
    ///
    /// 动机：`force_recycle` 是破坏性操作（kill 整棵进程树）。登录会话在可重试
    /// 失败后的回收、以及抢占等待超时后的兜底都会调用它；而定时浏览器任务与
    /// 登录在 Bridge 层**共享同一个会话槽位**且互不排斥（`check_session_compat`
    /// 对 `Some(Login)` 放行 `execute_browser_task`，有测试固化）。两者时间重叠时
    /// 无条件强杀会摧毁对方在途执行——对方经 `trigger_all` 收到 `Cancelled`
    /// （而非 `WorkerCrashed`：`force_recycle` 先取消会话区 token，在途请求的
    /// `select!` 分支胜出，用户可见「请求已取消」），症状是
    /// "任务自己失败"，根因却在登录路径上。
    ///
    /// 判定口径与 `grace_wait_slot_release` / `wait_cancel_ack_or_kill` 一致：
    /// 仅当 `current_cancel_id` 为空（槽位空闲）或不等于他人 id 时放行。
    /// `owner_cancel_id` 为 `None`（调用方无身份，如本轮 attempt 已结束）时，
    /// 槽位非空即视为他人占用而跳过——宁可少回收一次（下次 `ensure_worker`
    /// 会重新建立上下文），也不要误杀在途任务。
    pub async fn force_recycle_if_unowned(&self, owner_cancel_id: Option<&str>) -> bool {
        let unowned = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.current_cancel_id.as_deref() {
                // 槽位空闲：无人依赖当前 Worker
                None => true,
                // 槽位由本请求持有：回收是预期行为
                Some(current) => Some(current) == owner_cancel_id,
            }
        };
        if !unowned {
            return false;
        }
        self.force_recycle().await;
        true
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
            name: "bridge",
        };
        *self
            .service_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
        handle
    }

    /// 停止 supervisor task（ServiceHandle 模式）
    pub async fn stop(&self) {
        if let Some(tx) = self
            .service_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            && let Err(e) = tx.send(true)
        {
            debug!("发送停止信号失败（无活跃接收端）: {e}");
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
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .process
            .is_some()
    }

    /// 测试辅助：模拟「某请求正占用会话槽位」
    ///
    /// 供 [`BridgeSupervisor::force_recycle_if_unowned`] 的归属判定用例使用——
    /// 真实槽位只能由 `execute_inner` 的原子临界区写入，单测无从驱动完整 IPC。
    #[cfg(test)]
    pub(crate) fn register_test_cancel_id(&self, cancel_id: &str, token: CancellationToken) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.cancel_registry.register(cancel_id.to_string(), token);
        inner.current_cancel_id = Some(cancel_id.to_string());
        inner.current_session = Some(SessionType::Login);
        inner.worker_state = WorkerState::InLogin;
        inner.current_request_id = Some(inner.next_request_id);
    }

    /// 读取 Worker 运行时 OCR 能力（任务 10）
    ///
    /// 仅当 Worker 存活**且**最近一次 worker_health_check 上报了
    /// `capabilities.ocr` 时返回 `Some(bool)`；否则返回 `None`，
    /// 由调用方回退到文件探测（environment.ocr_ready）。
    pub fn runtime_ocr_capability(&self) -> Option<bool> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.process.as_ref()?;
        inner.worker_capabilities.as_ref()?.get("ocr")?.as_bool()
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

/// 超时自愈宽限等待与卡死强杀（历史遗留 F2 修复）
///
/// 超时请求 Cancel 后给 Worker 一段宽限期，等待 Python 侧命令超时自愈
/// （guard drop 释放会话槽位）。**归属校验**：仅当宽限期结束时槽位仍被
/// **同一请求**（`current_request_id == Some(stuck_request_id)`）占用才判定
/// 卡死并强杀回收；槽位已空（自愈成功）或已被**新请求**占用（旧请求已
/// 释放槽位、新会话合法进入）均视为自愈成功放行——旧实现只看
/// `current_request_id.is_some()`，会把新请求 B 占用的槽位误判为 A 卡死而强杀 B。
async fn grace_wait_slot_release(this: &BridgeSupervisor, stuck_request_id: u64, grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        let freed = {
            let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.current_request_id != Some(stuck_request_id)
        };
        if freed || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let still_stuck = {
        let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.current_request_id == Some(stuck_request_id)
    };
    if still_stuck {
        warn!(target: "python_worker", "命令超时后 Worker 未自愈，强制回收");
        kill_worker_now(this).await;
    }
}

/// BRG-1b：等待命令响应的可重置截止时刻循环
///
/// 截止时刻自调用时刻起算为 `now + timeout`；收到 `start_notify`（Python 侧
/// `command_started` 事件：该命令已出队并真正开始执行）后重置为
/// `now + timeout`——排队时间不再吃掉执行预算。旧 Worker 不发事件时截止时刻
/// 永不重置，语义与固定 `tokio::time::timeout(timeout, rx.recv())` 完全一致。
///
/// 使用 [`tokio::time::Instant`] 而非 [`std::time::Instant`]：与
/// `tokio::time::sleep` 同一时钟，单测可在 `tokio::time::pause` 下控时断言。
/// 三个等待分支均 cancel-safe，循环重入无副作用。
async fn recv_with_start_deadline(
    mut rx: mpsc::Receiver<Result<IpcResponse, BridgeError>>,
    start_notify: Arc<tokio::sync::Notify>,
    timeout: Duration,
) -> Result<IpcResponse, BridgeError> {
    let mut deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(BridgeError::Timeout);
        }
        tokio::select! {
            // Python 已真正开始执行：预算全额重启（Python 侧 0.9×预算守卫同时
            // 起跑，BRG-1「Python 自愈先于 Rust 超时」在排队场景重新成立）
            _ = start_notify.notified() => {
                deadline = tokio::time::Instant::now() + timeout;
            }
            r = rx.recv() => match r {
                Some(r) => return r,
                None => return Err(BridgeError::SupervisorNotRunning),
            },
            _ = tokio::time::sleep(remaining) => return Err(BridgeError::Timeout),
        }
    }
}

/// 处理 supervisor 命令
/// 等待 Worker 对取消的确认回包；宽限期内未确认且仍持有会话槽位则强杀。
///
/// 超时、显式取消与调用方中止（TSK-2）三条取消路径共用本收尾逻辑，
/// 确保「等 ACK → 归属校验 → 强杀」语义单一，不再各自实现导致漂移。
async fn wait_cancel_ack_or_kill(
    sup: &Arc<BridgeSupervisor>,
    rx: &mut oneshot::Receiver<Result<IpcResponse, BridgeError>>,
    cancel_id: &str,
) -> Result<IpcResponse, BridgeError> {
    match tokio::time::timeout(CANCEL_ACK_TIMEOUT, rx).await {
        Ok(_) => Err(BridgeError::Cancelled),
        Err(_) => {
            // 归属校验（与 grace_wait_slot_release 同一语义）：
            // 仅当会话槽位仍被本请求占用（current_cancel_id 即本
            // 请求 cancel_id）才强杀；槽位已被新请求接管或已空时，
            // 本请求与 Worker 存活与否已无关联，静默放弃——否则
            // 「不响应取消的挂起命令」的取消确认超时会误杀已接管
            // 槽位的新请求（F2 复发路径，
            // 见 tests/bridge_supervisor.rs 超时宽限用例）。
            let owns_slot = {
                let inner = sup.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.current_cancel_id.as_deref() == Some(cancel_id)
            };
            if owns_slot {
                warn!(
                    target: "python_worker",
                    "取消后 Worker 未在宽限期内停止，强制回收"
                );
                kill_worker_now(sup).await;
            }
            Err(BridgeError::Cancelled)
        }
    }
}

async fn handle_supervisor_command(this: &Arc<BridgeSupervisor>, cmd: SupervisorCommand) {
    match cmd {
        SupervisorCommand::Execute {
            method,
            params,
            response_tx,
            start_notify,
        } => {
            // 整个 execute_inner 在独立 task 中执行：ensure_worker 内的健康检查
            // 需 await Worker 回传的响应，而响应必须经 supervisor 主循环路由。若在主循环内
            // 直接 await，主循环被阻塞、无法处理 IPC 消息，健康检查将永远超时（死锁）。
            // 因此此处把 execute_inner 整体 spawn，主循环保持空闲以接收响应。
            let sup = Arc::clone(this);
            tokio::spawn(async move {
                match execute_inner(&sup, &method, params, start_notify).await {
                    Ok((mut rx, _guard, token, cancel_id)) => {
                        // 取消后保持 guard/会话槽位，直到 Worker 回包确认已经停止；
                        // 若 Worker 未响应则强制回收。不能仅因本地 token 触发就释放
                        // 槽位，否则新命令会进入仍被旧命令占用的串行 Worker。
                        let result = tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                wait_cancel_ack_or_kill(&sup, &mut rx, &cancel_id).await
                            },
                            // TSK-2：调用方 future 被 abort/drop 时响应通道断开。
                            // 此前无人监听该事件，Worker 会继续跑到自然结束或命令级
                            // 超时；现在进入与超时/显式取消相同的取消链路：发 Cancel
                            // 命令唤醒本地 token 与 Worker → 等 ACK/强杀，槽位经
                            // guard drop 释放。cancel_id 幂等：请求已结束时为 no-op。
                            _ = response_tx.closed() => {
                                sup.cancel(&cancel_id);
                                wait_cancel_ack_or_kill(&sup, &mut rx, &cancel_id).await
                            },
                            r = &mut rx => r.unwrap_or(Err(BridgeError::SupervisorNotRunning)),
                        };
                        // B3：guard drop 前结算调试会话开合（settle 需在守卫复位
                        // 判定前写入 debug_session_open）
                        match execute_debug_settle(&method, &result) {
                            DebugSettle::Open => {
                                let mut inner = sup.inner.lock().unwrap_or_else(|e| e.into_inner());
                                inner.debug_session_open = true;
                                inner.debug_last_activity = Some(Instant::now());
                                // 注意：不在此清空 last_screenshot_url——初始截图事件先于
                                // 本响应到达并已写入缓存，此处清空会抹掉它
                            }
                            DebugSettle::KeepOpen => {
                                let mut inner = sup.inner.lock().unwrap_or_else(|e| e.into_inner());
                                inner.last_activity = Instant::now();
                                inner.debug_last_activity = Some(Instant::now());
                            }
                            DebugSettle::Close => {}
                        }
                        let _ = response_tx.send(result).await;
                        // _guard drop 触发 reset_session / debug_guard_cleanup 清理
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
            let tx = {
                let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
                let tx = inner.pending_requests.remove(&resp.id);
                // BRG-1b：响应已结算，命令开始通知随之清理（与 pending 同生共死）
                inner.pending_starts.remove(&resp.id);
                tx
            };
            if let Some(tx) = tx {
                // oneshot::send 是同步操作，不会阻塞 supervisor 主循环
                let _ = tx.send(Ok(resp));
            } else if resp.id == SHUTDOWN_REQUEST_ID {
                // 优雅关闭请求（id=0）的哨兵回执：Rust 侧发出后不入 pending_requests，
                // Worker 按协议回包 → 到此即视为关闭 ACK，降 debug 不打虚假告警
                tracing::debug!(target: "python_worker", "收到关闭哨兵响应（id=0）");
            } else {
                tracing::warn!(target: "python_worker", "收到过期/未知响应 id={}", resp.id);
            }
        }
        ParsedMessage::ResponseError { id, error } => {
            // 带有效 id 但反序列化失败：以内部错误回收对应在途请求，防止永久泄漏
            // 与调试会话槽位卡死（历史遗留 F1）
            let tx = {
                let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
                let tx = inner.pending_requests.remove(&id);
                inner.pending_starts.remove(&id);
                tx
            };
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
        ParsedMessage::Event(mut ev) => {
            // BRG-1b 内部信令：Worker 已出队并开始执行该命令 → 触发对应请求的
            // 超时截止时刻重置（recv_with_start_deadline）。无注册项（健康检查
            // 直发路径 / 旧请求已结算 / id=0 shutdown 哨兵）时为 no-op。
            // 内部信令事件：不进 WS 白名单、不留痕（早于 trace 原始 dump 返回）。
            if ev.event == "command_started" {
                if let Some(id) = ev.data.get("id").and_then(Value::as_u64) {
                    let notify = this
                        .inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_starts
                        .remove(&id);
                    if let Some(notify) = notify {
                        notify.notify_one();
                    }
                }
                return;
            }
            // 事件转发白名单（step_progress/screenshot/dialog，均由 Python 侧实际
            // emit）转发到 WebSocket 日志流；其余事件仅 debug 记录。
            // 曾经白名单中的 `ocr_result` 为死臂：Python 侧从未 emit 该事件
            //（OCR 走 ocr_recognize 请求-响应，不走事件推送），已删除。
            // trace 级原始 dump：白名单事件已在下方以任务域 info 留痕，debug 级会与之双显
            trace!(target: "python_worker", "event {}: {:?}", ev.event, ev.data);
            if matches!(ev.event.as_str(), "screenshot" | "step_progress" | "dialog") {
                // 任务域留痕（target 归一为 source=task）：替代旧版前端合成条目，
                // 使步骤/弹窗日志获得统一 seq/时间戳，并落盘、可导出
                if ev.event == "step_progress" {
                    // 与旧前端合成文案保持一致：步骤 N 或 步骤 N/M: 描述
                    let idx = ev
                        .data
                        .get("step_index")
                        .and_then(|v| v.as_u64())
                        .map(|i| i + 1)
                        .unwrap_or(1);
                    let total = ev.data.get("total_steps").and_then(|v| v.as_u64());
                    let desc = ev
                        .data
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("执行步骤");
                    match total {
                        Some(total) => {
                            info!(target: "campus_auth::tasks", "步骤 {idx}/{total}: {desc}")
                        }
                        None => info!(target: "campus_auth::tasks", "步骤 {idx}: {desc}"),
                    }
                } else if ev.event == "dialog"
                    && let Some(msg) = ev
                        .data
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                {
                    info!(target: "campus_auth::tasks", "弹窗提示: {msg}");
                }
                // screenshot 事件负载为本地落盘 path，浏览器不可达；换算成
                // HTTP 预览 URL（GET /api/debug/screenshot/{filename}）供前端 <img> 使用
                if ev.event == "screenshot"
                    && let Some(path_str) = ev.data.get("path").and_then(|v| v.as_str())
                    && let Some(name) = std::path::Path::new(path_str).file_name()
                {
                    let url = format!("/api/debug/screenshot/{}", name.to_string_lossy());
                    ev.data["url"] = json!(url);
                    this.inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .last_screenshot_url = Some(url);
                }
                if let Some(tx) = this
                    .event_tx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                {
                    let payload = json!({ "type": ev.event, "data": ev.data });
                    if let Ok(s) = serde_json::to_string(&payload) {
                        let _ = tx.send(s);
                    }
                }
            }
        }
        ParsedMessage::InvalidLine(_) => {
            // 非 JSON 行已在 process.rs 的解析路径记录（含截断预览），此处不再重复
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
    mut params: Value,
    start_notify: Arc<tokio::sync::Notify>,
) -> Result<
    (
        oneshot::Receiver<Result<IpcResponse, BridgeError>>,
        SessionGuard,
        CancellationToken,
        String,
    ),
    BridgeError,
> {
    // OCR 只需要 Python Worker 与 ddddocr，不应被 Chromium 可执行文件状态阻断。
    let is_ocr = method == "ocr_recognize";
    // OCR 与问题报告捕获都是附着于现有 Worker 的轻量请求：允许并发，但绝不能
    // 抢占或复位登录/调试会话槽。feedback_capture 仍需浏览器环境，故不复用 is_ocr。
    let is_lightweight = is_lightweight_method(method);

    // debug_start 发起时清空上一会话的截图缓存：新会话的初始截图事件先于响应
    // 到达（Worker 先 emit 再返回），清空若放在响应结算处会把新缓存抹掉
    if method == "debug_start" {
        this.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_screenshot_url = None;
    }

    // G4：孤儿调试会话 TTL 预检（在原子临界区外执行 force_recycle，其内部同样
    // 需要 inner 锁）。调试面板被直接关闭（未点"停止调试"）时调试会话永久占住
    // 槽位、拒绝后续所有登录类命令；超过 30 分钟无调试活动判定为遗弃，强制
    // 回收 Worker（调试状态随进程销毁）放行本次请求。
    if matches!(
        method,
        "execute_login_attempt" | "execute_browser_task" | "test_redirect" | "profile_auto_login"
    ) {
        let stale = {
            let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.current_session == Some(SessionType::Debug)
                && debug_session_stale(inner.debug_last_activity, Instant::now())
        };
        if stale {
            tracing::warn!(
                "调试会话超过 30 分钟无活动，判定为孤儿会话，强制回收 Worker 放行本次请求"
            );
            this.force_recycle().await;
        }
    }

    // 1. 懒加载 Worker（环境就绪则 spawn）
    ensure_worker(this, is_ocr).await?;

    // 2. 会话类型（可见重定向检测独占窗口；debug_* 为调试会话；其余为登录任务）
    let session = if method == "test_redirect" {
        SessionType::RedirectTest
    } else if method.starts_with("debug_") {
        SessionType::Debug
    } else {
        SessionType::Login
    };
    // OCR 轻量请求：与任意会话并发（见 check_session_compat），不占用单会话槽位，
    // 也不触碰 current_session / worker_state / 空闲计时器（P1-6）。

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

    let browser_egress = if params.get("browser_settings").is_some() && !is_lightweight {
        let settings = params
            .get("network")
            .cloned()
            .map(serde_json::from_value::<crate::network::settings::NetworkSettings>)
            .transpose()
            .map_err(|_| BridgeError::IpcWriteError(std::io::Error::other("网络出口配置无效")))?
            .unwrap_or_else(|| this.config.runtime().load().network.clone());
        let previous = this
            .browser_egress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let route = if let Some(previous) = previous
            && previous.settings == settings
            && previous.still_current().await
        {
            previous
        } else {
            crate::network::egress::PreparedEgress::prepare(&settings)
                .await
                .map_err(|e| BridgeError::NetworkEgress(e.to_string()))?
        };
        route.apply_browser(&mut params["browser_settings"]);
        Some(route)
    } else {
        None
    };

    // 4. 单一原子临界区：互斥检查 → 分配 id → 注册 pending → 发送请求 → 注册 cancel/设置会话。
    // 全程同步操作，一次加锁完成，消除各步骤之间的窗口。原实现分 6 次独立加解锁，
    // “compat 检查”与“会话赋值”不原子，两个并发请求可能都通过互斥检查后各自赋值会话；
    // Cancel / reset_session / handle_ipc_message 均锁同一 Mutex，故本临界区与它们严格串行。
    let (resp_rx, request_id) = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());

        // 4.1 互斥检查（失败立即返回，此时尚未分配任何资源，无泄漏）
        check_session_compat(inner.current_session, method)?;
        // B3：调试会话已存活时拒绝重复 debug_start（槽位常驻 Some(Debug)，
        // 此检查兜底「compat 只看槽位」之外的开合状态）
        if method == "debug_start" && inner.debug_session_open {
            return Err(BridgeError::WorkerBusy);
        }

        // 4.2 分配 request id 并注册 pending 响应通道
        let request_id = inner.next_request_id;
        inner.next_request_id += 1;
        let (resp_tx, resp_rx) = oneshot::channel::<Result<IpcResponse, BridgeError>>();
        inner.pending_requests.insert(request_id, resp_tx);
        // BRG-1b：命令开始执行通知与 pending 同步注册（同生共死：响应命中 /
        // 下方两处回滚 / reset_session / lightweight_cleanup / drain 一并清理）
        inner.pending_starts.insert(request_id, start_notify);

        // 4.3 发送请求（可失败）。失败则回滚 pending；此时会话/取消状态尚未改动，无需额外回滚。
        match &inner.process {
            Some(proc) => {
                if let Err(e) = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
                    id: request_id,
                    method: method.to_string(),
                    params,
                })) {
                    inner.pending_requests.remove(&request_id);
                    inner.pending_starts.remove(&request_id);
                    return Err(BridgeError::IpcWriteError(std::io::Error::other(format!(
                        "Worker 通道已关闭或消息队列拥塞: {e}"
                    ))));
                }
            }
            None => {
                inner.pending_requests.remove(&request_id);
                inner.pending_starts.remove(&request_id);
                return Err(BridgeError::SupervisorNotRunning);
            }
        }

        if let Some(route) = browser_egress {
            *this
                .browser_egress
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(route);
        }
        // 4.4 发送成功后再改动会话/取消状态（此后不再有可失败操作）。
        // cancel_registry 支持多活跃 token 共存：InLogin 时新命令 supersede 会话槽位
        // （current_cancel_id 更新为新请求），但**不移除旧命令的 cancel_id**——
        // 旧登录的取消传播按 cancel_id 查注册表（session.rs attempt_cancel_id），
        // 提前移除会让用户对在途登录的取消静默失效。各命令自身的 cancel_id
        // 由其 SessionGuard drop 时无条件移除（防泄漏）。
        //
        // 轻量旁路注册到独立分区（P2-6）：`force_recycle` 的 `trigger_all`
        // 只取消会话区，避免 Worker 回收时误伤并发的 OCR/反馈截图请求。
        if is_lightweight {
            inner
                .cancel_registry
                .register_lightweight(cancel_id.clone(), token.clone());
        } else {
            inner
                .cancel_registry
                .register(cancel_id.clone(), token.clone());
        }
        if is_lightweight {
            // 轻量旁路：仅注册 cancel，不触碰会话槽位 / worker_state / 空闲计时器。
            // pending 与 cancel 的清理交给 guard drop 的轻量回调（lightweight_cleanup）。
        } else {
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
    // 轻量请求使用轻量守卫：drop 只清自身 pending 与 cancel 注册，绝不 reset_session
    // （否则会把并发登录会话的槽位复位为 Idle 并提前启动空闲计时器，见 5.1）。
    let guard = if is_lightweight {
        SessionGuard::new({
            let weak = this.self_weak.clone();
            let cancel_id = cancel_id.clone();
            move || {
                if let Some(sup) = weak.upgrade() {
                    lightweight_cleanup(&sup, request_id, &cancel_id);
                }
            }
        })
    } else if session == SessionType::Debug {
        // B3：Debug 命令的守卫不直接复位——是否保持会话开合由转发 task 的
        // settle 结果（debug_session_open）决定；stop / 未开合则完整复位
        SessionGuard::new({
            let weak = this.self_weak.clone();
            let cancel_id = cancel_id.clone();
            let is_stop = method == "debug_stop";
            move || {
                if let Some(sup) = weak.upgrade() {
                    debug_guard_cleanup(&sup, request_id, &cancel_id, is_stop);
                }
            }
        })
    } else {
        SessionGuard::new({
            let weak = this.self_weak.clone();
            let cancel_id = cancel_id.clone();
            move || {
                if let Some(sup) = weak.upgrade() {
                    reset_session(&sup, session, request_id);
                    // 会话槽位可能已被后续命令 supersede（reset_session 匹配失败
                    // 不做清理），本命令的 cancel_id 由守卫无条件移除防泄漏；
                    // cancel_id 为每请求 UUID，不会误删他人注册
                    sup.inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cancel_registry
                        .remove(&cancel_id);
                }
            }
        })
    };

    // 8. 返回 oneshot::Receiver、SessionGuard、CancellationToken 与本请求 cancel_id，
    //    由调用方（转发 task）等待响应；cancel_id 供「取消确认超时强杀」前做归属校验
    Ok((resp_rx, guard, token, cancel_id))
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
        // BRG-1b：命令已结束，开始执行通知随 pending 一并清理
        inner.pending_starts.remove(&id);
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
    // BRG-1b：轻量命令结束，开始执行通知随 pending 一并清理
    inner.pending_starts.remove(&request_id);
    inner.cancel_registry.remove(cancel_id);
}

/// 将当前 WorkerState 合并到 StatusManager 快照
///
/// 注意：调用方必须**已持有** `inner` 锁（或确保无锁竞争），本函数不再自行加锁。
/// 原实现内部 `this.inner.lock()` 会在调用方已持锁时导致 std::sync::Mutex 死锁。
fn merge_worker_status(inner: &BridgeInner, status: &StatusManager) {
    let state = worker_state_to_status(inner.worker_state, inner.process.is_some());
    status.merge(PartialSnapshot::Worker { state });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::test_support::{make_bridge, ok_resp};

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

    /// C7：cancel 在 Supervisor 未运行（命令接收端已丢弃，通道 Closed）时
    /// 不得 panic——Closed 路径静默降级为 debug 日志。
    #[tokio::test]
    async fn cancel_在_supervisor_未运行时不panic() {
        let (bridge, _dir) = make_bridge().await;
        // 丢弃命令接收端：模拟 supervisor 已停止
        drop(
            bridge
                .cmd_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        // 前置校验：确认 try_send 走的是 Closed 错误路径（而非静默入队）
        let probe = SupervisorCommand::Cancel {
            cancel_id: "probe".to_string(),
        };
        assert!(matches!(
            bridge.cmd_tx.try_send(probe),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
        ));
        // 连调多次：只 debug 日志，不得 panic
        for _ in 0..3 {
            bridge.cancel("x");
        }
    }

    /// C7：命令通道满（cap 64）时 cancel 升级为「spawn + await send」可靠投递，
    /// 不再静默丢弃——排空通道后取消命令最终送达。
    #[tokio::test]
    async fn cancel_通道满时升级为可靠投递() {
        let (bridge, _dir) = make_bridge().await;
        // 塞满命令通道（64）：后续 try_send 必走 Full 分支
        for i in 0..64 {
            assert!(
                bridge
                    .cmd_tx
                    .try_send(SupervisorCommand::Cancel {
                        cancel_id: format!("fill-{i}"),
                    })
                    .is_ok(),
                "填充消息 {i} 应成功入队"
            );
        }
        bridge.cancel("late"); // Full → 独立 task 阻塞投递
        // 手动消费：64 条填充消息之后，应收到 cancel("late")——证明未静默丢弃
        let mut rx = bridge
            .cmd_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("接收端应仍在");
        let mut got_late = false;
        for _ in 0..65 {
            match rx.recv().await.expect("通道不应关闭") {
                SupervisorCommand::Cancel { cancel_id } if cancel_id == "late" => {
                    got_late = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(got_late, "通道满时 cancel 仍应经阻塞投递送达");
    }

    /// F2：宽限期内槽位仍被**同一**超时请求占用 → 判定卡死，强杀回收。
    /// 可观测效果：kill_worker_now 将 worker_state 置为 Error 并 drain pending。
    #[tokio::test]
    async fn f2_宽限期槽位仍被同一请求占用_判定卡死强杀() {
        let (bridge, _dir) = make_bridge().await;
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::InLogin;
            inner.current_request_id = Some(7);
            let (tx, _rx) = oneshot::channel();
            inner.pending_requests.insert(7, tx);
        }
        grace_wait_slot_release(&bridge, 7, Duration::from_millis(50)).await;
        let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            inner.worker_state,
            WorkerState::Error,
            "卡死应触发强杀置 Error"
        );
        assert!(inner.pending_requests.is_empty(), "强杀应 drain 在途请求");
    }

    /// F2 核心回归：请求 A 超时自愈释放槽位后，新请求 B 占位——宽限循环不得
    /// 把 B 误判为 A 卡死而强杀（旧实现只看 is_some，B 会被误杀）。
    #[tokio::test]
    async fn f2_宽限期槽位被新请求占用_不误杀新会话() {
        let (bridge, _dir) = make_bridge().await;
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            // 模拟：A（request 7）已释放，B（request 8）已占用槽位
            inner.current_request_id = Some(8);
        }
        // 以 A 的 request id 进入宽限等待；B 占位应被视为自愈成功放行
        grace_wait_slot_release(&bridge, 7, Duration::from_millis(50)).await;
        let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
        assert_ne!(
            inner.worker_state,
            WorkerState::Error,
            "新请求占位不得触发强杀"
        );
        assert_eq!(inner.current_request_id, Some(8), "B 的槽位不应被复位");
    }

    /// F2：宽限期内槽位释放为空 → 自愈成功，不杀。
    #[tokio::test]
    async fn f2_宽限期槽位释放为空_不杀() {
        let (bridge, _dir) = make_bridge().await;
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            inner.current_request_id = None;
        }
        grace_wait_slot_release(&bridge, 7, Duration::from_millis(50)).await;
        let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
        assert_ne!(inner.worker_state, WorkerState::Error, "槽位已空不应强杀");
    }

    /// 任务 10：runtime_ocr_capability 在 Worker 未存活时一律 None（回退文件探测）。
    #[tokio::test]
    async fn 能力上报_worker未存活时返回none() {
        let (bridge, _dir) = make_bridge().await;
        // 无进程时即使缓存存在也不得上报
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_capabilities = Some(json!({ "ocr": true }));
        }
        assert_eq!(bridge.runtime_ocr_capability(), None);
        // 无缓存同样 None
        {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_capabilities = None;
        }
        assert_eq!(bridge.runtime_ocr_capability(), None);
    }

    // ============ C2 / BRG-1b：command_started 重置超时预算 ============

    /// 收到 command_started（notify）后截止时刻重置为「now + 完整预算」：
    /// 排队 2s + 执行 2s 回包，自发送起算 4s > 3s 预算（旧固定超时必判
    /// Timeout），但起算点后移后应正常收到响应。
    #[tokio::test]
    async fn command_started_重置超时预算() {
        tokio::time::pause();
        let (tx, rx) = mpsc::channel::<Result<IpcResponse, BridgeError>>(1);
        let start_notify = Arc::new(tokio::sync::Notify::new());
        let jh = tokio::spawn(recv_with_start_deadline(
            rx,
            start_notify.clone(),
            Duration::from_secs(3),
        ));
        // 排队 2s（未超 3s 预算）后命令才真正开始执行 → 预算重启
        tokio::time::advance(Duration::from_secs(2)).await;
        start_notify.notify_one();
        // 让等待循环确定性地消费 started 通知（重置截止时刻）后再推进时间
        tokio::task::yield_now().await;
        // 执行 2s 后回包：自发送起算 4s > 3s（旧语义必超时），
        // 自 command_started 起算仅 2s < 3s（新语义应收到响应）
        tokio::time::advance(Duration::from_secs(2)).await;
        tx.send(ok_resp()).await.expect("发送响应失败");
        let r = tokio::time::timeout(Duration::from_secs(1), jh)
            .await
            .expect("循环应在收到响应后立即返回")
            .expect("循环 task 不应 panic");
        assert!(
            matches!(r, Ok(ref resp) if resp.result.success),
            "command_started 后预算应重启，响应不应被判超时，实际 {r:?}"
        );
    }

    /// 旧 Worker 兼容：不发 command_started 时截止时刻永不重置，
    /// 预算耗尽即超时（与固定 tokio::time::timeout 语义一致）。
    #[tokio::test]
    async fn 无command_started_预算自发送起算() {
        tokio::time::pause();
        let (_tx, rx) = mpsc::channel::<Result<IpcResponse, BridgeError>>(1);
        let jh = tokio::spawn(recv_with_start_deadline(
            rx,
            Arc::new(tokio::sync::Notify::new()),
            Duration::from_secs(3),
        ));
        tokio::time::advance(Duration::from_secs(3)).await;
        let r = jh.await.expect("循环 task 不应 panic");
        assert!(
            matches!(r, Err(BridgeError::Timeout)),
            "无 started 事件应按固定预算超时（旧语义保持），实际 {r:?}"
        );
    }
}
