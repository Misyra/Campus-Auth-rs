//! 调度路由：定时任务 CRUD + 历史

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/scheduler/jobs — 列出全部定时任务
///
/// 返回时补充 `task_type` 展示字段（由 target 关联的任务类型推导），
/// 以及 `schedule_invalid`（cron 表达式解析失败、enabled 却永不触发的标记）。
pub async fn list_jobs(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let jobs = state.container.scheduler.list_tasks();
    let mut result = Vec::with_capacity(jobs.len());
    for job in jobs {
        let mut v = serde_json::to_value(&job)?;
        if let Some(tt) = state.container.scheduler.task_type_of(&job.target_id).await {
            v["task_type"] = serde_json::json!(tt);
        }
        v["schedule_invalid"] = serde_json::json!(state.container.scheduler.is_cron_invalid(&job.id));
        result.push(v);
    }
    Ok(data(result))
}

#[derive(Deserialize)]
pub struct JobCreateBody {
    pub id: String,
    pub name: Option<String>,
    pub target_id: String,
    pub cron: String,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub timeout: Option<u64>,
}

/// POST /api/scheduler/jobs — 创建定时任务
///
/// 任务类型由 `target_id` 关联的目标任务权威推导，不再单独存储。
/// 已存在的 id 返回 409（`save_task` 为 upsert 语义，此处显式拒绝静默覆盖）。
pub async fn create_job(
    State(state): State<AppState>,
    Json(body): Json<JobCreateBody>,
) -> Result<Json<Value>, ApiError> {
    if state.container.scheduler.get_task(&body.id).is_some() {
        return Err(ApiError::Conflict(format!("定时任务 {} 已存在", body.id)));
    }
    let job = crate::scheduler::task::ScheduledTask {
        id: body.id.clone(),
        name: body.name.unwrap_or_default(),
        description: body.description.unwrap_or_default(),
        cron: body.cron,
        target_id: body.target_id,
        profile_id: None,
        timeout: body.timeout,
        enabled: body.enabled.unwrap_or(true),
        last_run: None,
        last_result: None,
    };
    state.container.scheduler.save_task(&body.id, &job).await?;
    state.container.scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

#[derive(Deserialize)]
pub struct JobUpdateBody {
    pub cron: Option<String>,
    pub enabled: Option<bool>,
    pub name: Option<String>,
    pub target_id: Option<String>,
    pub profile_id: Option<String>,
    pub description: Option<String>,
    pub timeout: Option<u64>,
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
    if let Some(p) = body.profile_id {
        job.profile_id = Some(p);
    }
    if let Some(d) = body.description {
        job.description = d;
    }
    if let Some(t) = body.timeout {
        job.timeout = Some(t);
    }
    state.container.scheduler.save_task(&id, &job).await?;
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
    let mut v = serde_json::to_value(&job)?;
    // 补充 task_type 展示字段（由 target 关联任务类型推导）
    if let Some(tt) = state.container.scheduler.task_type_of(&job.target_id).await {
        v["task_type"] = serde_json::json!(tt);
    }
    Ok(data(v))
}

/// DELETE /api/scheduler/jobs/{id} — 删除定时任务
pub async fn delete_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.scheduler.delete_task(&id).await?;
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
        .toggle_task(&id, new_enabled)
        .await?;
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
    // 手动触发与 cron 触发共用同一并发信号量闸（原 run_id 为死数据，不再生成/返回）
    state.container.scheduler.spawn_manual_run(task);
    Ok(data(Value::String("ok".into())))
}

/// GET /api/scheduler/jobs/{id}/history — 读取任务执行历史
///
/// 前端期望扁平数组 `[{ run_at, success, message }]`，
/// 将磁盘存储的 `{ runs: [{ timestamp, status, message, duration }] }` 做字段映射。
pub async fn job_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // id 直接拼接路径，必须先校验防止路径穿越读取任意 .json
    if !crate::scheduler::task::ScheduledTask::is_valid_id(&id) {
        return Err(ApiError::BadRequest(format!("非法任务 ID: {id}")));
    }
    let history_dir = state.container.scheduler.history_dir();
    let path = history_dir.join(format!("{}.json", id));
    let items = if path.exists() {
        let content = tokio::fs::read_to_string(&path).await?;
        let raw: Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({"runs": []}));
        map_history_records(&raw)
    } else {
        Vec::new()
    };
    Ok(data(Value::Array(items)))
}

/// 将磁盘历史 `{ "runs": [{ timestamp, status, message, ... }] }` 映射为前端扁平数组
/// `[{ run_at, success, message }]`。`success` 由 `status == "success"` 推导。
fn map_history_records(raw: &Value) -> Vec<Value> {
    let runs = raw.get("runs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    runs
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ 任务历史记录字段映射 ============

    #[test]
    fn map_history_lossy_mapping() {
        let raw = serde_json::json!({
            "runs": [
                { "timestamp": "2026-08-14T01:00:00Z", "status": "success", "message": "完成", "duration": 1.2 },
                { "timestamp": "2026-08-14T02:00:00Z", "status": "error", "message": "失败" },
                { "status": "success" },
                { "message": "无状态" },
            ]
        });
        let mapped = map_history_records(&raw);
        assert_eq!(mapped.len(), 4);
        // success 由 status == "success" 推导
        assert_eq!(mapped[0]["success"], serde_json::json!(true));
        assert_eq!(mapped[1]["success"], serde_json::json!(false));
        // 无 status 时 success 为 false；无 timestamp 时为 null
        assert_eq!(mapped[2]["success"], serde_json::json!(true));
        assert_eq!(mapped[2]["run_at"], Value::Null);
        assert_eq!(mapped[3]["success"], serde_json::json!(false));
        assert_eq!(mapped[3]["message"], serde_json::json!("无状态"));
    }

    #[test]
    fn map_history_missing_or_empty_runs() {
        // 无 runs 字段 → 空数组
        assert_eq!(map_history_records(&serde_json::json!({})), Vec::<Value>::new());
        // runs 为空数组 → 空数组
        assert_eq!(map_history_records(&serde_json::json!({"runs": []})), Vec::<Value>::new());
        // runs 非数组 → 空数组
        assert_eq!(map_history_records(&serde_json::json!({"runs": "x"})), Vec::<Value>::new());
    }
}
