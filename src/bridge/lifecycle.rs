//! Worker 进程生命周期：懒加载 spawn、健康检查、强杀回收、优雅关闭、退出与空闲处理
//!
//! ensure_worker 串行化 spawn + 环境门禁 + 自愈重试；send_health_check 复用
//! pending 通道等待健康回包；kill/handle_shutdown/exit 处理统一 drain 在途请求；
//! 空闲计时器由 reset_session 等守卫路径启动、handle_idle_timeout 触发回收。

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::environment::PYTHON_EXE_RELATIVE;

use super::state::{BridgeInner, SupervisorCommand};
use super::{
    BridgeError, BridgeSupervisor, DEFAULT_SHUTDOWN_TIMEOUT_SECS,
    DEFAULT_WORKER_STARTUP_TIMEOUT_SECS, IpcMessage, IpcRequest, IpcResponse, SHUTDOWN_REQUEST_ID,
    SessionType, WorkerState, merge_worker_status, orphan, process, spawn_worker,
};

/// 连续 spawn 失败熔断阈值：达到后 ensure_worker 直接快速失败（不再 spawn）
const SPAWN_FAILURE_THRESHOLD: u32 = 3;

/// 执行孤儿浏览器清理：spawn_blocking 隔离同步枚举 + 5s 超时兜底（A-4）
async fn run_orphan_cleanup_with_timeout() {
    let task = tokio::task::spawn_blocking(orphan::cleanup_orphan_browsers);
    match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("孤儿浏览器清理任务失败: {e}"),
        Err(_) => tracing::warn!("孤儿浏览器清理超时（5s），跳过本次"),
    }
}

