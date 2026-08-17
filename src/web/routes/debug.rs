//! 调试路由：调试会话管理（通过 Bridge 执行调试命令）
//!
//! M1 细粒度 state（tasks 域）：`start_debug` 经 `State<Arc<dyn TaskApi>>`
//! 嵌入任务配置；Bridge 为独立域尚未 trait 化，仍经 `state.container` 触达。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::tasks::TaskApi;
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// POST /api/debug/start — 启动调试会话
///
/// 前端仅提供 `task_id` 时，按 id 加载浏览器任务配置并嵌入 `task_config` 键，
/// 与 Python Worker 的 `debug_start` 契约一致（步骤载体为 `task_config`）。
pub async fn start_debug(
    State(tasks): State<Arc<dyn TaskApi>>,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mut params = body.clone();
    if let Some(task_id) = body.get("task_id").and_then(|v| v.as_str()) {
        // 显式传入 task_config 时不覆盖；否则按 id 嵌入浏览器任务配置
        if !task_id.is_empty() && body.get("task_config").is_none() {
            tasks.embed_task_config(task_id, &mut params).await;
        }
    }
    let result = state.container.bridge.execute("debug_start", params).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// POST /api/debug/step — 调试单步执行
///
/// 透传请求体（可携带 `step` 完整配置或 `step_index`）。前端“下一步”无显式索引时，
/// 由 Python Worker 依据会话内游标自动执行下一个尚未运行的步骤。
pub async fn step_debug(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let params = if body.is_null() {
        serde_json::json!({})
    } else {
        body
    };
    let result = state
        .container
        .bridge
        .execute("debug_step", params)
        .await?;
    Ok(data(serde_json::to_value(result)?))
}

/// POST /api/debug/stop — 停止调试会话
pub async fn stop_debug(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .container
        .bridge
        .execute("debug_stop", Value::Null)
        .await?;
    Ok(data(serde_json::to_value(result)?))
}

/// POST /api/debug/run-all — 执行调试会话中全部剩余步骤
pub async fn run_all(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .container
        .bridge
        .execute("debug_run_all", Value::Null)
        .await?;
    Ok(data(serde_json::to_value(result)?))
}
