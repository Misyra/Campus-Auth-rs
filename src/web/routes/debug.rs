//! 调试路由：调试会话管理（通过 Bridge 执行调试命令）
//!
//! M1 细粒度 state：`start_debug` 经 `State<Arc<dyn TaskApi>>` 嵌入任务配置，
//! Bridge 命令派发经 `State<Arc<dyn BridgeApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};
use crate::web::state::AppState;

/// POST /api/debug/start — 启动调试会话
///
/// 前端仅提供 `task_id` 时，按 id 加载浏览器任务配置并嵌入 `task_config` 键，
/// 与 Python Worker 的 `debug_start` 契约一致（步骤载体为 `task_config`）。
/// 同时注入活跃 Profile 的系统保留变量，使登录类任务的 {{USERNAME}}/{{LOGIN_URL}}
/// 等模板在调试时的行为与真实执行一致（此前不注入会导航到 `{{LOGIN_URL}}` 字面量）。
pub async fn start_debug(
    State(tasks): State<Arc<dyn TaskApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(bridge): State<Arc<dyn BridgeApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mut params = body.clone();
    if let Some(task_id) = body.get("task_id").and_then(|v| v.as_str()) {
        // 显式传入 task_config 时不覆盖；否则按 id 嵌入浏览器任务配置
        if !task_id.is_empty() && body.get("task_config").is_none() {
            tasks.embed_task_config(task_id, &mut params).await;
        }
    }
    let rt = config.runtime_snapshot();
    let profile = &rt.profile;
    let entry = params
        .as_object_mut()
        .ok_or_else(|| ApiError::BadRequest("调试参数必须为 JSON 对象".into()))?;
    // 下发浏览器设置（与登录/任务执行一致）：缺失时 Worker 按默认 headless=true
    // 启动，导致调试会话永远无窗口，用户的"关闭无头模式"设置对调试不生效
    entry
        .entry("browser_settings".to_string())
        .or_insert_with(|| serde_json::to_value(&rt.browser).unwrap_or(Value::Null));
    // 显式提供的字段不被覆盖（允许调试时临时替换单个变量）
    entry
        .entry("username".to_string())
        .or_insert_with(|| profile.username.clone().into());
    entry
        .entry("password".to_string())
        .or_insert_with(|| profile.password.to_string().into());
    entry
        .entry("isp".to_string())
        .or_insert_with(|| profile.isp.clone().into());
    entry
        .entry("auth_url".to_string())
        .or_insert_with(|| profile.auth_url.clone().into());
    let result = bridge.execute("debug_start", params).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// POST /api/debug/step — 调试单步执行
///
/// 透传请求体（可携带 `step` 完整配置或 `step_index`）。前端“下一步”无显式索引时，
/// 由 Python Worker 依据会话内游标自动执行下一个尚未运行的步骤。
pub async fn step_debug(
    State(bridge): State<Arc<dyn BridgeApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let params = if body.is_null() {
        serde_json::json!({})
    } else {
        body
    };
    let result = bridge.execute("debug_step", params).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// POST /api/debug/stop — 停止调试会话
pub async fn stop_debug(State(bridge): State<Arc<dyn BridgeApi>>) -> Result<Json<Value>, ApiError> {
    let result = bridge.execute("debug_stop", Value::Null).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/debug/status — 查询是否存在活跃调试会话
///
/// 调试会话是 Worker 侧的持久状态：前端刷新/换页后内存状态丢失，界面上无法
/// 停止会话，登录等命令会一直撞上"Worker 忙: 调试会话进行中"。此端点供前端
/// 启动时恢复会话感知（配合"停止调试"入口自助解除占用）。
pub async fn debug_status(State(bridge): State<Arc<dyn BridgeApi>>) -> Json<Value> {
    let screenshot_url = bridge.last_screenshot_url();
    if !bridge.debug_session_active() {
        return Json(json!({ "data": { "active": false, "screenshot_url": screenshot_url } }));
    }
    // 会话活跃：向 Worker 查询完整会话详情（步骤列表/执行结果），无副作用；
    // 前端刷新后据此恢复面板，否则只剩"0/0 无步骤数据"骨架
    let session = match bridge
        .execute_with_timeout("debug_status", json!({}), std::time::Duration::from_secs(5))
        .await
    {
        Ok(resp) => resp.result.data,
        Err(e) => {
            tracing::warn!(target: "python_worker", "调试会话详情查询失败: {e}");
            Value::Null
        }
    };
    Json(json!({
        "data": {
            "active": true,
            "screenshot_url": screenshot_url,
            "session": session,
        }
    }))
}

/// POST /api/debug/run-all — 执行调试会话中全部剩余步骤
pub async fn run_all(State(bridge): State<Arc<dyn BridgeApi>>) -> Result<Json<Value>, ApiError> {
    let result = bridge.execute("debug_run_all", Value::Null).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/debug/screenshot/{filename} — 读取调试截图文件
///
/// `<img>` 引用无法携带自定义鉴权头（同背景图 GET 豁免先例），且 Worker 侧
/// 截图落盘路径对浏览器不可达，必须经 HTTP 暴露；文件名做防穿越校验。
pub async fn debug_screenshot(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use crate::environment::resolve_worker_project_path;
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let dir = resolve_worker_project_path(&state.config.base_path()).join("debug");
    let path = dir.join(&filename);
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("截图 {} 不存在", filename)));
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes))
}
