//! NDJSON 协议：IpcRequest / IpcResponse / IpcEvent

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Rust → Python 的请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    /// 请求 ID（自增）
    pub id: u64,
    /// 命令名
    pub method: String,
    /// 命令参数（JSON 对象）
    pub params: Value,
}

/// Rust → Python 的取消通知（无 id、无响应）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelNotification {
    /// 要取消的 cancel_id（UUID）
    pub cancel: String,
}

/// Python → Rust 的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    /// 对应请求的 ID
    pub id: u64,
    /// 执行结果
    pub result: IpcResult,
}

/// 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResult {
    /// 是否成功
    pub success: bool,
    /// 成功时的数据（含 StructuredResult）
    pub data: Value,
    /// 失败时的错误消息
    pub error: Option<String>,
}

/// Python → Rust 的事件推送（无 id）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEvent {
    /// 事件类型（step_progress / screenshot / dialog，均为 Python 侧实际 emit 的事件）
    pub event: String,
    /// 事件特有字段
    #[serde(default)]
    pub data: Value,
}

/// 从 IpcResponse.data 中提取的结构化结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredResult {
    /// 结果分类
    pub outcome: Outcome,
    /// 人类可读消息
    pub message: String,
    /// 兼容字段（部分调用方读取 result.data 整体）
    #[serde(default)]
    pub data: Value,
    /// 截图路径
    #[serde(default)]
    pub screenshot_url: Option<String>,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
}

/// 结果分类枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// 成功
    Success,
    /// 已取消
    Cancelled,
    /// 导航超时（可重试）
    NavigationTimeout,
    /// 选择器失败（可重试）
    SelectorFailed,
    /// 断言失败（assert_text 超时或不匹配）
    AssertionFailed,
    /// 验证码识别失败（按配置）
    CaptchaFailed,
    /// 凭证无效
    InvalidCredential,
    /// 网络错误（可重试；重试前由 should_force_recycle 强制回收 Worker）
    NetworkError,
    /// 未知错误（终态失败：classify 在 try_retry 之前终结，不触发回收）
    UnknownError,
}
