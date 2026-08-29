//! OCR 路由：OCR 识别（通过 Bridge 执行）
//!
//! M1 细粒度 state：Bridge 经 `State<Arc<dyn BridgeApi>>`、环境能力经
//! `State<Arc<dyn EnvironmentApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::environment::EnvironmentApi;
use crate::web::error::{ApiError, data};

/// recognize 请求体上限：Worker 的 NDJSON stdin 单行上限为 16 MiB，这里预留 1 MiB
/// 给 IPC 外壳与 JSON 字段，保证 Web 已接受的请求一定能完整送达 Worker。
pub(crate) const RECOGNIZE_BODY_LIMIT: usize = 15 * 1024 * 1024;

/// POST /api/ocr/recognize — 执行 OCR 识别
///
/// Worker 返回的是 IpcResponse `{ id, result: { success, data, error } }`，
/// 而前端契约只认 `{ data: <负载> }` / `{ error: {...} }` 两种信封：
/// - success=true：提取 `result.data`（形如 `{"text": "..."}`）作为业务负载返回；
/// - success=false：把 worker 的 error 消息转为 HTTP 错误（否则错误被埋在 200
///   响应体里，前端既不显示识别结果也不提示失败）。
pub async fn ocr_recognize(
    State(bridge): State<Arc<dyn BridgeApi>>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 注入 cancel_id="ocr"，使 /api/ocr/uninstall 的 bridge.cancel("ocr") 能命中
    // execute_inner 注册的取消令牌（execute_inner 从 params["cancel_id"] 读取并注册）
    if let Some(obj) = body.as_object_mut() {
        obj.insert("cancel_id".into(), Value::String("ocr".into()));
    }
    let response = bridge.execute("ocr_recognize", body).await?;
    if response.result.success {
        Ok(data(response.result.data))
    } else {
        Err(ApiError::Internal(
            response
                .result
                .error
                .unwrap_or_else(|| "OCR 识别失败".into()),
        ))
    }
}

/// GET /api/ocr/status — 获取 OCR 状态
///
/// 返回：
/// - `installed`：OCR **依赖（ddddocr）** 是否已实际安装（仅需 Python venv 就绪 +
///   dddddcr 已装入 venv，不依赖 Playwright 浏览器是否就绪——OCR 识别只用 CPU 推理）。
/// - `declared`：项目是否在 `python_worker/pyproject.toml` 的 `ocr` optional extra
///   中声明 ddddocr，表示当前构建支持 OCR 可选能力。
/// - `size_mb`：environment 目录估算体积。
/// - `runtime_ocr`（任务 10，向后兼容新增）：Worker 存活时最近一次健康检查
///   上报的运行时 OCR 能力（`capabilities.ocr`）；Worker 未存活/未上报时为
///   `null`，前端未消费该字段前不破坏既有契约。
pub async fn ocr_status(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // OCR 依赖是否安装：直接以「ddddocr 是否已装入 venv 的 site-packages」为准。
    // 只要 ddddocr 包目录/ dist-info 存在，就代表 OCR 运行环境已就绪——
    // 它天然隐含 python venv 可用，因此不再叠加 python_ready / playwright_ready 判定，
    // 避免这些偶发/滞后状态把已安装的 OCR 误报成「未安装」。
    let installed = environment.ocr_ready();
    let declared = environment.ocr_declared();
    // 运行时能力（任务 10）：Worker 存活时优先用最近健康检查缓存的 capabilities
    // （BridgeInner 内缓存），未存活回退文件探测（installed）
    let runtime_ocr = bridge.runtime_ocr_capability();
    // 统计 environment 目录大小（递归）
    let env_dir = config.base_path().join("environment");
    let size_bytes = if env_dir.exists() {
        // dir_size 递归遍历文件系统（同步阻塞），用 spawn_blocking 避免阻塞 async 运行时
        tokio::task::spawn_blocking(move || dir_size(&env_dir))
            .await
            .unwrap_or(0)
    } else {
        0
    };
    let status = serde_json::json!({
        "installed": installed,
        "declared": declared,
        "runtime_ocr": runtime_ocr,
        "size_mb": (size_bytes as f64 / (1024.0 * 1024.0)).round(),
    });
    Ok(data(status))
}

/// 递归统计目录大小
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&p);
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// POST /api/ocr/uninstall — 卸载 OCR（取消在途任务并移除依赖）
///
/// 取消在途 OCR 识别任务（bridge.cancel），并通过基础 `uv sync`
/// 移除 OCR extra（environment.remove_ocr_dep）。
pub async fn ocr_uninstall(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    bridge.cancel("ocr");
    // Windows 不允许删除已加载的 onnxruntime DLL，因此卸载前先回收持有模型的 Worker。
    bridge.recycle_if_running().await;
    environment.remove_ocr_dep().await?;
    Ok(data(Value::String("OCR 依赖已卸载".into())))
}

