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

use crate::tasks::{OrderData, TaskApi, TaskRunApi};
use crate::web::error::{ApiError, data};

/// GET /api/tasks — 列出全部自定义任务
pub async fn list_tasks(State(tasks): State<Arc<dyn TaskApi>>) -> Result<Json<Value>, ApiError> {
    let tasks = tasks.list_all_tasks().await;
    Ok(data(tasks))
}

#[derive(Deserialize)]
pub struct TaskCreateBody {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub url: Option<String>,
    pub script: Option<String>,
    /// browser 任务的步骤列表：缺省为空（由 save_task 的校验显式报错，
    /// 与 PUT /api/tasks 的口径一致），传入时反序列化为 StepConfig
    #[serde(default)]
    pub steps: Option<serde_json::Value>,
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
            // steps 缺省为空 → save_task 校验显式报「steps 不能为空」；
            // 传入时反序列化失败返回 400 而非 500（修复创建 browser 任务必然失败）
            let steps = match body.steps {
                Some(v) => serde_json::from_value(v)
                    .map_err(|e| ApiError::BadRequest(format!("steps 格式错误: {e}")))?,
                None => Vec::new(),
            };
            crate::tasks::TaskKind::Browser(crate::tasks::TaskConfig {
                common,
                url: body.url.unwrap_or_default(),
                steps,
                ..Default::default()
            })
        }
        // Shell 任务已移除：历史 kind=shell 明确拒绝，提示改用 script
        Some("shell") => {
            return Err(ApiError::BadRequest(
                "任务类型 shell 已移除，请改用 script 类型".into(),
            ));
        }
        Some("script") => crate::tasks::TaskKind::Script(crate::tasks::ScriptTaskConfig {
            common: common.clone(),
            content: body.script,
            ..Default::default()
        }),
        // 直连任务：这里只建最小骨架（地址 + 名称），请求头/体/判定关键字/脚本等
        // 由任务编辑器随后补齐。地址允许先留空——save_task 的校验只要求 http 任务
        // 的地址在**非空时**必须合法，占位保存后再填也不报错。
        Some("http") => crate::tasks::TaskKind::Http(crate::tasks::HttpTaskConfig {
            common,
            url: body.url.unwrap_or_default(),
            ..Default::default()
        }),
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "未知任务类型 kind: {other}（支持 browser / script / http）"
            )));
        }
    };
    tasks.save_task(&body.id, &kind).await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/tasks/{id} — 获取单个任务
