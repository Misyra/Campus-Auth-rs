//! 脚本路由：脚本管理、可执行文件列表、Shell 列表
//!
//! M1 细粒度 state：脚本 CRUD handler 声明 `State<Arc<dyn TaskApi>>` /
//! `State<Arc<dyn TaskRunApi>>` 依赖，可执行文件列表经
//! `State<Arc<dyn EnvironmentApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;

use crate::environment::EnvironmentApi;
use crate::tasks::{TaskApi, TaskRunApi};
use crate::web::error::{ApiError, data};

/// 校验脚本执行程序：仅允许 shell / bat / python / exe 四类，拒绝 PowerShell 等。
fn check_supported_binary(
    binary_path: Option<&str>,
    script_path: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(bp) = binary_path {
        let lower = bp.to_lowercase();
        if lower.contains("powershell") || lower.contains("pwsh") || lower.ends_with(".ps1") {
            return Err(ApiError::BadRequest(
                "不支持 PowerShell，仅支持 shell / bat / python / exe 四类脚本".into(),
            ));
        }
    }
    if let Some(sp) = script_path {
        let ext = std::path::Path::new(sp)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "ps1" {
            return Err(ApiError::BadRequest(
                "不支持 .ps1 脚本，仅支持 shell / bat / python / exe 四类".into(),
            ));
        }
    }
    Ok(())
}

/// GET /api/scripts — 列出全部脚本（复用任务列表）
pub async fn list_scripts(State(tasks): State<Arc<dyn TaskApi>>) -> Result<Json<Value>, ApiError> {
    let tasks = tasks.list_all_tasks().await;
    Ok(data(tasks))
}

#[derive(Deserialize)]
pub struct RunScriptBody {
    /// 脚本任务 ID
    pub task_id: Option<String>,
    /// 直接传入脚本内容（与 task_id 二选一）
    pub script: Option<String>,
}

/// POST /api/scripts/run — 运行脚本
///
/// 支持两种调用方式：
/// - `{"task_id": "<task_id>"}`：运行已保存的脚本任务
/// - `{"script": "<content>"}`：直接运行临时脚本内容
pub async fn run_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    State(runner): State<Arc<dyn TaskRunApi>>,
    Json(body): Json<RunScriptBody>,
) -> Result<Json<Value>, ApiError> {
    let task = if let Some(id) = body.task_id.as_deref() {
        tasks.load_task(id).await?
    } else if let Some(content) = body.script {
        crate::tasks::TaskKind::Script(crate::tasks::ScriptTaskConfig {
            common: crate::tasks::CommonFields {
                task_id: "adhoc_script".into(),
                name: "临时脚本".into(),
                description: String::new(),
            },
            content: Some(content),
            ..Default::default()
        })
    } else {
        return Err(ApiError::BadRequest("缺少 task_id 或 script 字段".into()));
    };
    let result = runner.execute(&task).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/scripts/binaries — 可用可执行文件列表
///
/// 扫描系统 PATH 中的常用解释器/可执行文件。
pub async fn list_binaries(
    State(environment): State<Arc<dyn EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    let python_path = environment.python_path().to_string_lossy().to_string();
    let mut binaries = vec![serde_json::json!({ "name": "python", "path": python_path })];

    // 扫描常见系统可执行文件（Windows + Unix 兼容）。
    // 仅列出受支持的脚本解释器：shell / bat / python；powershell 不在支持范围。
    #[cfg(target_os = "windows")]
    for (name, exe) in [("cmd", "cmd.exe")] {
        if let Some(path) = find_in_path(exe) {
            binaries.push(serde_json::json!({
                "name": name,
                "path": path,
            }));
        }
    }
    #[cfg(not(target_os = "windows"))]
    for (name, exe) in [("bash", "bash"), ("sh", "sh")] {
        if let Some(path) = find_in_path(exe) {
            binaries.push(serde_json::json!({
                "name": name,
                "path": path,
            }));
        }
    }
    Ok(data(binaries))
}

/// 在系统 PATH 中查找可执行文件
fn find_in_path(exe_name: &str) -> Option<String> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(exe_name);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// GET /api/scripts/{task_id} — 获取脚本完整内容
///
/// 返回编辑器所需的全部字段（name/description/content/binary_path 等）。
/// 内容来源两种存储模式：内联 `content` 字段或 `script_path` 指向的磁盘文件。
pub async fn get_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = tasks.load_task(&task_id).await?;
    let crate::tasks::TaskKind::Script(cfg) = task else {
        return Err(ApiError::NotFound(format!("脚本 {} 不存在", task_id)));
    };
    // script_path 模式：读取磁盘文件；内联模式：直接取 content 字段
    let content = if let Some(path) = tasks.get_script_path(&task_id).await {
        tokio::fs::read_to_string(&path).await?
    } else {
        cfg.content.clone().unwrap_or_default()
    };
    Ok(data(serde_json::json!({
        "id": task_id,
        "name": cfg.common.name,
        "description": cfg.common.description,
        "content": content,
        "binary_path": cfg.binary_path.clone().unwrap_or_default(),
        "script_path": cfg.script_path,
        "args": cfg.args,
        "timeout": cfg.timeout,
    })))
}