/// 启动空闲计时器：超时后发送 IdleTimeout
///
/// `keep_alive=true` 时跳过计时器（Worker 常驻）；否则使用配置 `worker.idle_timeout_seconds`。
pub(super) fn start_idle_timer(this: &BridgeSupervisor, inner: &mut BridgeInner) {
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
pub(super) async fn ensure_worker(
    this: &Arc<BridgeSupervisor>,
    worker_only_health_check: bool,
) -> Result<(), BridgeError> {
    // 快速路径：已就绪
    if cfg!(feature = "openwrt") {
        return Err(BridgeError::ExecutionError {
            message: "OpenWrt 版不提供浏览器自动化和 OCR Worker".into(),
            data: None,
        });
    }
    if is_worker_ready(this) {
        return Ok(());
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

    // 每次新 spawn 前重新执行真实 import 探针，避免进程运行期间依赖被删改后
    // 仍信任旧快照。失败由 EnvironmentManager 在同一门内执行 uv sync 自愈。
    let environment = this
        .environment
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(Weak::upgrade);
    if let Some(environment) = &environment {
        environment
            .refresh_status()
            .await
            .map_err(|error| BridgeError::WorkerEnvironmentInvalid(error.to_string()))?;
        let result = if worker_only_health_check {
            environment.ensure_worker_ready().await
        } else {
            environment.ensure_capability().await
        };
        result.map_err(|error| BridgeError::WorkerEnvironmentInvalid(error.to_string()))?;
    }

    // 环境修复会复位旧熔断，因此熔断检查必须放在门禁之后。
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
    // 校验 Python 解释器是否存在（路径解析与 EnvironmentManager 一致，含 dev 回退）
    let python_exe = this.worker_project_dir.join(PYTHON_EXE_RELATIVE);
    let worker_main = this.worker_project_dir.join("worker_main.py");
    if !python_exe.exists() {
        return Err(BridgeError::WorkerNotInstalled);
    }
    // 清理上次崩溃残留的孤儿浏览器进程（A-4 降频）：同步 powershell/proc 枚举
    // 冷启动可达秒级，仅在 Supervisor 生命周期首次 spawn 前执行；崩溃路径仍每次清理。
    // 包 5s 超时防 CIM 服务卡死拖住 spawn 路径
    if !this
        .orphan_cleanup_done
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        run_orphan_cleanup_with_timeout().await;
    }
    // spawn 子进程 + 四个后台 task；首次失败会强制同步核心依赖并原地重试一次。
    let ipc_tx = this
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ipc_tx
        .clone()
        .ok_or(BridgeError::WorkerStartupTimeout)?;
    let keep_alive = this.config.runtime().load().worker.keep_alive;
    for attempt in 0..2 {
        let process = match spawn_worker(
            &python_exe,
            &worker_main,
            &this.base_path,
            keep_alive,
            ipc_tx.clone(),
        )
        .await
        {
            Ok(process) => process,
            Err(error) if attempt == 0 && environment.is_some() => {
                warn!(target: "python_worker", "Worker 进程首次启动失败，尝试修复运行时: {error}");
                if let Some(environment) = &environment
                    && let Err(repair_error) = environment.repair_worker_runtime().await
                {
                    return Err(BridgeError::WorkerEnvironmentInvalid(
                        repair_error.to_string(),
                    ));
                }
                continue;
            }
            Err(error) => {
                record_spawn_failure(this);
                return Err(error);
            }
        };
        {
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Starting;
            inner.process = Some(process);
            merge_worker_status(&inner, &this.status);
        }
        if let Some(metrics) = &this.metrics {
            metrics.inc_worker_spawn();
        }

        // 纯 OCR 只验证 Worker IPC；浏览器任务继续验证 Playwright/Chromium。
        if matches!(
            send_health_check(this, worker_only_health_check).await,
            Ok(true)
        ) {
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            inner.consecutive_spawn_failures = 0;
            merge_worker_status(&inner, &this.status);
            info!(target: "python_worker", "Worker 健康检查通过，已就绪");
            return Ok(());
        }

        warn!(target: "python_worker", attempt = attempt + 1, "Worker 健康检查失败或超时");
        kill_worker_now(this).await;
        if attempt == 0
            && let Some(environment) = &environment
        {
            // 先区分故障面：健康检查失败多为浏览器二进制缺失/损坏（uv sync 对此无效），
            // 只有浏览器本身完好时才退回强制同步 Worker 依赖。
            let channel = {
                let cfg = this.config.runtime().load();
                cfg.browser.browser_channel.clone()
            };
            if environment
                .repair_browser_channel(&channel)
                .await
                .map_err(|error| BridgeError::WorkerEnvironmentInvalid(error.to_string()))?
            {
                tracing::info!(target: "python_worker", "浏览器组件重装完成，重试当前请求");
                continue;
            }
            tracing::info!(target: "python_worker", "强制同步 Worker 依赖后重试当前请求");
            environment
                .repair_worker_runtime()
                .await
                .map_err(|error| BridgeError::WorkerEnvironmentInvalid(error.to_string()))?;
            continue;
        }
        record_spawn_failure(this);
        return Err(BridgeError::WorkerStartupTimeout);
    }
    Err(BridgeError::WorkerStartupTimeout)
}

/// 仅在一次“预检 + 自愈重试”整体失败后计数，避免可修复故障快速打满熔断。
fn record_spawn_failure(this: &BridgeSupervisor) {
    let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
    inner.consecutive_spawn_failures += 1;
    merge_worker_status(&inner, &this.status);
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
async fn send_health_check(
    this: &BridgeSupervisor,
    worker_only: bool,
) -> Result<bool, BridgeError> {
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
                let (method, params) = if worker_only {
                    ("worker_health_check", Value::Null)
                } else {
                    let cfg = this.config.runtime().load();
                    let params = serde_json::to_value(&cfg.browser)
                        .map(|bs| json!({ "browser_settings": bs }))
                        .unwrap_or(Value::Null);
                    ("browser_health_check", params)
                };
                if let Err(e) = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
                    id: request_id,
                    method: method.to_string(),
                    params,
                })) {
                    inner.pending_requests.remove(&request_id);
                    return Err(BridgeError::IpcWriteError(std::io::Error::other(format!(
                        "IPC channel send failed: {e}"
                    ))));
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
    // 任务 10：Worker 轻量健康检查会随响应上报运行时能力（capabilities），
    // 此处缓存供 /api/ocr/status 等读取；缺失该字段时清空旧缓存（回退文件探测）。
    if worker_only {
        let caps = resp.result.data.get("capabilities").cloned();
        this.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker_capabilities = caps;
    }
    // 健康检查成功且 Worker 报告浏览器可用
    let healthy = resp.result.success
        && resp
            .result
            .data
            .get("healthy")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if !healthy && !worker_only {
        // Worker 随响应上报缺失组件；转成可操作日志，否则用户只看到通用「健康检查失败」。
        // `reinstallable` 由 Worker 给出：托管引擎（chromium/firefox/webkit）可由
        // `uv playwright install` 自动补齐；系统通道（msedge/chrome）与 custom 没有
        // 可下载的二进制，此时文案必须指向「装浏览器 / 换通道」而非「自动重装」。
        let data = &resp.result.data;
        let missing: Vec<&str> = data
            .get("missing")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let reinstallable = data
            .get("reinstallable")
            .and_then(Value::as_bool)
            .unwrap_or(!missing.is_empty());
        if missing.is_empty() {
            warn!(target: "python_worker", "浏览器健康检查未通过（未上报缺失组件明细）");
        } else if reinstallable {
            warn!(
                target: "python_worker",
                missing = %missing.join("、"),
                "浏览器健康检查未通过：缺少浏览器组件，将尝试自动重装"
            );
        } else {
            warn!(
                target: "python_worker",
                missing = %missing.join("、"),
                "浏览器健康检查未通过：配置的浏览器不可用且无法自动安装，请按提示处理后重试"
            );
        }
    }
    Ok(healthy)
}