///
/// **不先用 `has_task` 短路**：那样畸形 id（如 `../config/settings`）会被报成"任务不存在"
/// 的 404，而同一个 id 在 PUT / DELETE 上是 400 —— 同一形态在三个方法上给出两种结论，
/// 排查方向被带偏（自动保存打来一个畸形 id 时尤其明显）。交给加载路径判：
/// `InvalidTaskId → 400`、`TaskNotFound → 404`，与写路径同一套口径。
pub async fn get_task(
    State(task_api): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
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

/// 任务排序请求体（对齐前端 `{ all, scripts, http }` 契约）
///
/// `all` 是**浏览器任务**的历史字段名（三类任务里它曾是唯一一类），刻意不改名：
/// 改名会让「旧后端 + 新前端」这种组合（调试构建运行时读盘、前端先于后端更新时
/// 真实存在）在拖拽排序时因缺字段而 400。
#[derive(Deserialize)]
pub struct OrderBody {
    /// 浏览器任务 ID 顺序
    pub all: Vec<String>,
    /// 脚本任务 ID 顺序
    pub scripts: Vec<String>,
    /// 直连任务 ID 顺序
    ///
    /// 允许缺省：旧前端（尚无此分组）不发该字段，此时直连任务顺序回落为目录扫描
    /// 顺序——与"加入排序前"的行为一致，不报错、也不丢任务文件。
    #[serde(default)]
    pub http: Vec<String>,
}

/// POST /api/tasks/order — 保存任务排序
///
/// 接受前端 `{ all, scripts, http }` 结构，合并写入内部 `OrderData.order`
/// （`.order.json` 是三类任务**共用**的一份扁平 id 列表，见 `tasks/loader.rs`）。
/// **三组必须全量互传**：本函数整体替换排序表，漏传一组等于把那一组的顺序清空。
///
/// 不再"先 load_order 再改"：那是跨两次独立加锁的读改写，与自动保存的
/// `PUT /api/tasks/{id}`（`save_task` 会把新 id 追进排序表）并发时会丢更新——
/// 用户看到的是"拖完排序，另一类任务顺序莫名回退"。载荷本来就是全量的，
/// 直接用请求体整体替换即可。
pub async fn order_tasks(
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<OrderBody>,
) -> Result<Json<Value>, ApiError> {
    // 去除可能的重复 ID（三组之间可能有交叉），保留前端传入顺序
    let mut seen = std::collections::HashSet::new();
    let order: Vec<String> = body
        .all
        .into_iter()
        .chain(body.scripts)
        .chain(body.http)
        .filter(|id| seen.insert(id.clone()))
        .collect();
    tasks.save_order(&OrderData { order }).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/tasks/import — 导入任务（支持单个对象或数组）
///
/// 标准格式即导出结果：`GET /api/tasks/export/{id}` 原样的
/// `{ "summary": { "id": ... }, "config": { ... } }`（批量为其数组），导出文件可直接回导。
/// 另向后兼容两种写法：顶层 `id` 的扁平任务对象、`tasks/` 磁盘文件的 `task_id` 形态；
/// 外层另兼容 `{ "tasks"|"data"|"items": [...] }` 包裹与单个对象。
fn unwrap_import_items(body: Value) -> Vec<Value> {
    if let Some(arr) = body.as_array() {
        return arr.clone();
    }
    if let Some(obj) = body.as_object() {
        for key in ["tasks", "items", "data"] {
            if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                return arr.clone();
            }
        }
        // API 信封原样 `{ "code":..,"data":{...} }`：data 为单个任务对象
        if let Some(inner) = obj.get("data").filter(|v| v.is_object()) {
            return vec![inner.clone()];
        }
    }
    vec![body]
}

/// 从单条导入记录提取 `(任务 ID, 任务配置 JSON)`，空 ID 表示无法识别
fn normalize_import_item(item: &Value) -> (String, Value) {
    if let Some(id) = item
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return (id.to_string(), item.clone());
    }
    // 导出端点形态：{ summary: { id }, config: {...} }
    if let Some(cfg) = item.get("config").filter(|v| v.is_object()) {
        let id = item
            .pointer("/summary/id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                cfg.get("task_id")
                    .or_else(|| cfg.get("id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        return (id, cfg.clone());
    }
    // 磁盘文件形态：task_id 即 ID
    if let Some(task_id) = item
        .get("task_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return (task_id.to_string(), item.clone());
    }
    (String::new(), item.clone())
}

/// POST /api/tasks/import — 批量导入任务
///
/// 载荷形状契约：任务对象数组、单个任务对象，或 `{"tasks": [...]}` 包裹均可；条目
/// ID 依次取顶层 `id`、导出详情的 `summary.id`、磁盘文件形态的 `task_id`；任务体
/// 可为完整配置（含 `steps` 等 step 字段）或 `{"config": {...}}` 包裹的导出结果。
/// 逐条导入互不中止，响应回传 `imported` 计数与 `failed` 失败明细。
pub async fn import_tasks(
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let items = unwrap_import_items(body);
    if items.is_empty() {
        return Err(ApiError::BadRequest("导入列表为空".into()));
    }
    let mut imported = 0u32;
    let mut skipped_no_id = 0u32;
    let mut failed: Vec<Value> = Vec::new();
    for item in items {
        let (id, task_value) = normalize_import_item(&item);
        if id.is_empty() {
            skipped_no_id += 1;
            continue;
        }
        // 逐条导入：任一条失败不中止整体，收集失败项供前端提示
        let task: crate::tasks::TaskKind = match serde_json::from_value(task_value) {
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
    // 一条都没导入且无具体失败项：载荷中没有任何可识别 ID 的任务记录，明确报错
    if imported == 0 && failed.is_empty() {
        let hint = if skipped_no_id > 0 {
            format!("{skipped_no_id} 条记录缺少任务 ID（顶层 id / summary.id / task_id 均未找到）")
        } else {
            "载荷无法识别为任务".to_string()
        };
        return Err(ApiError::BadRequest(format!(
            "没有可导入的任务：{hint}（应为任务对象/数组，ID 可为顶层 id、导出结果 summary.id 或磁盘文件的 task_id；外层可用 {{\"tasks\": [...]}} 包裹）"
        )));
    }
    if !failed.is_empty() {
        // 部分失败仅进响应体易被忽略，warn 留痕失败条目数与名称列表
        let names: Vec<String> = failed
            .iter()
            .filter_map(|f| f.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        tracing::warn!(
            failed = failed.len() as u64,
            tasks = %names.join(","),
            "任务导入部分失败"
        );
    }
    Ok(data(
        serde_json::json!({ "imported": imported, "failed": failed }),
    ))
}

/// GET /api/tasks/export/{id} — 导出指定任务的完整配置
///
/// 与 [`get_task`] 同口径：畸形 id 是 400（用户可改），不存在才是 404。
pub async fn export_task(
    State(task_api): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let detail = task_api.get_task_detail(&id).await?;
    Ok(data(serde_json::to_value(detail)?))
}

/// POST /api/tasks/{id}/execute — 手动执行任务（通用语义：浏览器/脚本）
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
        // TaskError 按变体映射（NotFound/400/409/500），不再统一 404 丢失排查信息
        .map_err(ApiError::from)?;
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
                    ..TaskSummary::default()
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

        async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
            let kind = self.load_task(task_id).await?;
            Ok(TaskDetail {
                summary: TaskSummary {
                    id: task_id.to_string(),
                    name: kind.common().name.clone(),
                    description: kind.common().description.clone(),
                    task_type: kind.type_name().to_string(),
                    ..TaskSummary::default()
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
            order: OrderData::default(),
            executed: Vec::new(),
        }));
        let state = TestState {
            api: Arc::new(MockTaskApi(inner.clone())),
            runner: Arc::new(MockTaskRunApi(inner.clone())),
        };
        let app = axum::Router::new()
            .route("/api/tasks", get(list_tasks).post(create_task))
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

    // 测试脚手架统一走共享 test_support（WE2-5：原逐文件复制的 body_json 已收敛）
    use crate::web::routes::test_support::body_json;

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

    /// 创建任务（http 直连类型）：最小骨架只需地址与名称，其余字段走默认值
    #[tokio::test]
    async fn test_create_task_http_kind() {
        let (app, inner) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "h1",
                            "name": "直连",
                            "kind": "http",
                            "url": "http://10.1.1.55/login"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 独立作用域持有 guard：断言完即释放，避免跨后续 await 持锁
        {
            let inner = inner.lock().unwrap();
            let Some(TaskKind::Http(cfg)) = inner
                .tasks
                .iter()
                .find(|(id, _)| id == "h1")
                .map(|(_, k)| k)
            else {
                panic!("应为 http 任务");
            };
            assert_eq!(cfg.common.task_id, "h1");
            assert_eq!(cfg.common.name, "直连");
            assert_eq!(cfg.url, "http://10.1.1.55/login");
            // 未提供的字段一律为默认（不是从别的类型残留过来的值）
            assert_eq!(cfg.method, crate::tasks::HttpRequestMethod::Get);
            assert!(cfg.headers.is_empty() && cfg.body.is_empty());
            assert_eq!(cfg.ignore_https_errors, None);
        }

        // 未知类型错误文案必须列出 http，否则用户按报错改仍会被拒
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"id": "x1", "name": "拼错", "kind": "httpp"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        let msg = json["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("browser / script / http"),
            "有效类型列表应含 http: {msg}"
        );
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

    /// 排序合并去重（all / scripts / http 三组之间交叉）
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
                        serde_json::json!({
                            "all": ["t1", "t2"],
                            "scripts": ["t2", "s1"],
                            "http": ["h1", "t1"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let order = inner.lock().unwrap().order.clone();
        // 顺序 = 浏览器 → 脚本 → 直连，重复 id 只保留首次出现（t1 在 all 里已在位）
        assert_eq!(order.order, vec!["t1", "t2", "s1", "h1"]);
    }

    /// 旧前端（不带 `http` 分组）仍被接受：直连任务顺序回落，不报错、不丢其他两组
    #[tokio::test]
    async fn test_order_tasks_without_http_group_is_accepted() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/order")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"all": ["t1"], "scripts": ["s1"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().order.order, vec!["t1", "s1"]);
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
    /// 导入：导出结果原样（{ summary, config }）可直接回导
    #[tokio::test]
    async fn test_import_tasks_accepts_export_envelope() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/import")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!([{
                            "summary": { "id": "e1", "name": "导出", "description": "", "task_type": "browser" },
                            "config": { "type": "browser", "task_id": "e1", "name": "导出", "url": "https://a.example.com", "steps": [] }
                        }])
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["imported"], 1);
        assert!(inner.lock().unwrap().tasks.iter().any(|(id, _)| id == "e1"));
    }

    /// 导入：磁盘文件原样（task_id）与 {"tasks": [...]} 包裹可直接导入
    #[tokio::test]
    async fn test_import_tasks_accepts_task_id_and_wrapper() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/import")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"tasks": [
                            { "type": "browser", "task_id": "d1", "name": "磁盘", "url": "https://a.example.com", "steps": [] }
                        ]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["imported"], 1);
        assert!(inner.lock().unwrap().tasks.iter().any(|(id, _)| id == "d1"));
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
    /// 导出 → 导入往返：导出结果原样即标准导入格式
    #[tokio::test]
    async fn test_export_import_roundtrip() {
        let (app, inner) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tasks/export/t1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let exported = body_json(resp).await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tasks/import")
                    .header("content-type", "application/json")
                    .body(Body::from(exported["data"].to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["imported"], 1);
        assert!(inner.lock().unwrap().tasks.iter().any(|(id, _)| id == "t1"));
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
