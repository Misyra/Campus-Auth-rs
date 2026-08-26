//! 自启动路由：自启动状态、启用/禁用、模式
//!
//! M1 细粒度 state（config 域）：handler 声明 `State<Arc<dyn ConfigApi>>` 依赖，
//! 不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};

#[derive(Deserialize)]
pub struct AutostartModeBody {
    /// 自启动模式（如 "login_once" / "monitor" / "none"）
    pub runtime_mode: String,
}

/// GET /api/autostart/status — 获取自启动状态
pub async fn get_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    let enabled = settings.global.app.autostart_enabled;
    let runtime_mode = serde_json::to_value(&settings.global.app.startup_action)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "monitor".to_string());
    let method = if enabled {
        match std::env::consts::OS {
            "windows" => "Registry",
            "macos" => "LaunchAgent",
            _ => "desktop file",
        }
    } else {
        "-"
    };
    Ok(data(serde_json::json!({
        "platform": std::env::consts::OS,
        "enabled": enabled,
        "method": method,
        "location": "",
        "runtime_mode": runtime_mode,
    })))
}

/// POST /api/autostart/enable — 启用自启动
pub async fn enable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = config.load_settings_async().await;
    settings.global.app.autostart_enabled = true;
    config.save_settings(&settings).await?;
    // 真正注册系统自启动（schtasks 计划任务）
    register_self_start(true).await?;
    Ok(data(serde_json::json!({ "message": "已启用开机自启动" })))
}

/// POST /api/autostart/disable — 禁用自启动
pub async fn disable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = config.load_settings_async().await;
    settings.global.app.autostart_enabled = false;
    config.save_settings(&settings).await?;
    // 真正取消系统自启动注册
    register_self_start(false).await?;
    Ok(data(serde_json::json!({ "message": "已禁用开机自启动" })))
}

/// 注册/取消系统自启动（同步阻塞 I/O，置于 spawn_blocking 中执行）
///
/// 仅在配置标志变更后调用，确保“开关状态”与系统实际注册一致。
async fn register_self_start(enabled: bool) -> Result<(), ApiError> {
    let join = tokio::task::spawn_blocking(move || crate::utils::platform::set_self_start(enabled));
    match join.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ApiError::Internal(format!("注册自启动失败: {e}"))),
        Err(e) => Err(ApiError::Internal(format!("自启动注册任务异常: {e}"))),
    }
}

/// POST /api/autostart/mode — 设置自启动模式（startup_action）
pub async fn set_autostart_mode(
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<AutostartModeBody>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = config.load_settings_async().await;
    let action = match body.runtime_mode.as_str() {
        "login_once" => crate::config::StartupAction::LoginOnce,
        "monitor" => crate::config::StartupAction::Monitor,
        "none" => crate::config::StartupAction::None,
        other => return Err(ApiError::BadRequest(format!("未知的自启动模式: {other}"))),
    };
    settings.global.app.startup_action = action;
    config.save_settings(&settings).await?;
    Ok(data(serde_json::json!({ "message": "自启动模式已更新" })))
}
