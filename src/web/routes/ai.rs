//! AI 任务生成路由：LLM 配置、登录页捕获与任务 JSON 生成
//!
//! M1 细粒度 state：Bridge 经 `State<Arc<dyn BridgeApi>>`、环境能力经
//! `State<Arc<dyn EnvironmentApi>>`、任务校验经 `State<Arc<dyn TaskApi>>`
//! 提取；LLM 配置读写与生成编排内聚在 [`crate::ai`]，本文件只做协议转换。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::ai::prompt::CaptureContext;
use crate::ai::{self, LlmSettings};
use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::environment::EnvironmentApi;
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};

/// capture 单次超时：导航 + networkidle 等待 + CDP 资源快照，宽于常规命令
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 脱敏后的 LLM 配置视图（API key 永不出站，只回是否已设置）
fn masked_view(settings: &LlmSettings) -> Value {
    json!({
        "base_url": settings.base_url,
        "model": settings.model,
        "has_api_key": !settings.api_key_enc.is_empty(),
    })
}

/// GET /api/ai/llm-config — 读取 LLM 配置（脱敏）
pub async fn get_llm_config(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = ai::load_llm_settings(&config.base_path());
    Ok(data(masked_view(&settings)))
}

/// PUT /api/ai/llm-config — 保存 LLM 配置
///
/// body: `{ base_url, model, api_key? }`。`api_key` 缺省表示保持不变，
/// 空串表示清除，非空表示更新（AES-256-GCM 加密落盘）。
pub async fn put_llm_config(
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("请求体必须为 JSON 对象".into()))?;
    let base_url_raw = obj
        .get("base_url")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("缺少 base_url".into()))?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if model.is_empty() {
        return Err(ApiError::BadRequest("模型名不能为空".into()));
    }
    let base_url = ai::validate_base_url(base_url_raw).map_err(ApiError::BadRequest)?;

    let base = config.base_path();
    let mut settings = ai::load_llm_settings(&base);
    settings.base_url = base_url;
    settings.model = model.to_string();
    match obj.get("api_key").and_then(Value::as_str) {
        None => {}
        Some("") => settings.api_key_enc = String::new(),
        Some(raw) => {
            settings.api_key_enc = ai::encrypt_api_key(raw)
                .map_err(|e| ApiError::Internal(format!("API Key 加密失败: {e}")))?;
        }
    }
    ai::save_llm_settings(&base, &settings)
        .map_err(|e| ApiError::Internal(format!("LLM 配置写入失败: {e}")))?;
    tracing::info!("LLM 配置已更新: model={}", settings.model);
    Ok(data(masked_view(&settings)))
}

/// POST /api/ai/capture — 捕获登录页面（导航 + 截图 + HTML/JS 落盘）
///
/// body: `{ url }`。产物固定写入 `captures/latest/`（Worker 侧落盘，响应只回
/// 轻量元数据——NDJSON 单行上限 1 MiB，门户页 HTML/JS 普遍超限）。
pub async fn capture(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::BadRequest("缺少 url".into()))?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ApiError::BadRequest("仅支持 http/https 页面捕获".into()));
    }
    // 环境门槛与调试/登录对齐：未就绪时先引导，失败以 503 明确回报
    environment
        .ensure_capability()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("Python 环境未就绪: {e}")))?;

    let rt = config.runtime_snapshot();
    let params = json!({
        "url": url,
        "browser_settings": serde_json::to_value(&rt.browser).unwrap_or(Value::Null),
        // 固定 cancel_id：与 OCR 同模式，取消端点可命中本请求
        "cancel_id": "ai-capture",
    });
    let resp = bridge
        .execute_with_timeout("page_capture", params, CAPTURE_TIMEOUT)
        .await?;
    if !resp.result.success {
        return Err(ApiError::Internal(
            resp.result.error.unwrap_or_else(|| "页面捕获失败".into()),
        ));
    }
    let mut payload = resp.result.data;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("screenshot_url".into(), json!("/api/ai/capture/screenshot"));
    }
    tracing::info!(url, "登录页捕获完成");
    Ok(data(payload))
}

/// GET /api/ai/capture/screenshot — 读取最近一次捕获的截图（只读 PNG）
///
/// `<img>` 引用无法携带自定义鉴权头（同 debug 截图豁免先例），路径固定于
/// `captures/latest/`，无用户输入参与，不存在穿越面。
pub async fn capture_screenshot(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<impl IntoResponse, ApiError> {
    let path = ai::capture_dir(&config.base_path()).join("screenshot.png");
    if !path.exists() {
        return Err(ApiError::NotFound("尚无捕获截图，请先执行捕获".into()));
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes))
}