/// 强杀当前 Worker 子进程并标记 Error（不清理 cancel 注册表/会话）
pub(super) async fn kill_worker_now(this: &BridgeSupervisor) {
    this.browser_egress
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let proc = this
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .process
        .take();
    if let Some(p) = proc {
        // 先尝试优雅关闭，超时则由 shutdown 内部强杀
        let _ = p.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
            id: SHUTDOWN_REQUEST_ID,
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        // 仅依赖 p.shutdown 内部的 timeout，避免外层再包 timeout 导致可达 2 倍超时
        p.shutdown(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS))
            .await;
    }
    // 进程已终止，在途请求不可能再得到响应：立即 drain 并结算，
    // 否则对应 execute_inner 要挂满 300s 超时才返回（且只能拿到 Timeout 错误）
    drain_pending_requests(this, "worker killed by supervisor");
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.worker_state = WorkerState::Error;
        // 能力缓存随进程失效（任务 10），避免对已死 Worker 上报运行时能力
        inner.worker_capabilities = None;
        // 进程已亡，调试会话随之终结（B3）
        inner.debug_session_open = false;
        inner.debug_last_activity = None;
        inner.last_screenshot_url = None;
        merge_worker_status(&inner, &this.status);
    }
}

/// 优雅关闭 Worker（shutdown 命令 → 等超时 → kill）
pub(super) async fn handle_shutdown(this: &Arc<BridgeSupervisor>) {
    let process = this
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .process
        .take();
    if let Some(proc) = process {
        // 先发送 shutdown 命令，等待 Worker 自行退出
        let _ = proc.stdin_tx.try_send(IpcMessage::Request(IpcRequest {
            id: SHUTDOWN_REQUEST_ID,
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        // 仅依赖 proc.shutdown 内部的 timeout，避免外层再包 timeout 导致可达 2 倍超时
        proc.shutdown(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS))
            .await;
        info!(target: "python_worker", "Worker 已关闭");
    }
    // 与 kill_worker_now 同理：进程已回收，drain 在途请求避免悬挂至超时
    drain_pending_requests(this, "worker shut down");
    {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.worker_state = WorkerState::Idle;
        // 能力缓存随进程失效（任务 10）；调试会话随进程终结（B3）
        inner.worker_capabilities = None;
        inner.debug_session_open = false;
        inner.debug_last_activity = None;
        inner.last_screenshot_url = None;
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
        // BRG-1b：进程已终结，在途请求全部结算，开始执行通知一并清空
        inner.pending_starts.clear();
        inner.pending_requests.drain().map(|(_, tx)| tx).collect()
    };
    for tx in pending {
        let _ = tx.send(Err(BridgeError::WorkerCrashed {
            reason: reason.to_string(),
        }));
    }
}

/// 空闲计时器触发：若仍处于 Idle 则回收 Worker
///
/// 历史遗留 F3：`Idle` 只代表会话槽位空闲，OCR 等轻量在途请求**不占槽位、
/// 不改 worker_state**，仅体现在 `pending_requests` 中。存在在途请求时不能
/// 回收 Worker（否则请求被 drain 为 `WorkerCrashed`），重置活动时刻并重启
/// 空闲计时器顺延一个完整空闲周期；待请求结束后再由计时器正常回收。
pub(super) async fn handle_idle_timeout(this: &Arc<BridgeSupervisor>) {
    let should_shutdown = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(inner.worker_state, WorkerState::Idle) {
            return;
        }
        // F3：在途请求检查优先于回收判定——OCR 轻量旁路不占会话槽位、
        // 不改 worker_state，仅体现在 pending_requests 中，Idle 状态下仍可能有
        // 在途请求。此时顺延而非 shutdown，否则请求被 drain 为 WorkerCrashed。
        if !inner.pending_requests.is_empty() {
            // 重置 last_activity 保证顺延后仍空闲满完整 idle 周期才回收；
            // 重启计时器接管已被消耗的旧计时器。
            inner.last_activity = Instant::now();
            start_idle_timer(this, &mut inner);
            debug!(
                target: "python_worker",
                "空闲回收触发但存在 {} 个在途请求，顺延一个空闲周期",
                inner.pending_requests.len()
            );
            return;
        }
        inner.process.is_some()
    };
    if should_shutdown {
        handle_shutdown(this).await;
    }
}

/// Worker 退出处理：区分正常退出（exit_code=0）与崩溃
pub(super) async fn handle_worker_exited(this: &Arc<BridgeSupervisor>, code: i32) {
    // 汇总本 Worker 生命周期内的非 JSON IPC 行（stdout 被意外 print 污染的信号）
    let invalid_lines = process::take_invalid_ipc_line_count();
    if invalid_lines > 0 {
        warn!(
            target: "python_worker",
            "Worker 运行期间累计 {invalid_lines} 行非 JSON IPC 输出（stdout 疑似被第三方库污染），相关请求可能已超时"
        );
    }
    // 正常退出（空闲回收 / 用户主动停止 / shutdown / Worker 自行退出）：不记为崩溃，
    // 不计入指标，不触发孤儿清理，也不置 Error。但必须完整清理进程与会话状态——
    // Worker 可能非由 Rust 发起 shutdown 而自行退出（如 stdin EOF）：若只置 Idle
    // 而保留 process/current_session，is_worker_ready 会持续误报 Ready，后续命令
    // 写死管道且永不重 spawn，调试会话滞留还会让所有登录被 WorkerBusy 拒绝。
    if code == 0 {
        info!(target: "python_worker", "Worker 正常退出，exit_code=0");
        // 正常退出时通常不应有在途请求；仍 drain 防御，调用方收到定性错误
        drain_pending_requests(this, "worker exited (code 0)");
        let handles = {
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.cancel_registry.clear();
            inner.current_session = None;
            inner.current_cancel_id = None;
            inner.current_request_id = None;
            inner.debug_session_open = false;
            inner.debug_last_activity = None;
            // 进程对象必须 take：残留会让 is_worker_ready（Idle && process.is_some()）
            // 对已死 Worker 返回 true，execute 走快速路径写已关闭的 stdin
            let handles = inner.process.take().map(|p| p.handles);
            inner.worker_state = WorkerState::Idle;
            // 能力缓存随进程失效（与崩溃分支同语义）
            inner.worker_capabilities = None;
            merge_worker_status(&inner, &this.status);
            handles
        };
        if let Some(h) = handles {
            h.stdin_task.abort();
            h.stdout_task.abort();
            h.stderr_task.abort();
            h.health_task.abort();
        }
        return;
    }
    // 崩溃日志单点记录：是否正处于调试会话经 debug_session 字段表达
    //（替代原先"崩溃 + 调试会话被强制终止"两条相邻 warn）
    let debug_session = {
        let inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.current_session == Some(SessionType::Debug)
    };
    warn!(
        target: "python_worker",
        debug_session,
        exit_code = code,
        "Worker 进程崩溃退出"
    );
    // 崩溃恢复时清理可能残留的孤儿浏览器进程（异常退出路径，A-4 不降频）；
    // 同样包 5s 超时防枚举卡死拖住恢复流程
    run_orphan_cleanup_with_timeout().await;
    // Worker 崩溃，递增指标
    if let Some(m) = &this.metrics {
        m.inc_worker_crash();
    }
    let (pending, handles, crashed_session) = {
        let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pending: Vec<_> = inner.pending_requests.drain().map(|(_, tx)| tx).collect();
        // BRG-1b：崩溃路径独立于 drain_pending_requests 结算在途请求，
        // 开始执行通知必须一并清空（否则残留 Arc 泄漏且永无唤醒意义）
        inner.pending_starts.clear();
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
        // 能力缓存随进程失效（任务 10）
        inner.worker_capabilities = None;
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
    if crashed_session == Some(SessionType::Debug)
        && let Some(tx) = this
            .event_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
    {
        let payload =
            json!({ "type": "debug_session_closed", "data": { "reason": "worker_crashed" } });
        if let Ok(s) = serde_json::to_string(&payload) {
            let _ = tx.send(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::bridge::test_support::make_bridge;

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
        let result = ensure_worker(&bridge, false).await;
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

    /// F3：空闲计时器触发但存在在途请求（OCR 轻量旁路）→ 顺延而非 shutdown。
    /// 可观测效果：pending 不被 drain（oneshot 未收到 WorkerCrashed）、
    /// idle_timer 被重启。
    #[tokio::test]
    async fn f3_idle触发但有在途请求_顺延不回收() {
        use tokio::sync::oneshot;

        let (bridge, _dir) = make_bridge().await;
        let mut rx = {
            let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.worker_state = WorkerState::Idle;
            let (tx, rx) = oneshot::channel();
            inner.pending_requests.insert(100, tx);
            rx
        };
        handle_idle_timeout(&bridge).await;
        {
            let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
            // 在途请求未被 drain（未被结算为 WorkerCrashed）
            assert!(
                inner.pending_requests.contains_key(&100),
                "顺延路径不应 drain 在途请求"
            );
            // 空闲计时器被重启（顺延一个周期）
            assert!(inner.idle_timer.is_some(), "顺延路径应重启空闲计时器");
        }
        // oneshot 未收到任何结算（shutdown 路径会立即收到 WorkerCrashed）
        assert!(rx.try_recv().is_err(), "顺延路径不应向在途请求结算错误");
        // 清理：中止重启的计时器，避免测试运行时后台 task 残留
        if let Some(h) = bridge
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle_timer
            .take()
        {
            h.abort();
        }
    }
}
