//! 任务管理：TaskManager + TaskExecutor
//!
//! 本模块提供任务的文件 CRUD 管理（[`TaskManager`]）与脚本/Shell/浏览器任务的异步执行
//! （[`TaskExecutor`]）。任务数据模型见 [`models`]；任务执行统一结果见 [`TaskResult`]；
//! 统一错误类型见 [`TaskError`]。

pub mod executor;
pub mod loader;
pub mod models;

pub use executor::{TaskExecutor, TaskResult};
pub use loader::{OrderData, TaskDetail, TaskManager, TaskSummary};
pub use models::*;

use thiserror::Error;

/// Web 层消费的任务管理抽象（M1 细粒度 state：tasks 域）
///
/// handler 通过 `State<Arc<dyn TaskApi>>` 提取依赖，不再触达 `state.container`，
/// 测试可注入内存实现（见 `web/routes/tasks.rs` 模块测试）。
#[async_trait::async_trait]
pub trait TaskApi: Send + Sync {
    /// 列出所有任务摘要（按 `.order.json` 排序）。
    async fn list_all_tasks(&self) -> Vec<TaskSummary>;
    /// 加载单个任务的完整配置。
    async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError>;
    /// 按任务 ID 将浏览器任务配置嵌入调试参数的 `task_config` 键。
    async fn embed_task_config(&self, task_id: &str, params: &mut serde_json::Value) -> bool;
    /// 保存任务（创建或更新）。
    async fn save_task(&self, task_id: &str, task: &TaskKind) -> Result<(), TaskError>;
    /// 删除任务。
    async fn delete_task(&self, task_id: &str) -> Result<(), TaskError>;
    /// 获取当前活跃任务 ID。
    async fn get_active_task(&self) -> String;
    /// 设置活跃任务。
    async fn set_active_task(&self, task_id: &str) -> Result<(), TaskError>;
    /// 获取任务详情（摘要 + 完整配置）。
    async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError>;
    /// 读取任务排序数据。
    async fn load_order(&self) -> OrderData;
    /// 保存任务排序数据。
    async fn save_order(&self, order: &OrderData) -> Result<(), TaskError>;
    /// 返回脚本任务的磁盘文件路径（内联内容模式返回 None）。
    async fn get_script_path(&self, task_id: &str) -> Option<std::path::PathBuf>;
    /// 任务是否存在（同步内存/磁盘检查）。
    fn has_task(&self, task_id: &str) -> bool;
    /// 校验任务 JSON（不落盘）：AI 任务生成等外部来源在入库前的程序化校验入口。
    ///
    /// 默认放行仅供测试 mock 免实现；真实实现必须转发 `TaskManager::validate_task`
    /// 的强校验，否则外部来源的非法任务会绕过 `save_task` 之外的预检。
    async fn validate_task_json(&self, _config: &serde_json::Value) -> Result<(), Vec<String>> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl TaskApi for TaskManager {
    async fn list_all_tasks(&self) -> Vec<TaskSummary> {
        TaskManager::list_all_tasks(self).await
    }

    async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
        TaskManager::load_task(self, task_id).await
    }

    async fn embed_task_config(&self, task_id: &str, params: &mut serde_json::Value) -> bool {
        TaskManager::embed_task_config(self, task_id, params).await
    }

    async fn save_task(&self, task_id: &str, task: &TaskKind) -> Result<(), TaskError> {
        TaskManager::save_task(self, task_id, task).await
    }

    async fn delete_task(&self, task_id: &str) -> Result<(), TaskError> {
        TaskManager::delete_task(self, task_id).await
    }

    async fn get_active_task(&self) -> String {
        TaskManager::get_active_task(self).await
    }

    async fn set_active_task(&self, task_id: &str) -> Result<(), TaskError> {
        TaskManager::set_active_task(self, task_id).await
    }

    async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
        TaskManager::get_task_detail(self, task_id).await
    }

    async fn load_order(&self) -> OrderData {
        TaskManager::load_order(self).await
    }

    async fn save_order(&self, order: &OrderData) -> Result<(), TaskError> {
        TaskManager::save_order(self, order).await
    }

    async fn get_script_path(&self, task_id: &str) -> Option<std::path::PathBuf> {
        TaskManager::get_script_path(self, task_id).await
    }

    fn has_task(&self, task_id: &str) -> bool {
        TaskManager::has_task(self, task_id)
    }

    async fn validate_task_json(&self, config: &serde_json::Value) -> Result<(), Vec<String>> {
        TaskManager::validate_task(self, config)
    }
}

/// Web 层消费的任务执行抽象（M1：tasks 域伴生，仅统一入口一个方法）
///
/// `execute_task` / `run_script` handler 经 `State<Arc<dyn TaskRunApi>>` 提取，
/// 测试可注入内存实现返回构造的 [`TaskResult`]。
#[async_trait::async_trait]
pub trait TaskRunApi: Send + Sync {
    /// 统一执行入口：按任务类型分派。
    async fn execute(&self, task: &TaskKind) -> Result<TaskResult, TaskError>;
}

#[async_trait::async_trait]
impl TaskRunApi for TaskExecutor {
    async fn execute(&self, task: &TaskKind) -> Result<TaskResult, TaskError> {
        TaskExecutor::execute(self, task).await
    }
}

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
    #[error("不能删除默认任务 default（系统内置登录任务）")]
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