/// POST /api/ocr/install — 安装 OCR 环境并增量补装 OCR 依赖
///
/// 后台执行环境能力安装（uv/Python/Playwright）并显式同步 `ocr` extra，
/// 补齐 OCR 依赖，进度通过 StatusManager 推送。
pub async fn ocr_install(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    let env = environment.clone();
    tokio::spawn(async move {
        // 先确保核心能力就绪，再同步 OCR optional extra
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("OCR 环境引导失败: {e}");
            return;
        }
        if let Err(e) = env.install_ocr_dep().await {
            tracing::error!("OCR 依赖安装失败: {e}");
            return;
        }
        // Worker 可能在安装 OCR 前已经启动；若不回收，首次识别仍会在线程池中
        // 首次导入 numpy/onnxruntime，Windows 下可能卡在 DLL loader lock。
        bridge.recycle_if_running().await;
    });
    Ok(data(serde_json::json!({
        "message": "OCR 环境安装已启动",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use tower::ServiceExt; // oneshot

    use crate::bridge::{BridgeError, IpcResponse, IpcResult};

    struct MockInner {
        executed: Vec<(String, Value)>,
        cancelled: Vec<String>,
        removed: bool,
        recycled: usize,
        installed: bool,
        /// execute 的预置响应：(success, data, error)，默认成功并返回识别文本
        respond: (bool, Value, Option<String>),
        /// runtime_ocr_capability 的预置返回值（任务 10）
        ocr_capability: Option<bool>,
    }

    impl Default for MockInner {
        fn default() -> Self {
            Self {
                executed: Vec::new(),
                cancelled: Vec::new(),
                removed: false,
                recycled: 0,
                installed: false,
                respond: (true, serde_json::json!({ "text": "abcd" }), None),
                ocr_capability: None,
            }
        }
    }

    /// 内存 BridgeApi：记录 execute 的 method/params 与 cancel 的 cancel_id
    struct MockBridgeApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl BridgeApi for MockBridgeApi {
        async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
            let mut inner = self.0.lock().unwrap();
            inner.executed.push((method.to_string(), params));
            let (success, data, error) = inner.respond.clone();
            Ok(IpcResponse {
                id: 1,
                result: IpcResult {
                    success,
                    data,
                    error,
                },
            })
        }

        fn cancel(&self, cancel_id: &str) {
            self.0.lock().unwrap().cancelled.push(cancel_id.to_string());
        }

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: std::time::Duration,
        ) -> Result<IpcResponse, BridgeError> {
            self.execute(method, params).await
        }

        async fn force_recycle(&self) {}

        fn has_live_worker(&self) -> bool {
            false
        }

        async fn recycle_if_running(&self) {
            self.0.lock().unwrap().recycled += 1;
        }

        async fn shutdown(&self) {}

        fn runtime_ocr_capability(&self) -> Option<bool> {
            self.0.lock().unwrap().ocr_capability
        }
    }

    use crate::environment::{BootstrapStage, EnvironmentApi, EnvironmentError, EnvironmentStatus};

    /// 内存 EnvironmentApi：remove_ocr_dep 记录到 inner.removed
    struct MockEnvironmentApi {
        removed: Arc<std::sync::Mutex<MockInner>>,
    }

    #[async_trait::async_trait]
    impl EnvironmentApi for MockEnvironmentApi {
        fn status(&self) -> EnvironmentStatus {
            EnvironmentStatus {
                uv_ready: false,
                python_ready: false,
                playwright_ready: false,
                git_ready: false,
                capability_ready: false,
                stage: BootstrapStage::Idle,
                progress: None,
                last_error: None,
            }
        }
        fn python_path(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_playwright_browser(&self, _browser: &str) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
            self.removed.lock().unwrap().installed = true;
            Ok(())
        }
        async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
            self.removed.lock().unwrap().removed = true;
            Ok(())
        }
        fn ocr_ready(&self) -> bool {
            false
        }
        fn ocr_declared(&self) -> bool {
            true
        }
    }

    /// 双域 state：BridgeApi + EnvironmentApi 各自经 FromRef 委派提取
    #[derive(Clone)]
    struct TestState {
        bridge: Arc<dyn BridgeApi>,
        env: Arc<dyn EnvironmentApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn BridgeApi> {
        fn from_ref(state: &TestState) -> Self {
            state.bridge.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn EnvironmentApi> {
        fn from_ref(state: &TestState) -> Self {
            state.env.clone()
        }
    }

    /// status 路由专用 state：额外携带真实 ConfigService（base_path 指向临时目录，
    /// environment 子目录不存在 → size_mb=0，避免真实磁盘遍历）
    #[derive(Clone)]
    struct StatusState {
        base: TestState,
        config: Arc<crate::config::ConfigService>,
    }

    impl axum::extract::FromRef<StatusState> for Arc<dyn BridgeApi> {
        fn from_ref(state: &StatusState) -> Self {
            state.base.bridge.clone()
        }
    }

    impl axum::extract::FromRef<StatusState> for Arc<dyn EnvironmentApi> {
        fn from_ref(state: &StatusState) -> Self {
            state.base.env.clone()
        }
    }

    impl axum::extract::FromRef<StatusState> for Arc<dyn crate::config::ConfigApi> {
        fn from_ref(state: &StatusState) -> Self {
            state.config.clone()
        }
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner::default()));
        let bridge: Arc<dyn BridgeApi> = Arc::new(MockBridgeApi(inner.clone()));
        let env: Arc<dyn EnvironmentApi> = Arc::new(MockEnvironmentApi {
            removed: inner.clone(),
        });
        let state = TestState { bridge, env };
        let app = axum::Router::new()
            // 与 route_table 中真实注册形态一致：携带放宽的请求体限制
            .route(
                "/api/ocr/recognize",
                post(ocr_recognize).layer(DefaultBodyLimit::max(RECOGNIZE_BODY_LIMIT)),
            )
            .route("/api/ocr/uninstall", post(ocr_uninstall))
            .route("/api/ocr/install", post(ocr_install))
            .with_state(state);
        (app, inner)
    }

    /// status 路由测试 app（Bridge 能力 mock + 真实 ConfigService 锚定临时目录）
    async fn status_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner::default()));
        let bridge: Arc<dyn BridgeApi> = Arc::new(MockBridgeApi(inner.clone()));
        let env: Arc<dyn EnvironmentApi> = Arc::new(MockEnvironmentApi {
            removed: inner.clone(),
        });
        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = crate::config::ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        let state = StatusState {
            base: TestState { bridge, env },
            config,
        };
        let app = axum::Router::new()
            .route("/api/ocr/status", axum::routing::get(ocr_status))
            .with_state(state);
        (app, inner)
    }

    /// 任务 10：status 响应包含 runtime_ocr 字段，取自 Bridge 缓存的运行时能力；
    /// 未上报时为 null（前端未消费前不破坏既有字段）
    #[tokio::test]
    async fn test_ocr_status_reports_runtime_capability() {
        let (app, inner) = status_app().await;

        // 未上报（Worker 未存活/无缓存）→ null
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ocr/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["data"]["runtime_ocr"], Value::Null);
        assert_eq!(body["data"]["declared"], true);

        // Worker 上报 capabilities.ocr=true → runtime_ocr=true
        inner.lock().unwrap().ocr_capability = Some(true);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ocr/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["data"]["runtime_ocr"], true);
    }

    /// recognize 注入 cancel_id="ocr" 后派发命令，成功时仅提取 result.data 入信封
    #[tokio::test]
    async fn test_ocr_recognize_injects_cancel_id() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/recognize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"image_base64": "aW1n"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let calls = inner.lock().unwrap().executed.clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "ocr_recognize");
        assert_eq!(calls[0].1["cancel_id"], "ocr");
        assert_eq!(calls[0].1["image_base64"], "aW1n");
        // 响应信封：仅含 worker 的 result.data，不透传 IpcResponse 的 id/result 外壳
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body, serde_json::json!({ "data": { "text": "abcd" } }));
    }

    /// worker 返回 success=false 时映射为 500，错误消息透传到 error.message
    #[tokio::test]
    async fn test_ocr_recognize_worker_failure_maps_to_error() {
        let (app, inner) = mock_app();
        inner.lock().unwrap().respond = (false, Value::Null, Some("ddddocr 未安装".to_string()));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/recognize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"image_base64": "aW1n"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["error"]["message"], "ddddocr 未安装");
    }

    /// 大图回归：超过 axum 默认 2MB 的 base64 请求体（>1.5MB 原图）不再报 413
    #[tokio::test]
    async fn test_ocr_recognize_accepts_large_payload() {
        let (app, _inner) = mock_app();
        let big = "a".repeat(3 * 1024 * 1024);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/recognize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "image_base64": big }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// 超过 IPC 安全上限的请求由 Web 层直接拒绝，不能送进 Worker 后静默丢失。
    #[tokio::test]
    async fn test_ocr_recognize_rejects_payload_above_ipc_safe_limit() {
        let (app, _inner) = mock_app();
        let too_big = "a".repeat(RECOGNIZE_BODY_LIMIT);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/recognize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "image_base64": too_big }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// uninstall 派发 cancel("ocr")、移除依赖并回收已运行 Worker
    #[tokio::test]
    async fn test_ocr_uninstall_cancels_and_removes_dep() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/uninstall")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let inner = inner.lock().unwrap();
        assert_eq!(inner.cancelled, vec!["ocr"]);
        assert!(inner.removed);
        assert_eq!(inner.recycled, 1);
    }

    /// install 后台任务成功补装依赖后回收已运行 Worker，确保下次启动主线程预加载。
    #[tokio::test]
    async fn test_ocr_install_recycles_worker_after_install() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/install")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        for _ in 0..100 {
            {
                let state = inner.lock().unwrap();
                if state.installed && state.recycled == 1 {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
        panic!("OCR 安装完成后未回收 Worker");
    }
}