/// POST /api/ai/generate — 由捕获产物生成任务 JSON
///
/// body: `{ extra_prompt? }`。流程：读 `captures/latest/` → 组装提示词
/// （schema 浓缩指南 + 截图 + HTML/JS）→ LLM 生成 → 强校验 → 错误回喂自纠一轮。
/// 返回的 JSON 未入库，前端预览/编辑后走 `/api/tasks/import` 保存。
pub async fn generate(
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let extra_prompt = body
        .get("extra_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let base = config.base_path();
    let settings = ai::load_llm_settings(&base);
    if !settings.is_configured() {
        return Err(ApiError::BadRequest(
            "请先配置 LLM 的 Base URL 与模型名".into(),
        ));
    }
    let api_key = if settings.api_key_enc.is_empty() {
        String::new()
    } else {
        ai::decrypt_api_key(&settings.api_key_enc)
            .map_err(|_| {
                ApiError::BadRequest(
                    "API Key 解密失败（密钥文件可能已轮转），请在配置区重新保存 API Key".into(),
                )
            })?
            .to_string()
    };

    let ctx = load_capture_context(&base).await?;
    let warnings = ctx.1;
    // 注入强校验：Value 小，clone 进 future 规避 HRTB 生命周期问题
    let validate = move |v: &Value| {
        let task = v.clone();
        let tasks = tasks.clone();
        async move { tasks.validate_task_json(&task).await }
    };
    let outcome = crate::ai::generate::generate_with(&ctx.0, extra_prompt, validate, |messages| {
        crate::ai::llm::chat_completion(&settings, &api_key, messages)
    })
    .await
    .map_err(ApiError::BadRequest)?;

    let mut all_warnings = warnings;
    all_warnings.extend(outcome.warnings);
    tracing::info!(
        attempts = outcome.attempts,
        model = %settings.model,
        "AI 任务生成完成"
    );
    Ok(data(json!({
        "task": outcome.task,
        "attempts": outcome.attempts,
        "warnings": all_warnings,
        "model": settings.model,
        "base_url": settings.base_url,
    })))
}

/// 从落盘产物组装生成上下文；(上下文, 非致命提示)
///
/// `meta.json` 为捕获契约锚点；截图是视觉模型的核心输入，缺失直接报错。
/// JS/CSS 不进 LLM 上下文（完整资源走「保存页面文件」下载），仅读 HTML + 截图。
async fn load_capture_context(
    base: &std::path::Path,
) -> Result<(CaptureContext, Vec<String>), ApiError> {
    let dir = ai::capture_dir(base);
    let meta_path = dir.join("meta.json");
    if !meta_path.exists() {
        return Err(ApiError::BadRequest(
            "尚无捕获产物，请先在上方执行页面捕获".into(),
        ));
    }
    let meta_bytes = tokio::fs::read(&meta_path).await?;
    let meta: Value = serde_json::from_slice(&meta_bytes)
        .map_err(|e| ApiError::Internal(format!("捕获元数据损坏: {e}")))?;
    let field = |name: &str| {
        meta.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    let html = tokio::fs::read_to_string(dir.join("page.html"))
        .await
        .map_err(|e| ApiError::Internal(format!("读取捕获 HTML 失败: {e}")))?;
    let screenshot_png = tokio::fs::read(dir.join("screenshot.png"))
        .await
        .map_err(|_| ApiError::BadRequest("捕获截图缺失，请重新执行页面捕获".into()))?;

    let mut warnings: Vec<String> = Vec::new();
    if let Some(note) = meta.get("note").and_then(Value::as_str) {
        warnings.push(note.to_string());
    }

    let ctx = CaptureContext {
        request_url: field("request_url"),
        final_url: field("final_url"),
        title: field("title"),
        html,
        screenshot_png,
        note: None,
    };
    Ok((ctx, warnings))
}

/// GET /api/ai/capture/bundle — 下载最近一次捕获的完整页面文件（zip）
///
/// 内容：MHTML 完整布局（自包含样式/图片）+ page.html + CSS/JS 资源快照 +
/// 截图 + meta.json。供离线分析或分享适配；需鉴权（不同于只读截图豁免）。
pub async fn capture_bundle(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<impl IntoResponse, ApiError> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let dir = ai::capture_dir(&config.base_path());
    if !dir.join("meta.json").exists() {
        return Err(ApiError::NotFound("尚无捕获产物，请先执行页面捕获".into()));
    }
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        // meta / HTML / MHTML / 截图：顶层固定名
        for name in ["meta.json", "page.html", "page.mhtml", "screenshot.png"] {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            let bytes = tokio::fs::read(&path).await?;
            zw.start_file(name, opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(&bytes)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        // resources/：捕获时经 CDP 抓取的 CSS/JS 快照
        let resources_dir = dir.join("resources");
        if let Ok(mut rd) = tokio::fs::read_dir(&resources_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if let Ok(bytes) = tokio::fs::read(entry.path()).await {
                    zw.start_file(format!("resources/{name}"), opts)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                    zw.write_all(&bytes)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                }
            }
        }
        zw.finish().map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let bytes = buf.into_inner();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let filename = format!("campus-auth-capture-{stamp}.zip");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post, put};
    use tower::ServiceExt; // oneshot

    use crate::bridge::{BridgeError, IpcResponse, IpcResult};
    use crate::environment::{BootstrapStage, EnvironmentApi, EnvironmentError, EnvironmentStatus};
    use crate::web::routes::test_support::{MockConfigApi, MockConfigInner};

    struct MockInner {
        executed: Vec<(String, Value)>,
        respond: (bool, Value, Option<String>),
        ensure_fails: bool,
    }

    impl Default for MockInner {
        fn default() -> Self {
            Self {
                executed: Vec::new(),
                respond: (
                    true,
                    json!({"final_url": "http://p/login", "title": "t"}),
                    None,
                ),
                ensure_fails: false,
            }
        }
    }

    struct MockBridgeApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl BridgeApi for MockBridgeApi {
        async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
            self.execute_with_timeout(method, params, std::time::Duration::ZERO)
                .await
        }

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: std::time::Duration,
        ) -> Result<IpcResponse, BridgeError> {
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

        fn cancel(&self, _cancel_id: &str) {}
        async fn force_recycle(&self) {}
        fn has_live_worker(&self) -> bool {
            false
        }
        async fn recycle_if_running(&self) {}
        async fn shutdown(&self) {}
        fn runtime_ocr_capability(&self) -> Option<bool> {
            None
        }
    }

    struct MockEnvironmentApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl EnvironmentApi for MockEnvironmentApi {
        fn status(&self) -> EnvironmentStatus {
            EnvironmentStatus {
                uv_ready: false,
                python_ready: false,
                playwright_ready: false,
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
            if self.0.lock().unwrap().ensure_fails {
                return Err(EnvironmentError::BootstrapFailedShared(
                    "mock 环境缺失".into(),
                ));
            }
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

    /// 校验型 TaskApi：仅按 name 是否为空做判定（generate 链路只依赖校验行为）
    struct MockTaskApi;

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<crate::tasks::TaskSummary> {
            Vec::new()
        }
        async fn load_task(
            &self,
            _task_id: &str,
        ) -> Result<crate::tasks::TaskKind, crate::tasks::TaskError> {
            Err(crate::tasks::TaskError::TaskNotFound("x".into()))
        }
        async fn embed_task_config(&self, _task_id: &str, _params: &mut Value) -> bool {
            false
        }
        async fn save_task(
            &self,
            _task_id: &str,
            _task: &crate::tasks::TaskKind,
        ) -> Result<(), crate::tasks::TaskError> {
            Ok(())
        }
        async fn delete_task(&self, _task_id: &str) -> Result<(), crate::tasks::TaskError> {
            Ok(())
        }
        async fn get_active_task(&self) -> String {
            String::new()
        }
        async fn set_active_task(&self, _task_id: &str) -> Result<(), crate::tasks::TaskError> {
            Ok(())
        }
        async fn get_task_detail(
            &self,
            _task_id: &str,
        ) -> Result<crate::tasks::TaskDetail, crate::tasks::TaskError> {
            Err(crate::tasks::TaskError::TaskNotFound("x".into()))
        }
        async fn load_order(&self) -> crate::tasks::OrderData {
            crate::tasks::OrderData::default()
        }
        async fn save_order(
            &self,
            _order: &crate::tasks::OrderData,
        ) -> Result<(), crate::tasks::TaskError> {
            Ok(())
        }
        async fn get_script_path(&self, _task_id: &str) -> Option<std::path::PathBuf> {
            None
        }
        fn has_task(&self, _task_id: &str) -> bool {
            false
        }
        async fn validate_task_json(&self, config: &Value) -> Result<(), Vec<String>> {
            let name_empty = config
                .get("name")
                .and_then(Value::as_str)
                .map(str::is_empty)
                .unwrap_or(true);
            if name_empty {
                Err(vec!["name 不能为空".into()])
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone)]
    struct TestState {
        config: Arc<dyn ConfigApi>,
        bridge: Arc<dyn BridgeApi>,
        env: Arc<dyn EnvironmentApi>,
        tasks: Arc<dyn TaskApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn ConfigApi> {
        fn from_ref(state: &TestState) -> Self {
            state.config.clone()
        }
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
    impl axum::extract::FromRef<TestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &TestState) -> Self {
            state.tasks.clone()
        }
    }

    fn mock_app() -> (
        axum::Router,
        Arc<std::sync::Mutex<MockInner>>,
        Arc<std::sync::Mutex<MockConfigInner>>,
        tempfile::TempDir,
    ) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner::default()));
        let (config, cfg_inner) = MockConfigApi::mocked();
        let dir = tempfile::tempdir().unwrap();
        // 预置 <base>/python_worker 使 worker_project_dir 命中 tempdir，
        // 隔离 dev 回退（CARGO_MANIFEST_DIR/python_worker）下的真实捕获产物
        std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
        cfg_inner.lock().unwrap().base_path = dir.path().to_path_buf();
        let state = TestState {
            config,
            bridge: Arc::new(MockBridgeApi(inner.clone())),
            env: Arc::new(MockEnvironmentApi(inner.clone())),
            tasks: Arc::new(MockTaskApi),
        };
        let app = axum::Router::new()
            .route("/api/ai/llm-config", get(get_llm_config))
            .route("/api/ai/llm-config", put(put_llm_config))
            .route("/api/ai/capture", post(capture))
            .route(
                "/api/ai/capture/screenshot",
                get(capture_screenshot).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
            )
            .route("/api/ai/capture/bundle", get(capture_bundle))
            .route("/api/ai/generate", post(generate))
            .with_state(state);
        (app, inner, cfg_inner, dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// GET 配置：未配置时返回空串 + has_api_key=false
    #[tokio::test]
    async fn test_get_llm_config_unconfigured() {
        let (app, _, _, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ai/llm-config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["base_url"], "");
        assert_eq!(v["data"]["has_api_key"], false);
    }

    /// PUT 配置：URL 规范化 + key 加密落盘 + 响应不含明文 key
    #[tokio::test]
    async fn test_put_llm_config_encrypts_and_normalizes() {
        let (app, _, _, dir) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/ai/llm-config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"base_url": "https://api.example.com/v1/", "model": "glm-4v-flash", "api_key": "sk-abc"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["base_url"], "https://api.example.com/v1");
        assert_eq!(v["data"]["has_api_key"], true);
        assert!(!v.to_string().contains("sk-abc"), "响应不得包含明文 key");

        // 落盘校验：密文带 ENC: 前缀，可解回原文
        let raw = std::fs::read_to_string(ai::llm_config_path(dir.path())).unwrap();
        assert!(raw.contains("ENC:"));
        let settings = ai::load_llm_settings(dir.path());
        assert_eq!(
            &*ai::decrypt_api_key(&settings.api_key_enc).unwrap(),
            "sk-abc"
        );
    }

    /// PUT 配置：非法 URL（userinfo / 非 http 协议）→ 400 且不落盘
    #[tokio::test]
    async fn test_put_llm_config_rejects_bad_url() {
        let (app, _, _, dir) = mock_app();
        for bad in ["https://key@evil.com", "ftp://x.com", ""] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/api/ai/llm-config")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"base_url": bad, "model": "m"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "url={bad}");
        }
        assert!(
            !ai::llm_config_path(dir.path()).exists(),
            "校验失败不得落盘"
        );
    }

    /// PUT 配置：api_key 缺省保持原值，空串清除
    #[tokio::test]
    async fn test_put_llm_config_key_semantics() {
        let (app, _, _, dir) = mock_app();
        // 首次设置 key
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/ai/llm-config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"base_url": "https://a.com", "model": "m", "api_key": "sk-1"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 缺省 api_key：key 保持
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/ai/llm-config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"base_url": "https://b.com", "model": "m2"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings = ai::load_llm_settings(dir.path());
        assert_eq!(
            &*ai::decrypt_api_key(&settings.api_key_enc).unwrap(),
            "sk-1"
        );
        assert_eq!(settings.model, "m2");

        // 空串：清除
        let _ = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/ai/llm-config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"base_url": "https://b.com", "model": "m2", "api_key": ""})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings = ai::load_llm_settings(dir.path());
        assert!(settings.api_key_enc.is_empty());
    }

    /// capture：注入 browser_settings 与 cancel_id，派发 page_capture，响应补截图 URL
    #[tokio::test]
    async fn test_capture_dispatches_with_settings() {
        let (app, inner, cfg, _) = mock_app();
        cfg.lock().unwrap().runtime.browser.timeout = 7;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/capture")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"url": "http://portal/"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        {
            let guard = inner.lock().unwrap();
            assert_eq!(guard.executed[0].0, "page_capture");
            assert_eq!(guard.executed[0].1["url"], "http://portal/");
            assert_eq!(guard.executed[0].1["cancel_id"], "ai-capture");
            assert!(guard.executed[0].1.get("browser_settings").is_some());
        }
        let v = body_json(resp).await;
        assert_eq!(v["data"]["screenshot_url"], "/api/ai/capture/screenshot");
    }

    /// capture：URL 缺失/协议非法 → 400；环境未就绪 → 503 不触达 Bridge
    #[tokio::test]
    async fn test_capture_validates_url_and_environment() {
        let (app, inner, _cfg, _) = mock_app();
        for body in [json!({}), json!({"url": "ftp://x"})] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/ai/capture")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }
        inner.lock().unwrap().ensure_fails = true;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/capture")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"url": "http://x"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(inner.lock().unwrap().executed.is_empty());
    }

    /// worker 失败 → 500 透传错误消息
    #[tokio::test]
    async fn test_capture_worker_failure_maps_to_error() {
        let (app, inner, _, _) = mock_app();
        inner.lock().unwrap().respond = (false, Value::Null, Some("存在活跃调试会话".into()));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/capture")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"url": "http://x"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["message"], "存在活跃调试会话");
    }

    /// 截图端点：无产物 404，有产物 200 image/png
    #[tokio::test]
    async fn test_capture_screenshot_hit_and_miss() {
        let (app, _, _, dir) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ai/capture/screenshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let shot_dir = ai::capture_dir(dir.path());
        std::fs::create_dir_all(&shot_dir).unwrap();
        std::fs::write(shot_dir.join("screenshot.png"), b"\x89PNG-hit").unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ai/capture/screenshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "image/png");
    }

    /// bundle：无产物 404；有产物 200 zip 且包含 MHTML/HTML/资源/截图/meta
    #[tokio::test]
    async fn test_capture_bundle_hit_and_miss() {
        let (app, _, _, dir) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ai/capture/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let cap = ai::capture_dir(dir.path());
        let res = cap.join("resources");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(cap.join("meta.json"), b"{}").unwrap();
        std::fs::write(cap.join("page.html"), b"<html></html>").unwrap();
        std::fs::write(cap.join("page.mhtml"), b"MIME-Version: 1.0").unwrap();
        std::fs::write(cap.join("screenshot.png"), b"\x89PNG").unwrap();
        std::fs::write(res.join("main.js"), b"console.log(1)").unwrap();
        std::fs::write(res.join("style.css"), b"body{}").unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/ai/capture/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "application/zip");
        assert!(
            resp.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment; filename=\"campus-auth-capture-")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        for expect in [
            "meta.json",
            "page.html",
            "page.mhtml",
            "screenshot.png",
            "resources/main.js",
            "resources/style.css",
        ] {
            assert!(
                names.contains(&expect.to_string()),
                "missing {expect}: {names:?}"
            );
        }
    }

    /// generate：未配置 LLM → 400；未捕获 → 400
    #[tokio::test]
    async fn test_generate_requires_config_and_capture() {
        let (app, _, _, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(v["error"]["message"].as_str().unwrap().contains("请先配置"));

        // 配置后但未捕获
        let (app, _, _, dir) = mock_app();
        let settings = LlmSettings {
            base_url: "https://a.com".into(),
            model: "m".into(),
            api_key_enc: String::new(),
        };
        ai::save_llm_settings(dir.path(), &settings).unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(v["error"]["message"].as_str().unwrap().contains("捕获"));
    }
}
