//! 调度路由：定时任务 CRUD + 历史

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::scheduler::task::ScheduledTaskType;
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/scheduler/jobs — 列出全部定时任务
pub async fn list_jobs(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let jobs = state.container.scheduler.list_tasks();
    Ok(data(jobs))
}

#[derive(Deserialize)]
pub struct JobCreateBody {
    pub id: String,
    pub name: Option<String>,
    pub target_id: String,
    pub cron: String,
    pub task_type: Option<String>,
    pub enabled: Option<bool>,
}

/// POST /api/scheduler/jobs — 创建定时任务
pub async fn create_job(
    State(state): State<AppState>,
    Json(body): Json<JobCreateBody>,
) -> Result<Json<Value>, ApiError> {
    let task_type = match body.task_type.as_deref() {
        Some("script") => ScheduledTaskType::Script,
        Some("shell") => ScheduledTaskType::Shell,
        _ => ScheduledTaskType::Browser,
    };
    let job = crate::scheduler::task::ScheduledTask {
        id: body.id.clone(),
        name: body.name.unwrap_or_default(),
        description: String::new(),
        cron: body.cron,
        target_id: body.target_id,
        task_type,
        profile_id: None,
        timeout: None,
        args: vec![],
        work_dir: None,
        enabled: body.enabled.unwrap_or(true),
        last_run: None,
        last_result: None,
    };
    state.container.scheduler.save_task(&body.id, &job)?;
    state.container.scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

#[derive(Deserialize)]
pub struct JobUpdateBody {
    pub cron: Option<String>,
    pub enabled: Option<bool>,
    pub name: Option<String>,
    pub target_id: Option<String>,
    pub task_type: Option<String>,
    pub profile_id: Option<String>,
    pub description: Option<String>,
    pub timeout: Option<u64>,
    pub args: Option<Vec<String>>,
    pub work_dir: Option<String>,
}

/// PUT /api/scheduler/jobs/{id} — 更新定时任务
pub async fn update_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<JobUpdateBody>,
) -> Result<Json<Value>, ApiError> {
    let mut job = state
        .container
        .scheduler
        .get_task(&id)
        .ok_or_else(|| ApiError::NotFound(format!("定时任务 {} 不存在", id)))?;
    if let Some(c) = body.cron {
        job.cron = c;
    }
    if let Some(e) = body.enabled {
        job.enabled = e;
    }
    if let Some(n) = body.name {
        job.name = n;
    }
    if let Some(t) = body.target_id {
        job.target_id = t;
    }
    if let Some(tt) = body.task_type {
        job.task_type = match tt.as_str() {
            "script" => ScheduledTaskType::Script,
            "shell" => ScheduledTaskType::Shell,
            _ => ScheduledTaskType::Browser,
        };
    }
    if let Some(p) = body.profile_id {
        job.profile_id = Some(p);
    }
    if let Some(d) = body.description {
        job.description = d;
    }
    if let Some(t) = body.timeout {
        job.timeout = Some(t);
    }
    if let Some(a) = body.args {
        job.args = a;
    }
    if let Some(w) = body.work_dir {
        job.work_dir = Some(w);
    }
    state.container.scheduler.save_task(&id, &job)?;
    state.container.scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

/// GET /api/scheduler/jobs/{id} — 查询单个定时任务
pub async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let job = state
        .container
        .scheduler
        .get_task(&id)
        .ok_or_else(|| ApiError::NotFound(format!("定时任务 {} 不存在", id)))?;
    Ok(data(serde_json::to_value(&job)?))
}

/// DELETE /api/scheduler/jobs/{id} — 删除定时任务
pub async fn delete_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.scheduler.delete_task(&id)?;
    state.container.scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

/// POST /api/scheduler/jobs/{id}/toggle — 切换启用/禁用
pub async fn toggle_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let job = state
        .container
        .scheduler
        .get_task(&id)
        .ok_or_else(|| ApiError::NotFound(format!("定时任务 {} 不存在", id)))?;
    let new_enabled = !job.enabled;
    state
        .container
        .scheduler
        .toggle_task(&id, new_enabled)?;
    state.container.scheduler.notify_change();
    Ok(data(serde_json::json!({ "enabled": new_enabled })))
}

/// POST /api/scheduler/jobs/{id}/run — 手动触发定时任务
pub async fn run_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = state
        .container
        .scheduler
        .get_task(&id)
        .ok_or_else(|| crate::scheduler::SchedulerError::TaskNotFound(id.clone()))?;
    let run_id = Uuid::new_v4().to_string();
    let svc = state.container.scheduler.clone();
    tokio::spawn(async move {
        crate::scheduler::execute_scheduled_task(task, svc).await;
    });
    Ok(data(serde_json::json!({ "run_id": run_id })))
}

/// GET /api/scheduler/jobs/{id}/history — 读取任务执行历史
///
/// 前端期望扁平数组 `[{ run_at, success, message }]`，
/// 将磁盘存储的 `{ runs: [{ timestamp, status, message, duration }] }` 做字段映射。
pub async fn job_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let history_dir = state.container.scheduler.history_dir();
    let path = history_dir.join(format!("{}.json", id));
    let items = if path.exists() {
        let content = tokio::fs::read_to_string(&path).await?;
        let raw: Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({"runs": []}));
        // 从 { "runs": [...] } 提取数组并做字段映射
        let runs = raw.get("runs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mapped: Vec<Value> = runs
            .into_iter()
            .map(|record| {
                let run_at = record.get("timestamp").cloned().unwrap_or(Value::Null);
                let success = record
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "success")
                    .unwrap_or(false);
                let message = record.get("message").cloned().unwrap_or(Value::Null);
                serde_json::json!({
                    "run_at": run_at,
                    "success": success,
                    "message": message
                })
            })
            .collect();
        mapped
    } else {
        Vec::new()
    };
    Ok(data(items))
}
