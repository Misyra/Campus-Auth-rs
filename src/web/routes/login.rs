//! 登录路由：触发登录、取消登录、查询状态、一次性登录

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::status::LoginSource;
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// POST /api/login — 触发手动登录
///
/// 前端可能发送 `null` 或 `{source, task_id}`，用 `Json<Value>` 宽松接收。
pub async fn trigger_login(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some("browser") => LoginSource::Browser,
        _ => LoginSource::Manual,
    };
    let task_id = body
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let profile_id = body
        .get("profile_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let handle = state.container.login.submit(source, task_id, profile_id).await;
    let result = handle.await_result().await;
    // 登录失败（配置缺失、auth_url 不可达、凭证无效等）是预期业务结果而非服务端错误，
    // 统一以 200 + {success, message, duration} 返回，避免前端把环境性失败当作 500 异常。
    Ok(data(serde_json::json!({
        "success": result.success,
        "message": result.message,
        "duration": result.duration.as_secs_f64(),
    })))
}

/// POST /api/login/cancel — 取消当前登录流程
pub async fn cancel_login(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    state.container.login.cancel();
    Ok(data(Value::String("已取消".into())))
}

/// GET /api/login/status — 查询当前登录状态
pub async fn get_login_status(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let snapshot = state.container.status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
}

/// POST /api/login/once — login_once 模式（执行一次登录后退出）
///
/// 与 trigger_login 保持一致：登录失败返回 200 + {success: false}，而非 500。  
pub async fn login_once(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some("browser") => LoginSource::Browser,
        _ => LoginSource::LoginOnce,
    };
    let task_id = body
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let profile_id = body
        .get("profile_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let handle = state.container.login.submit(source, task_id, profile_id).await;
    let result = handle.await_result().await;
    Ok(data(serde_json::json!({
        "once": true,
        "success": result.success,
        "message": result.message,
    })))
}