/// PUT /api/scripts/{task_id} — 更新脚本
///
/// 显式要求 `type == "script"`：`TaskKind` 反序列化在 type 缺失时默认归为
/// browser 任务，会把脚本负载静默转存为空的浏览器任务（脚本内容丢失），
/// 故在此前置拦截，防止前端回归。
pub async fn update_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if body.get("type").and_then(Value::as_str) != Some("script") {
        return Err(ApiError::BadRequest(
            "脚本接口仅接受 type=script 的负载".into(),
        ));
    }
    // 校验二进制与脚本路径：仅允许 shell / bat / python / exe，拒绝 PowerShell
    check_supported_binary(
        body.get("binary_path").and_then(Value::as_str),
        body.get("script_path").and_then(Value::as_str),
    )?;
    let task: crate::tasks::TaskKind =
        serde_json::from_value(body).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    tasks.save_task(&task_id, &task).await?;
    Ok(data(task_id))
}

/// DELETE /api/scripts/{task_id} — 删除脚本
pub async fn delete_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    tasks.delete_task(&task_id).await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/shells — Shell 列表（Shell 任务专用，与 Script 任务正交）
///
/// 返回系统可用 Shell（用于 `TaskKind::Shell.shell_path`），支持
/// `powershell/pwsh`；Script 任务（`TaskKind::Script`）的 `binary_path`
/// 禁止 PowerShell（见 `check_supported_binary` 与 `is_supported_ext`），
/// 二者域不同，不视为矛盾。
pub async fn list_shells() -> Result<Json<Value>, ApiError> {
    #[cfg(target_os = "windows")]
    {
        Ok(data(serde_json::json!({
            "shells": [
                { "name": "PowerShell", "path": "powershell.exe" },
                { "name": "CMD", "path": "cmd.exe" }
            ],
            "default": "powershell.exe"
        })))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(data(serde_json::json!({
            "shells": [
                { "name": "bash", "path": "/bin/bash" },
                { "name": "sh", "path": "/bin/sh" }
            ],
            "default": "/bin/bash"
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt; // oneshot

    use crate::environment::{BootstrapStage, EnvironmentApi, EnvironmentError, EnvironmentStatus};
    use crate::tasks::{
        CommonFields, OrderData, ScriptTaskConfig, TaskApi, TaskDetail, TaskError, TaskKind,
        TaskResult, TaskRunApi, TaskSummary,
    };

    use super::super::test_support::body_json;

    struct MockInner {
        tasks: Vec<(String, TaskKind)>,
        ran: Vec<String>,
    }

    fn script_kind(id: &str, content: &str) -> TaskKind {
        TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                task_id: id.to_string(),
                name: format!("{id} 脚本"),
                description: String::new(),
            },
            content: Some(content.to_string()),
            ..Default::default()
        })
    }

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
            String::new()
        }

        async fn set_active_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
            let kind = self.load_task(task_id).await?;
            Ok(TaskDetail {
                summary: TaskSummary {
                    id: task_id.to_string(),
                    name: kind.common().name.clone(),
                    description: String::new(),
                    task_type: kind.type_name().to_string(),
                },
                config: kind,
            })
        }

        async fn load_order(&self) -> OrderData {
            OrderData::default()
        }

        async fn save_order(&self, _order: &OrderData) -> Result<(), TaskError> {
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

    struct MockTaskRunApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl TaskRunApi for MockTaskRunApi {
        async fn execute(&self, task: &TaskKind) -> Result<TaskResult, TaskError> {
            let mut inner = self.0.lock().unwrap();
            inner.ran.push(task.common().task_id.clone());
            Ok(TaskResult {
                success: true,
                output: "mock-ok".to_string(),
                exit_code: 0,
                duration_ms: 1,
                error: None,
            })
        }
    }

    struct MockEnvironmentApi;

    #[async_trait::async_trait]
    impl EnvironmentApi for MockEnvironmentApi {
        fn status(&self) -> EnvironmentStatus {
            EnvironmentStatus {
                uv_ready: true,
                python_ready: true,
                playwright_ready: true,
                git_ready: true,
                capability_ready: true,
                stage: BootstrapStage::Idle,
                progress: None,
                last_error: None,
            }
        }
        fn python_path(&self) -> std::path::PathBuf {
            std::path::PathBuf::from("/mock/python")
        }
        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_playwright_browser(&self, _browser: &str) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        fn ocr_ready(&self) -> bool {
            false
        }
        fn ocr_declared(&self) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct TestState {
        tasks: Arc<dyn TaskApi>,
        runner: Arc<dyn TaskRunApi>,
        env: Arc<dyn EnvironmentApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &TestState) -> Self {
            state.tasks.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskRunApi> {
        fn from_ref(state: &TestState) -> Self {
            state.runner.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn EnvironmentApi> {
        fn from_ref(state: &TestState) -> Self {
            state.env.clone()
        }
    }

    fn mock_app(seed: Vec<(String, TaskKind)>) -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            tasks: seed,
            ran: Vec::new(),
        }));
        let state = TestState {
            tasks: Arc::new(MockTaskApi(inner.clone())),
            runner: Arc::new(MockTaskRunApi(inner.clone())),
            env: Arc::new(MockEnvironmentApi),
        };
        let app = axum::Router::new()
            .route("/api/scripts", get(list_scripts))
            .route("/api/scripts/run", post(run_script))
            .route("/api/scripts/binaries", get(list_binaries))
            .route(
                "/api/scripts/{task_id}",
                get(get_script).put(update_script).delete(delete_script),
            )
            .route("/api/shells", get(list_shells))
            .with_state(state);
        (app, inner)
    }

    /// 空列表形状：data 为 []
    #[tokio::test]
    async fn list_empty_returns_array() {
        let (app, _) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scripts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["data"], serde_json::json!([]));
    }

    /// task_id 与 script 双缺 → 400（不触达执行器）
    #[tokio::test]
    async fn run_without_id_or_script_is_bad_request() {
        let (app, inner) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scripts/run")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(inner.lock().unwrap().ran.is_empty());
    }

    /// 临时脚本直跑：构造 adhoc Script 任务并执行，结果透出 success
    #[tokio::test]
    async fn run_adhoc_script_executes() {
        let (app, inner) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scripts/run")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"script":"print(1)"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["success"], true);
        assert_eq!(inner.lock().unwrap().ran, vec!["adhoc_script"]);
    }

    /// 未知 task_id → 404
    #[tokio::test]
    async fn run_unknown_id_is_not_found() {
        let (app, _) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scripts/run")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"task_id":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 内联 content 直取：不读盘
    #[tokio::test]
    async fn get_script_returns_inline_content() {
        let (app, _) = mock_app(vec![("s1".into(), script_kind("s1", "print(1)"))]);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scripts/s1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["content"], "print(1)");
        assert_eq!(v["data"]["id"], "s1");
    }

    /// 非脚本类型（browser）→ 404（防编辑器误读）
    #[tokio::test]
    async fn get_script_rejects_non_script_type() {
        let browser: TaskKind =
            serde_json::from_value(serde_json::json!({"type":"browser","task_id":"b","name":"b"}))
                .unwrap();
        let (app, _) = mock_app(vec![("b".into(), browser)]);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scripts/b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 更新缺 type → 400（防静默转存为空浏览器任务）；ps1 → 400
    #[tokio::test]
    async fn update_validates_type_and_binary() {
        let (app, _) = mock_app(vec![]);
        for body in [
            r#"{"name":"x"}"#,
            r#"{"type":"script","task_id":"s","name":"x","binary_path":"C:\\w\\p.ps1"}"#,
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/api/scripts/s")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body={body}");
        }
    }

    /// 合法脚本更新落盘到内存存储
    #[tokio::test]
    async fn update_persists_valid_script() {
        let (app, inner) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/scripts/s2")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"type":"script","task_id":"s2","name":"n","content":"print(2)"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(inner.lock().unwrap().tasks.iter().any(|(id, _)| id == "s2"));
    }

    /// 删除成功与删除不存在
    #[tokio::test]
    async fn delete_ok_and_not_found() {
        let (app, _) = mock_app(vec![("s1".into(), script_kind("s1", "x"))]);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/scripts/s1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/scripts/s1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// binaries 首项恒为 python（路径取自环境）
    #[tokio::test]
    async fn binaries_lists_python_first() {
        let (app, _) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scripts/binaries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"][0]["name"], "python");
        assert_eq!(v["data"][0]["path"], "/mock/python");
    }

    /// shells 按编译目标返回（无状态依赖）
    #[tokio::test]
    async fn shells_match_platform() {
        let (app, _) = mock_app(vec![]);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/shells")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        #[cfg(target_os = "windows")]
        assert_eq!(v["data"]["default"], "powershell.exe");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(v["data"]["default"], "/bin/bash");
    }
}
