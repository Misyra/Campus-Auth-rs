//! Supervisor 内部状态与命令
//!
//! `BridgeInner` 为 BridgeSupervisor 的互斥保护状态（会话槽位、pending 请求与
//! 命令开始通知、取消注册表、调试会话标记等）；`SupervisorCommand` 为后台主
//! 循环处理的外部命令。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{
    BridgeError, CancelRegistry, IpcResponse, ParsedMessage, SessionType, WorkerProcess,
    WorkerState,
};

/// Supervisor 后台 task 处理的命令
pub(super) enum SupervisorCommand {
    /// 执行命令，结果通过 oneshot 返回
    Execute {
        method: String,
        params: Value,
        response_tx: mpsc::Sender<Result<IpcResponse, BridgeError>>,
        /// BRG-1b：命令开始执行通知。execute_inner 注册到 pending_starts，
        /// Worker emit `command_started` 事件时触发，execute_with_timeout
        /// 据此把超时截止时刻重置为「now + 完整预算」。
        start_notify: Arc<tokio::sync::Notify>,
    },
    /// 发送取消通知
    Cancel { cancel_id: String },
    /// 优雅关闭 Worker
    Shutdown,
    /// 空闲计时器触发
    IdleTimeout,
}

/// Bridge 内部状态（Mutex 保护）
pub(super) struct BridgeInner {
    pub(super) worker_state: WorkerState,
    pub(super) process: Option<WorkerProcess>,
    pub(super) pending_requests: HashMap<u64, oneshot::Sender<Result<IpcResponse, BridgeError>>>,
    pub(super) next_request_id: u64,
    pub(super) last_activity: Instant,
    pub(super) idle_timer: Option<JoinHandle<()>>,
    pub(super) cancel_registry: CancelRegistry,
    pub(super) current_session: Option<SessionType>,
    pub(super) current_cancel_id: Option<String>,
    pub(super) current_request_id: Option<u64>,
    /// Supervisor 主循环持有的 IPC 消息 Receiver 对应的 Sender
    pub(super) ipc_tx: Option<mpsc::Sender<ParsedMessage>>,
    /// 连续 spawn/健康检查失败计数（B3 熔断）
    pub(super) consecutive_spawn_failures: u32,
    /// 调试会话存活标记（B3 根治）
    ///
    /// `debug_start` 成功后置位，`debug_stop` / 失败 / Worker 退出时清除。
    /// 存活期内会话槽位保持 `Some(Debug)`：登录/浏览器任务被 compat 矩阵
    /// 快速失败（此前仅命令在途窗口受保护，命令间隙自动登录可插入共用页面）；
    /// 空闲计时器不启动（调试静置不再被回收）。
    pub(super) debug_session_open: bool,
    /// 调试会话最近一次活动时刻（start/step/run_all 成功完成时刷新）。
    /// G4：登录类命令遇活跃调试会话时，若超过 `DEBUG_SESSION_TTL` 无活动，
    /// 判定为孤儿会话并强制回收 Worker 放行，而非无限拒绝。
    pub(super) debug_last_activity: Option<std::time::Instant>,
    /// 调试会话存续期最近一次截图的预览 URL（screenshot 事件转发时更新，
    /// debug_start 置会话时清空）。供 /api/debug/status 在前端刷新"失忆"后
    /// 恢复截图预览——WS 事件不会重放。
    pub(super) last_screenshot_url: Option<String>,
    /// 最近一次 Worker 健康检查上报的运行时能力（如 `{"ocr": true}`）
    ///
    /// 任务 10：由 `send_health_check`（worker_health_check 路径）捕获，
    /// Worker 回收/退出时失效。供 `/api/ocr/status` 在 Worker 存活时
    /// 优先展示运行时能力，替代文件探测。
    pub(super) worker_capabilities: Option<Value>,
    /// 请求 id → 命令开始执行通知（BRG-1b）。execute_with_timeout 借此把
    /// 超时起算点改到 Python 真正开始执行；生命周期与 pending_requests 严格
    /// 同生共死（响应命中 / 注册回滚 / guard 清理 / drain 一并清理）。
    /// request_id 单调递增永不复用（含 id=0 shutdown 哨兵），不存在陈旧
    /// Notify 误唤醒新请求的可能。
    pub(super) pending_starts: HashMap<u64, Arc<tokio::sync::Notify>>,
}
