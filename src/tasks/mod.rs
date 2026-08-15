//! 任务管理：TaskManager + TaskExecutor
//!
//! 本模块提供任务的文件 CRUD 管理（[`TaskManager`]）与脚本/Shell/浏览器任务的异步执行
//! （[`TaskExecutor`]）。任务数据模型见 [`models`]；任务执行统一结果见 [`TaskResult`]；
//! 统一错误类型见 [`TaskError`]。

pub mod models;
pub mod loader;
pub mod executor;

pub use models::*;
pub use loader::{TaskManager, TaskSummary, OrderData, TaskDetail};
pub use executor::{TaskExecutor, TaskResult};

use thiserror::Error;

/// 任务模块统一错误类型
#[derive(Debug, Error)]
pub enum TaskError {
    /// task_id 不匹配 `TASK_ID_PATTERN`
    #[error("无效的任务ID: {0}")]
    InvalidTaskId(String),
    /// 任务文件不存在
    #[error("任务不存在: {0}")]
    TaskNotFound(String),
    /// 创建时 ID 冲突
    #[error("任务ID重复: {0}")]
    DuplicateTaskId(String),
    /// 尝试删除 `default` 任务
    #[error("不能删除默认任务")]
    DeleteDefaultTask,
    /// JSON 格式/字段校验不通过
    #[error("任务验证失败: {0:?}")]
    ValidationFailed(Vec<String>),
    /// script_path 指向的文件不存在
    #[error("脚本文件不存在: {0}")]
    ScriptNotFound(String),
    /// 扩展名不在白名单
    #[error("不支持的脚本扩展名: {0}")]
    UnsupportedExtension(String),
    /// Shell 任务 command 为空
    #[error("命令为空")]
    CommandEmpty,
    /// 执行超时
    #[error("执行超时: {0}s")]
    ExecutionTimeout(u64),
    /// 文件/进程 IO 错误
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),
    /// 序列化/反序列化错误
    #[error("JSON 错误: {0}")]
    JsonError(#[from] serde_json::Error),
    /// Bridge IPC 错误（保留变体，供上层映射 409 WorkerBusy 等）
    #[error("Bridge 错误: {0}")]
    Bridge(#[from] crate::bridge::BridgeError),
    /// 环境能力错误
    #[error("环境能力错误: {0}")]
    Environment(String),
}
