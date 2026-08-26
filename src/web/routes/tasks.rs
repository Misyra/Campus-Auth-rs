//! 任务路由：自定义任务 CRUD + 导入导出 + 排序
//!
//! M1 细粒度 state（tasks 域）：handler 声明 `State<Arc<dyn TaskApi>>` /
//! `State<Arc<dyn TaskRunApi>>` 依赖（经 AppState 的 FromRef 委派提取），
//! 不再触达 `state.container`，测试可注入内存实现（见模块测试）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::tasks::{TaskApi, TaskRunApi};
use crate::web::error::{ApiError, data};

/// GET /api/tasks — 列出全部自定义任务
pub async fn list_tasks(State(tasks): State<Arc<dyn TaskApi>>) -> Result<Json<Value>, ApiError> {
    let tasks = tasks.list_all_tasks().await;
    Ok(data(tasks))
}

/// GET /api/tasks/active — 获取当前活跃任务
pub async fn get_active_task(
    State(tasks): State<Arc<dyn TaskApi>>,
) -> Result<Json<Value>, ApiError> {
    let active_id = tasks.get_active_task().await;
    Ok(data(serde_json::json!({ "task_id": active_id })))
}

/// POST /api/tasks/active/{task_id} — 设置活跃任务
pub async fn set_active_task(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    tasks.set_active_task(&task_id).await?;
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
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<TaskCreateBody>,
) -> Result<Json<Value>, ApiError> {
    // 共享字段构造一次，三分支复用（原先三处重复构造，TaskKind 访问器收敛）
    let common = crate::tasks::CommonFields {
        task_id: body.id.clone(),
        name: body.name,
        description: String::new(),
    };
    let kind = match body.kind.as_deref() {
        // 与 G6 同语义：缺失/空串/browser → 浏览器任务（向后兼容）；
        // 存在但未知 → 明确 400，不静默回退
        None | Some("") | Some("browser") => {
            crate::tasks::TaskKind::Browser(crate::tasks::TaskConfig {
                common,
                url: body.url.unwrap_or_default(),
                ..Default::default()
            })
        }
        Some("shell") => crate::tasks::TaskKind::Shell(crate::tasks::ShellTaskConfig {
            common: common.clone(),
            command: body.command.unwrap_or_default(),
            timeout: 300,
            shell_path: None,
        }),
        Some("script") => crate::tasks::TaskKind::Script(crate::tasks::ScriptTaskConfig {
            common: common.clone(),
            content: body.script,
            ..Default::default()
        }),
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "未知任务类型 kind: {other}（支持 browser / script / shell）"
            )));
        }
    };
    tasks.save_task(&body.id, &kind).await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/tasks/{id} — 获取单个任务
pub async fn get_task(
    State(task_api): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !task_api.has_task(&id) {
        return Err(ApiError::NotFound(format!("任务 {} 不存在", id)));
    }
    let task = task_api.get_task_detail(&id).await?;
    Ok(data(serde_json::to_value(task)?))
}

/// PUT /api/tasks/{id} — 更新任务
pub async fn update_task(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let task: crate::tasks::TaskKind =
        serde_json::from_value(body).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    tasks.save_task(&id, &task).await?;
    Ok(data(Value::String("ok".into())))
}

