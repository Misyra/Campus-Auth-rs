//! 任务路由：自定义任务 CRUD + 导入导出 + 排序

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/tasks — 列出全部自定义任务
pub async fn list_tasks(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let tasks = state.container.tasks.list_all_tasks().await;
    Ok(data(tasks))
}

/// GET /api/tasks/active — 获取当前活跃任务
pub async fn get_active_task(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let active_id = state.container.tasks.get_active_task().await;
    Ok(data(serde_json::json!({ "task_id": active_id })))
}

/// POST /api/tasks/active/{task_id} — 设置活跃任务
pub async fn set_active_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.tasks.set_active_task(&task_id).await?;
    Ok(data(Value::String("ok".into())))
}

#[derive(Deserialize)]
pub struct TaskCreateBody {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub url: Option<String>,
    pub script: Option<String>,
    pub command: Option<String>,
}

/// POST /api/tasks — 创建任务
pub async fn create_task(
    State(state): State<AppState>,
    Json(body): Json<TaskCreateBody>,
) -> Result<Json<Value>, ApiError> {
    let kind = match body.kind.as_deref() {
        Some("shell") => crate::tasks::TaskKind::Shell(crate::tasks::ShellTaskConfig {
            common: crate::tasks::CommonFields {
                task_id: body.id.clone(),
                name: body.name,
                description: String::new(),
            },
            command: body.command.unwrap_or_default(),
            timeout: 300,
            shell_path: None,
        }),
        Some("script") => crate::tasks::TaskKind::Script(crate::tasks::ScriptTaskConfig {
            common: crate::tasks::CommonFields {
                task_id: body.id.clone(),
                name: body.name,
                description: String::new(),
            },
            content: body.script,
            ..Default::default()
        }),
        _ => crate::tasks::TaskKind::Browser(crate::tasks::TaskConfig {
            common: crate::tasks::CommonFields {
                task_id: body.id.clone(),
                name: body.name,
                description: String::new(),
            },
            url: body.url.unwrap_or_default(),
            ..Default::default()
        }),
    };
    state
        .container
        .tasks
        .save_task(&body.id, &kind)
        .await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/tasks/{id} — 获取单个任务
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !state.container.tasks.has_task(&id) {
        return Err(ApiError::NotFound(format!("任务 {} 不存在", id)));
    }
    let task = state.container.tasks.get_task_detail(&id).await?;
    Ok(data(serde_json::to_value(task)?))
}

/// PUT /api/tasks/{id} — 更新任务
pub async fn update_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let task: crate::tasks::TaskKind = serde_json::from_value(body)?;
    state.container.tasks.save_task(&id, &task).await?;
    Ok(data(Value::String("ok".into())))
}

/// DELETE /api/tasks/{id} — 删除任务
pub async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.tasks.delete_task(&id).await?;
    Ok(data(Value::String("ok".into())))
}

/// 任务排序请求体（对齐前端 `{ all, scripts }` 契约）
#[derive(Deserialize)]
pub struct OrderBody {
    /// 浏览器任务 ID 顺序
    pub all: Vec<String>,
    /// 脚本任务 ID 顺序
    pub scripts: Vec<String>,
}

/// POST /api/tasks/order — 保存任务排序
///
/// 接受前端 `{ all, scripts }` 结构，合并写入内部 `OrderData.order`，
/// 同时保留已持久化的 `active` 字段。
pub async fn order_tasks(
    State(state): State<AppState>,
    Json(body): Json<OrderBody>,
) -> Result<Json<Value>, ApiError> {
    let mut order = state.container.tasks.load_order().await;
    order.order.clear();
    order.order.extend(body.all);
    order.order.extend(body.scripts);
    // 去除可能的重复 ID（all 与 scripts 可能有交叉），保留前端传入顺序
    let mut seen = std::collections::HashSet::new();
    order.order.retain(|id| seen.insert(id.clone()));
    state.container.tasks.save_order(&order).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/tasks/import — 导入任务（支持单个对象或数组）
pub async fn import_tasks(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let items = match body.as_array() {
        Some(arr) => arr.clone(),
        None => vec![body],
    };
    let mut imported = 0u32;
    for item in items {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let task: crate::tasks::TaskKind = serde_json::from_value(item)?;
        state.container.tasks.save_task(&id, &task).await?;
        imported += 1;
    }
    Ok(data(serde_json::json!({ "imported": imported })))
}

/// GET /api/tasks/export/{id} — 导出指定任务的完整配置
pub async fn export_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !state.container.tasks.has_task(&id) {
        return Err(ApiError::NotFound(format!("任务 {} 不存在", id)));
    }
    let detail = state.container.tasks.get_task_detail(&id).await?;
    Ok(data(serde_json::to_value(detail)?))
}

/// POST /api/tasks/{id}/execute — 手动执行任务（通用语义：浏览器/脚本/Shell）
///
/// 浏览器任务走通用执行（不注入账号密码，用于打卡/签到等日常自动化）；
/// 带凭据的登录语义请走 `POST /api/login`。
pub async fn execute_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = state
        .container
        .tasks
        .load_task(&id)
        .await
        .map_err(|e| ApiError::NotFound(format!("任务不存在: {e}")))?;
    let result = state
        .container
        .executor
        .execute(&task)
        .await
        .map_err(|e| ApiError::Internal(format!("执行失败: {e}")))?;
    Ok(data(serde_json::to_value(&result)?))
}
