//! 调度路由：定时任务 CRUD + 历史
//!
//! M1 细粒度 state（scheduler 域）：handler 直接声明 `State<Arc<dyn SchedulerApi>>`
//! 依赖（经 AppState 的 FromRef 委派提取），不再触达 `state.container`，
//! 测试可注入内存实现做 handler 级单测（见模块测试）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;

use crate::scheduler::{SchedulerApi, SchedulerError};
use crate::web::error::{ApiError, data};

/// GET /api/scheduler/jobs — 列出全部定时任务
///
/// 返回时补充 `task_type` 展示字段（由 target 关联的任务类型推导），
/// 以及 `schedule_invalid`（cron 表达式解析失败、enabled 却永不触发的标记）。
pub async fn list_jobs(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
) -> Result<Json<Value>, ApiError> {
    let jobs = scheduler.list_tasks();
    let mut result = Vec::with_capacity(jobs.len());
    for job in jobs {
        let mut v = serde_json::to_value(&job)?;
        // ScheduledTask.id 标记 #[serde(skip)]（id 从文件名 stem 推导，不参与 JSON 落盘），
        // 此处必须显式回填，否则前端 task.id 为 undefined，查看历史/运行/编辑/删除/启停
        // 等所有按 id 的行级操作全部失效（曾表现为"查看历史"点击无任何反应）。
        v["id"] = serde_json::json!(job.id);
        if let Some(tt) = scheduler.task_type_of(&job.target_id).await {
            v["task_type"] = serde_json::json!(tt);
        }
        v["schedule_invalid"] = serde_json::json!(scheduler.is_cron_invalid(&job.id));
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
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Json(body): Json<JobCreateBody>,
) -> Result<Json<Value>, ApiError> {
    if scheduler.get_task(&body.id).is_some() {
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
    scheduler.save_task(&body.id, &job).await?;
    scheduler.notify_change();
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
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
    Json(body): Json<JobUpdateBody>,
) -> Result<Json<Value>, ApiError> {
    let mut job = scheduler
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
    scheduler.save_task(&id, &job).await?;
    scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

/// GET /api/scheduler/jobs/{id} — 查询单个定时任务
pub async fn get_job(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let job = scheduler
        .get_task(&id)
        .ok_or_else(|| ApiError::NotFound(format!("定时任务 {} 不存在", id)))?;
    let mut v = serde_json::to_value(&job)?;
    // 同 list_jobs：id 被 serde(skip)，显式回填保证单个响应也带 id
    v["id"] = serde_json::json!(job.id);
    // 补充 task_type 展示字段（由 target 关联任务类型推导）
    if let Some(tt) = scheduler.task_type_of(&job.target_id).await {
        v["task_type"] = serde_json::json!(tt);
    }
    Ok(data(v))
}

/// DELETE /api/scheduler/jobs/{id} — 删除定时任务
pub async fn delete_job(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    scheduler.delete_task(&id).await?;
    scheduler.notify_change();
    Ok(data(Value::String("ok".into())))
}

/// POST /api/scheduler/jobs/{id}/toggle — 切换启用/禁用
pub async fn toggle_job(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let job = scheduler
        .get_task(&id)
        .ok_or_else(|| ApiError::NotFound(format!("定时任务 {} 不存在", id)))?;
    let new_enabled = !job.enabled;
    scheduler.toggle_task(&id, new_enabled).await?;
    scheduler.notify_change();
    Ok(data(serde_json::json!({ "enabled": new_enabled })))
}

/// POST /api/scheduler/jobs/{id}/run — 手动触发定时任务
pub async fn run_job(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = scheduler
        .get_task(&id)
        .ok_or_else(|| SchedulerError::TaskNotFound(id.clone()))?;
    // 手动触发与 cron 触发共用同一并发信号量闸（原 run_id 为死数据，不再生成/返回）
    scheduler.spawn_manual_run(task);
    Ok(data(Value::String("ok".into())))
}

/// GET /api/scheduler/jobs/{id}/history — 读取任务执行历史
///
/// id 校验、读盘与字段映射内聚在 `SchedulerApi::read_history`（调度域），
/// handler 仅做转发；前端期望扁平数组 `[{ run_at, success, message }]`。
pub async fn job_history(
    State(scheduler): State<Arc<dyn SchedulerApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let items = scheduler.read_history(&id).await?;
    Ok(data(Value::Array(items)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注：map_history_records 纯函数及其测试已随 read_history 内聚迁至
    // src/scheduler/task.rs（M11），此处仅保留 handler 级单测。

    // ============ handler 级单测（内存 MockScheduler，M1） ============

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt; // oneshot

    use crate::scheduler::task::ScheduledTask;

    #[derive(Default)]
    struct MockInner {
        tasks: Vec<ScheduledTask>,
        notify_calls: usize,
        manual_run_ids: Vec<String>,
    }

    /// 内存 SchedulerApi：无需磁盘与完整 ServiceContainer
    struct MockScheduler(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl SchedulerApi for MockScheduler {
        fn list_tasks(&self) -> Vec<ScheduledTask> {
            self.0.lock().unwrap().tasks.clone()
        }

        fn get_task(&self, id: &str) -> Option<ScheduledTask> {
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .find(|t| t.id == id)
                .cloned()
        }

        async fn task_type_of(&self, _target_id: &str) -> Option<&'static str> {
            Some("script")
        }

        async fn save_task(&self, id: &str, task: &ScheduledTask) -> Result<(), SchedulerError> {
            let mut inner = self.0.lock().unwrap();
            if let Some(existing) = inner.tasks.iter_mut().find(|t| t.id == id) {
                *existing = task.clone();
            } else {
                inner.tasks.push(task.clone());
            }
            Ok(())
        }

        async fn delete_task(&self, id: &str) -> Result<(), SchedulerError> {
            let mut inner = self.0.lock().unwrap();
            match inner.tasks.iter().position(|t| t.id == id) {
                Some(idx) => {
                    inner.tasks.remove(idx);
                    Ok(())
                }
                None => Err(SchedulerError::TaskNotFound(id.to_string())),
            }
        }

        async fn toggle_task(&self, id: &str, enabled: bool) -> Result<(), SchedulerError> {
            let mut inner = self.0.lock().unwrap();
            let t = inner
                .tasks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| SchedulerError::TaskNotFound(id.to_string()))?;
            t.enabled = enabled;
            Ok(())
        }

        fn notify_change(&self) {
            self.0.lock().unwrap().notify_calls += 1;
        }

        fn is_cron_invalid(&self, _id: &str) -> bool {
            false
        }

        fn spawn_manual_run(&self, task: ScheduledTask) {
            self.0.lock().unwrap().manual_run_ids.push(task.id);
        }

        async fn read_history(&self, id: &str) -> Result<Vec<Value>, ApiError> {
            // 与真实实现对齐：非法 id 返回 400（路径穿越测试依赖此行为）
            if !crate::scheduler::task::ScheduledTask::is_valid_id(id) {
                return Err(ApiError::BadRequest(format!("非法任务 ID: {id}")));
            }
            Ok(Vec::new())
        }
    }

    fn sample_task(id: &str, enabled: bool) -> ScheduledTask {
        ScheduledTask {
            id: id.into(),
            name: format!("任务 {id}"),
            description: String::new(),
            cron: "0 8 * * *".into(),
            target_id: "t1".into(),
            profile_id: None,
            timeout: None,
            enabled,
            last_run: None,
            last_result: None,
        }
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            tasks: vec![sample_task("job1", true), sample_task("job2", false)],
            notify_calls: 0,
            manual_run_ids: Vec::new(),
        }));
        let api: Arc<dyn SchedulerApi> = Arc::new(MockScheduler(inner.clone()));
        let app = axum::Router::new()
            .route("/api/scheduler/jobs", get(list_jobs).post(create_job))
            .route(
                "/api/scheduler/jobs/{id}",
                get(get_job).put(update_job).delete(delete_job),
            )
            .route("/api/scheduler/jobs/{id}/toggle", post(toggle_job))
            .route("/api/scheduler/jobs/{id}/run", post(run_job))
            .route("/api/scheduler/jobs/{id}/history", get(job_history))
            .with_state(api);
        (app, inner)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 列表补充 task_type 与 schedule_invalid 展示字段
    #[tokio::test]
    async fn test_list_jobs_enriches_fields() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scheduler/jobs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let arr = v.get("data").and_then(|d| d.as_array()).unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["task_type"], "script");
        assert_eq!(arr[0]["schedule_invalid"], false);
    }

    /// 创建重复 id 返回 409
    #[tokio::test]
    async fn test_create_job_conflict_on_existing_id() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scheduler/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "job1", "target_id": "t1", "cron": "0 8 * * *"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // 未调用 save/notify
        assert_eq!(inner.lock().unwrap().notify_calls, 0);
        assert_eq!(inner.lock().unwrap().tasks.len(), 2);
    }

    /// 创建新任务成功且通知主循环
    #[tokio::test]
    async fn test_create_job_ok() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scheduler/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "job3", "target_id": "t1", "cron": "0 9 * * *", "name": "新任务"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        assert_eq!(inner.tasks.len(), 3);
        assert_eq!(inner.notify_calls, 1);
    }

    /// 更新不存在的任务返回 404
    #[tokio::test]
    async fn test_update_job_not_found() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/scheduler/jobs/missing")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"cron": "0 7 * * *"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 更新已有任务：字段合并后落盘
    #[tokio::test]
    async fn test_update_job_merges_fields() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/scheduler/jobs/job1")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"cron": "30 6 * * *", "enabled": false}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        let job = inner.tasks.iter().find(|t| t.id == "job1").unwrap();
        assert_eq!(job.cron, "30 6 * * *");
        assert!(!job.enabled);
        // 未指定的字段保留原值
        assert_eq!(job.name, "任务 job1");
    }

    /// 删除不存在的任务返回 404
    #[tokio::test]
    async fn test_delete_job_not_found() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/scheduler/jobs/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 删除已有任务成功
    #[tokio::test]
    async fn test_delete_job_ok() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/scheduler/jobs/job1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        assert!(inner.tasks.iter().all(|t| t.id != "job1"));
        assert_eq!(inner.notify_calls, 1);
    }

    /// toggle 翻转启用状态并返回新值
    #[tokio::test]
    async fn test_toggle_job_flips_enabled() {
        let (app, inner) = mock_app();
        // job1 当前 enabled=true → 翻转为 false
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scheduler/jobs/job1/toggle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["enabled"], false);
        assert!(!inner.lock().unwrap().tasks[0].enabled);
    }

    /// 手动触发调用 spawn_manual_run
    #[tokio::test]
    async fn test_run_job_triggers_manual_run() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scheduler/jobs/job1/run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().manual_run_ids, vec!["job1"]);
    }

    /// 非法任务 ID（路径穿越）返回 400
    #[tokio::test]
    async fn test_job_history_rejects_invalid_id() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scheduler/jobs/..%5Cevil/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 历史文件不存在时返回空数组
    #[tokio::test]
    async fn test_job_history_missing_file_is_empty() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scheduler/jobs/job1/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"], serde_json::json!([]));
    }
}
