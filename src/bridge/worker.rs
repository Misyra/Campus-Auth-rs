//! Worker 状态机：内部状态 + 外部状态映射

use crate::status::WorkerStatus;

/// Worker 内部状态（6 变体，驱动状态机转换）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Python 环境未就绪
    NotInstalled,
    /// 子进程 spawn 中，等待 browser_health_check
    Starting,
    /// Worker 已就绪，空闲等待命令
    Idle,
    /// 正在执行 execute_login_attempt / execute_browser_task
    InLogin,
    /// debug_start 已保留浏览器上下文
    InDebug,
    /// Worker 崩溃后标记
    Error,
}

/// 将内部 WorkerState 映射为外部 WorkerStatus
pub fn worker_state_to_status(state: WorkerState, process_alive: bool) -> WorkerStatus {
    use WorkerState::*;
    match state {
        NotInstalled => WorkerStatus::NotInstalled,
        Starting => WorkerStatus::Starting,
        Idle => {
            if process_alive {
                WorkerStatus::Ready
            } else {
                WorkerStatus::Stopped
            }
        }
        InLogin | InDebug => WorkerStatus::Busy,
        Error => WorkerStatus::Error,
    }
}