/// DELETE /api/tasks/{id} — 删除任务
pub async fn delete_task(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    tasks.delete_task(&id).await?;
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
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<OrderBody>,
) -> Result<Json<Value>, ApiError> {
    let mut order = tasks.load_order().await;
    order.order.clear();
    order.order.extend(body.all);
    order.order.extend(body.scripts);
    // 去除可能的重复 ID（all 与 scripts 可能有交叉），保留前端传入顺序
    let mut seen = std::collections::HashSet::new();
    order.order.retain(|id| seen.insert(id.clone()));
    tasks.save_order(&order).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/tasks/import — 导入任务（支持单个对象或数组）
pub async fn import_tasks(
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let items = match body.as_array() {
        Some(arr) => arr.clone(),
        None => vec![body],
    };
    let mut imported = 0u32;
    let mut failed: Vec<Value> = Vec::new();
    for item in items {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        // 逐条导入：任一条失败不中止整体，收集失败项供前端提示
        let task: crate::tasks::TaskKind = match serde_json::from_value(item) {
            Ok(t) => t,
            Err(e) => {
                failed.push(json!({ "id": id, "reason": e.to_string() }));
                continue;
            }
        };
        match tasks.save_task(&id, &task).await {
            Ok(()) => imported += 1,
            Err(e) => failed.push(json!({ "id": id, "reason": e.to_string() })),
        }
    }
    Ok(data(
        serde_json::json!({ "imported": imported, "failed": failed }),
    ))
}

/// GET /api/tasks/export/{id} — 导出指定任务的完整配置
pub async fn export_task(
    State(task_api): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !task_api.has_task(&id) {
        return Err(ApiError::NotFound(format!("任务 {} 不存在", id)));
    }
    let detail = task_api.get_task_detail(&id).await?;
    Ok(data(serde_json::to_value(detail)?))
}

/// POST /api/tasks/{id}/execute — 手动执行任务（通用语义：浏览器/脚本/Shell）
///
/// 浏览器任务走通用执行（不注入账号密码，用于打卡/签到等日常自动化）；
/// 带凭据的登录语义请走 `POST /api/login`。
pub async fn execute_task(
    State(tasks): State<Arc<dyn TaskApi>>,
    State(runner): State<Arc<dyn TaskRunApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = tasks
        .load_task(&id)
        .await
        .map_err(|e| ApiError::NotFound(format!("任务不存在: {e}")))?;
    let result = runner.execute(&task).await.map_err(ApiError::from)?;
    Ok(data(serde_json::to_value(&result)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt; // oneshot

    use crate::tasks::{OrderData, TaskDetail, TaskError, TaskKind, TaskResult, TaskSummary};

    #[derive(Default)]
    struct MockInner {
        tasks: Vec<(String, TaskKind)>,
        active: String,
        order: OrderData,
        executed: Vec<String>,
    }

    /// 内存 TaskApi：无需磁盘与完整 ServiceContainer（M1）
    struct MockTaskApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<TaskSummary> {
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .map(|(id, kind)| TaskSummary {
                    id: id.clone(),
                    name: kind.common().name.clone(),
                    description: kind.common().description.clone(),
                    task_type: kind.type_name().to_string(),
                })
                .collect()
        }

        async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .find(|(id, _)| id == task_id)
                .map(|(_, k)| k.clone())
                .ok_or_else(|| TaskError::TaskNotFound(task_id.to_string()))
        }

        async fn embed_task_config(&self, _task_id: &str, _params: &mut Value) -> bool {
            false
        }

        async fn save_task(&self, task_id: &str, task: &TaskKind) -> Result<(), TaskError> {
            let mut inner = self.0.lock().unwrap();
            match inner.tasks.iter_mut().find(|(id, _)| id == task_id) {
                Some(slot) => slot.1 = task.clone(),
                None => inner.tasks.push((task_id.to_string(), task.clone())),
            }
            Ok(())
        }

        async fn delete_task(&self, task_id: &str) -> Result<(), TaskError> {
            let mut inner = self.0.lock().unwrap();
            match inner.tasks.iter().position(|(id, _)| id == task_id) {
                Some(idx) => {
                    inner.tasks.remove(idx);
                    Ok(())
                }
                None => Err(TaskError::TaskNotFound(task_id.to_string())),
            }
        }

        async fn get_active_task(&self) -> String {
            self.0.lock().unwrap().active.clone()
        }

        async fn set_active_task(&self, task_id: &str) -> Result<(), TaskError> {
            if !self.has_task(task_id) {
                return Err(TaskError::TaskNotFound(task_id.to_string()));
            }
            self.0.lock().unwrap().active = task_id.to_string();
            Ok(())
        }

        async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
            let kind = self.load_task(task_id).await?;
            Ok(TaskDetail {
                summary: TaskSummary {
                    id: task_id.to_string(),
                    name: kind.common().name.clone(),
                    description: kind.common().description.clone(),
                    task_type: kind.type_name().to_string(),
                },
                config: kind,
            })
        }

        async fn load_order(&self) -> OrderData {
            self.0.lock().unwrap().order.clone()
        }

        async fn save_order(&self, order: &OrderData) -> Result<(), TaskError> {
            self.0.lock().unwrap().order = order.clone();
            Ok(())
        }

        async fn get_script_path(&self, _task_id: &str) -> Option<std::path::PathBuf> {
            None
        }

        fn has_task(&self, task_id: &str) -> bool {
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .any(|(id, _)| id == task_id)
        }
    }

    /// 内存 TaskRunApi：记录执行的任务并返回成功结果
    struct MockTaskRunApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl TaskRunApi for MockTaskRunApi {
        async fn execute(&self, task: &TaskKind) -> Result<TaskResult, TaskError> {
            self.0
                .lock()
                .unwrap()
                .executed
                .push(task.common().task_id.clone());
            Ok(TaskResult {
                success: true,
                output: "mock".into(),
                exit_code: 0,
                duration_ms: 1,
                error: None,
            })
        }
    }

    fn browser_task(id: &str) -> TaskKind {
        TaskKind::Browser(crate::tasks::TaskConfig {
            common: crate::tasks::CommonFields {
                task_id: id.into(),
                name: format!("任务 {id}"),
                description: String::new(),
            },
            url: "https://example.com".into(),
            ..Default::default()
        })
    }

    /// 多 State 提取的测试 Router：TaskApi 与 TaskRunApi 需组合为单一 state 类型
    #[derive(Clone)]
    struct TestState {
        api: Arc<dyn TaskApi>,
        runner: Arc<dyn TaskRunApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &TestState) -> Self {
            state.api.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskRunApi> {
        fn from_ref(state: &TestState) -> Self {
            state.runner.clone()
        }
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            tasks: vec![("t1".into(), browser_task("t1"))],
            active: "t1".into(),
            order: OrderData::default(),
            executed: Vec::new(),
        }));
        let state = TestState {
            api: Arc::new(MockTaskApi(inner.clone())),
            runner: Arc::new(MockTaskRunApi(inner.clone())),
        };
        let app = axum::Router::new()
            .route("/api/tasks", get(list_tasks).post(create_task))
            .route("/api/tasks/active", get(get_active_task))
            .route("/api/tasks/active/{task_id}", post(set_active_task))
            .route(
                "/api/tasks/{id}",
                get(get_task).put(update_task).delete(delete_task),
            )
            .route("/api/tasks/order", post(order_tasks))
            .route("/api/tasks/import", post(import_tasks))
            .route("/api/tasks/export/{id}", get(export_task))
            .route("/api/tasks/{id}/execute", post(execute_task))
            .with_state(state);
        (app, inner)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 列表返回内存中的任务摘要
    #[tokio::test]
    async fn test_list_tasks_returns_summaries() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let arr = v.get("data").and_then(|d| d.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "t1");
    }

    /// 活跃任务读写
    #[tokio::test]
    async fn test_active_task_roundtrip() {
        let (app, inner) = mock_app();
        // 读取
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/active")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(resp).await;
        assert_eq!(v["data"]["task_id"], "t1");
        // 写入
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/active/t1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().active, "t1");
    }

    /// 创建任务（script 类型）
    #[tokio::test]
    async fn test_create_task_script_kind() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "s1", "name": "脚本", "kind": "script", "script": "print(1)"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        assert!(matches!(
            inner
                .tasks
                .iter()
                .find(|(id, _)| id == "s1")
                .map(|(_, k)| k),
            Some(TaskKind::Script(_))
        ));
    }

    /// 查询不存在任务返回 404
    #[tokio::test]
    async fn test_get_task_not_found() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 更新任务（完整负载）
    #[tokio::test]
    async fn test_update_task_overwrites() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/tasks/t1")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "type": "browser", "id": "t1", "name": "改名", "url": "https://new.example.com"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        let TaskKind::Browser(cfg) = inner
            .tasks
            .iter()
            .find(|(id, _)| id == "t1")
            .unwrap()
            .1
            .clone()
        else {
            panic!("应为 browser 任务");
        };
        assert_eq!(cfg.common.name, "改名");
    }

    /// 删除任务
    #[tokio::test]
    async fn test_delete_task_removes() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/tasks/t1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(inner.lock().unwrap().tasks.is_empty());
    }

    /// 排序合并去重（all 与 scripts 交叉）
    #[tokio::test]
    async fn test_order_tasks_dedupes() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/order")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"all": ["t1", "t2"], "scripts": ["t2", "s1"]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let order = inner.lock().unwrap().order.clone();
        assert_eq!(order.order, vec!["t1", "t2", "s1"]);
    }

    /// 导入：合法条目计数、非法条目收集失败原因
    #[tokio::test]
    async fn test_import_tasks_partial_success() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/import")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!([
                            { "type": "browser", "id": "i1", "name": "导入1", "url": "https://a.example.com" },
                            // content 类型错误（数字而非字符串）→ 反序列化失败
                            { "type": "script", "id": "i2", "name": "坏负载", "content": 123 }
                        ])
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["imported"], 1);
        assert_eq!(v["data"]["failed"].as_array().unwrap().len(), 1);
        assert_eq!(inner.lock().unwrap().tasks.len(), 2);
    }

    /// 导出返回任务详情
    #[tokio::test]
    async fn test_export_task_returns_detail() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/export/t1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["summary"]["id"], "t1");
    }

    /// 手动执行：加载任务并交给执行器
    #[tokio::test]
    async fn test_execute_task_runs() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/t1/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["success"], true);
        assert_eq!(inner.lock().unwrap().executed, vec!["t1"]);
    }
}
