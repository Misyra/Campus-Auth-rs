//! Bridge 错误类型

use serde_json::Value;

/// Bridge 相关错误
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// 所选网络出口暂不可用；可冷却重试，但不改走其他网卡。
    #[error("校园网出口不可用: {0}")]
    NetworkEgress(String),
    /// Python 环境未安装
    #[error("Worker 环境未安装")]
    WorkerNotInstalled,

    /// Worker 启动超时
    #[error("Worker 启动超时")]
    WorkerStartupTimeout,

    /// Worker 进程崩溃
    #[error("Worker 进程崩溃: {reason}")]
    WorkerCrashed {
        /// 失败原因。
        reason: String,
    },

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
        /// 供用户或日志展示的说明。
        message: String,
        /// 可选事件数据。
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

    /// Worker 启动前环境验证或自愈失败
    #[error("Worker 环境验证失败: {0}")]
    WorkerEnvironmentInvalid(String),
}
