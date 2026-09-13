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
use tokio_util::sync::CancellationToken;

use crate::ai::prompt::CaptureContext;
use crate::ai::{self, LlmSettings};
use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::environment::EnvironmentApi;
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};
use crate::web::operations::{OperationRegistration, WebOperations};

/// capture 单次超时：导航 + networkidle 等待 + CDP 资源快照，宽于常规命令
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 脱敏后的 LLM 配置视图（API key 永不出站，只回是否已设置）
fn masked_view(settings: &LlmSettings) -> Value {
    json!({
        "provider": settings.provider,
        "base_url": settings.base_url,
        "model": settings.model,
        "has_api_key": settings.has_active_api_key(),
        "configured_providers": settings.configured_providers(),
        "max_tokens": settings.max_tokens,
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
/// body: `{ provider?, base_url, model, api_key?, max_tokens? }`。`api_key` 缺省表示保持不变，
/// 空串表示清除，非空表示更新（AES-256-GCM 加密落盘）；`max_tokens` 为整数或
/// `null`（不携带该字段，交由服务商默认），缺省表示保持不变。
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
    let base_url = ai::validate_base_url(base_url_raw)?;
    let provider = match obj.get("provider").and_then(Value::as_str) {
        Some(raw) => ai::validate_provider(raw)?,
        None => ai::infer_provider(&base_url).to_string(),
    };
    ai::validate_provider_base_url(&provider, &base_url)?;

    let base = config.base_path();
    let mut settings = ai::load_llm_settings(&base);
    settings.provider = provider;
    settings.base_url = base_url;
    settings.model = model.to_string();
    match obj.get("api_key").and_then(Value::as_str) {
        None => {}
        Some(raw) if raw.trim().is_empty() => settings.set_active_api_key_enc(String::new()),
        Some(raw) => {
            let encrypted = ai::encrypt_api_key(raw.trim())
                .map_err(|e| ApiError::Internal(format!("API Key 加密失败: {e}")))?;
            settings.set_active_api_key_enc(encrypted);
        }
    }
    if let Some(mt) = obj.get("max_tokens") {
        settings.max_tokens = if mt.is_null() {
            None
        } else {
            Some(
                mt.as_u64()
                    .filter(|v| (1..=200_000).contains(v))
                    .ok_or_else(|| {
                        ApiError::BadRequest("max_tokens 必须为 1..=200000 的整数或 null".into())
                    })? as u32,
            )
        };
    }
    ai::save_llm_settings(&base, &settings)
        .map_err(|e| ApiError::Internal(format!("LLM 配置写入失败: {e}")))?;
    tracing::info!(provider = %settings.provider, model = %settings.model, "LLM 配置已更新");
    Ok(data(masked_view(&settings)))
}

/// POST /api/ai/llm-config/test — 用已保存配置发送极小请求，验证接口与凭据。
pub async fn test_llm_connection(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = ai::load_llm_settings(&config.base_path());
    if !settings.is_configured() {
        return Err(ApiError::BadRequest("请先保存 LLM 配置".into()));
    }
    settings.max_tokens = Some(8);
    let api_key = if settings.active_api_key_enc().is_empty() {
        zeroize::Zeroizing::new(String::new())
    } else {
        ai::decrypt_api_key(settings.active_api_key_enc()).map_err(|_| {
            ApiError::BadRequest("API Key 解密失败，请重新保存当前服务商的 Key".into())
        })?
    };
    let started = std::time::Instant::now();
    let messages = vec![
        json!({"role": "system", "content": "只回复 OK"}),
        json!({"role": "user", "content": "连接测试"}),
    ];
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::ai::llm::chat_completion(&settings, api_key.as_str(), messages),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("连接测试超时（30 秒）".into()))?
    .map_err(|error| ApiError::ServiceUnavailable(error.to_string()))?;
    Ok(data(json!({
        "connected": true,
        "latency_ms": started.elapsed().as_millis(),
        "request_url": format!("{}/chat/completions", settings.base_url.trim_end_matches('/')),
    })))
}

/// POST /api/ai/capture — 捕获登录页面（导航 + 截图 + HTML/JS 落盘）
///
/// body: `{ url }`。产物固定写入 `captures/latest/`（Worker 侧落盘，响应只回
/// 轻量元数据——NDJSON 单行上限 1 MiB，门户页 HTML/JS 普遍超限）。
pub async fn capture(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    State(operations): State<Arc<WebOperations>>,
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
    // capture 与 generate 共用单飞登记器，固定 captures/latest 在任一时刻只允许
    // 一个读者或写者，避免生成读取到捕获覆盖一半的文件集合。
    let capture_operation = operations
        .ai_generation()
        .register(format!("ai-capture-{}", uuid::Uuid::new_v4()))
        .map_err(|_| ApiError::Conflict("页面捕获或任务生成正在进行，请稍后重试".into()))?;
    // 环境门槛与调试/登录对齐：未就绪时先引导，失败以 503 明确回报
    environment
        .ensure_capability()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("Python 环境未就绪: {e}")))?;

    let rt = config.runtime_snapshot();
    // 每请求唯一 cancel_id：固定 id 会与 CancelRegistry 的 pending TTL 串扰
    //（无在途请求时的迟到取消会命中 60s 内的下一次同 id 请求）
    let params = json!({
        "url": url,
        "browser_settings": serde_json::to_value(&rt.browser).unwrap_or(Value::Null),
        "cancel_id": format!("ai-capture-{}", uuid::Uuid::new_v4()),
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
    capture_operation.finish();
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

/// GET /api/ai/capture/status — 查询最近一次捕获产物是否可用
///
/// 页面刷新后前端据此恢复捕获状态（避免强制重新捕获）；meta 损坏按不可用处理。
pub async fn capture_status(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let dir = ai::capture_dir(&config.base_path());
    let available = dir.join("meta.json").exists()
        && dir.join("page.html").exists()
        && dir.join("screenshot.png").exists();
    if !available {
        return Ok(data(json!({ "available": false })));
    }
    let meta: Value = tokio::fs::read(dir.join("meta.json"))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    if meta.is_null() {
        return Ok(data(json!({ "available": false })));
    }
    let field = |name: &str| {
        meta.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Ok(data(json!({
        "available": true,
        "request_url": field("request_url"),
        "final_url": field("final_url"),
        "title": field("title"),
        "structure_summary": meta.get("structure_summary").cloned().unwrap_or(Value::Null),
    })))
}

/// 获取在途 AI 捕获/生成登记（防重入）：已有同域操作在途时返回 409。
fn acquire_generation(operations: &WebOperations) -> Result<OperationRegistration, ApiError> {
    operations
        .ai_generation()
        .register(format!("ai-generate-{}", uuid::Uuid::new_v4()))
        .map_err(|_| ApiError::Conflict("页面捕获或任务生成正在进行，请稍后重试".into()))
}

/// 事件转发器：每 40ms 把共享缓冲的新事件刷到 SSE 通道。
///
/// MutexGuard 不跨 await；终止事件（Done/Error）随本批一起取出作为退出判据
/// （终止事件由生成器最后恰好 push 一次）。接收端消失即取消生成令牌。
fn spawn_event_forwarder(
    shared: std::sync::Arc<std::sync::Mutex<Vec<crate::ai::generate::StreamEvent>>>,
    tx: tokio::sync::mpsc::Sender<crate::ai::generate::StreamEvent>,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(40)) => {}
            }
            // 一步取走全部未转发事件（drain 后 Vec 清空，免去手写游标 idx 的分页）；
            // 终止事件随本批一起取出，作为本轮退出判据（终止事件由生成器最后恰好 push 一次）
            let batch: Vec<crate::ai::generate::StreamEvent> = {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.drain(..).collect()
            };
            let should_exit = batch.iter().any(|e| {
                matches!(
                    e,
                    crate::ai::generate::StreamEvent::Done { .. }
                        | crate::ai::generate::StreamEvent::Error { .. }
                )
            });
            for ev in batch {
                let _ = tx.send(ev).await;
            }
            if tx.is_closed() {
                cancel_token.cancel();
                break;
            }
            if should_exit {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                break;
            }
        }
    })
}

/// 生成收尾：把编排终态（Done/Error）推进共享缓冲，并等待转发器排空。
async fn finalize_generation(
    shared: &std::sync::Arc<std::sync::Mutex<Vec<crate::ai::generate::StreamEvent>>>,
    forward_handle: tokio::task::JoinHandle<()>,
    outcome: Result<crate::ai::generate::GenerateOutcome, crate::ai::AiError>,
    base_warnings: &[String],
) {
    match outcome {
        Ok(o) => {
            let mut warnings = base_warnings.to_vec();
            warnings.extend(o.warnings.clone());
            shared.lock().unwrap_or_else(|p| p.into_inner()).push(
                crate::ai::generate::StreamEvent::Done {
                    attempts: o.attempts,
                    warnings,
                    task: o.task,
                },
            );
            let _ = forward_handle.await;
        }
        Err(e) => {
            shared.lock().unwrap_or_else(|p| p.into_inner()).push(
                crate::ai::generate::StreamEvent::Error {
                    message: e.to_string(),
                },
            );
            let _ = forward_handle.await;
        }
    }
}

/// POST /api/ai/generate/stream — 流式生成（SSE）
///
/// 与 `generate` 语义一致，但以 `text/event-stream` 实时推送进度：
/// 每个 LLM 增量、校验/重试状态均以 `data: <json>` 帧发出，前端据此在
/// 最下方同步展示进度（流式文本预览 + 步骤状态）。请求同时受分块空闲超时与
/// 10 分钟总预算约束，持续输出也不会超过总预算。
pub async fn generate_stream(
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
    State(operations): State<Arc<WebOperations>>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, ApiError> {
    let extra_prompt = body
        .get("extra_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(4_000).collect::<String>());

    let base = config.base_path();
    let settings = ai::load_llm_settings(&base);
    if !settings.is_configured() {
        return Err(ApiError::BadRequest(
            "请先配置 LLM 的 Base URL 与模型名".into(),
        ));
    }
    let api_key = if settings.active_api_key_enc().is_empty() {
        zeroize::Zeroizing::new(String::new())
    } else {
        ai::decrypt_api_key(settings.active_api_key_enc()).map_err(|_| {
            ApiError::BadRequest(
                "API Key 解密失败（密钥文件可能已轮转），请在配置区重新保存 API Key".into(),
            )
        })?
    };
    let api_key = Arc::new(api_key);

    // 必须先登记再读取 captures/latest，确保 capture 不会在多文件读取期间覆盖目录。
    let generation = acquire_generation(&operations)?;
    let ctx = load_capture_context(&base).await?;
    let capture_warnings = ctx.1;
    let capture_ctx = ctx.0;
    let model = settings.model.clone();
    let base_url = settings.base_url.clone();

    let cancel_token = generation.cancellation_token();

    let (tx, rx) = tokio::sync::mpsc::channel::<crate::ai::generate::StreamEvent>(1024);
    let shared: std::sync::Arc<std::sync::Mutex<Vec<crate::ai::generate::StreamEvent>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let shared_for_gen = shared.clone();
    let shared_for_forward = shared.clone();
    let tx_for_forward = tx.clone();
    let token_for_forward = cancel_token.clone();

    // 转发器：每 40ms 把新事件刷到 SSE（MutexGuard 不跨 await）。
    // 接收端消失（客户端断连/响应流结束）即取消生成令牌，停止无谓的 LLM 消耗
    let forward_handle =
        spawn_event_forwarder(shared_for_forward, tx_for_forward, token_for_forward);

    let extra_prompt_bg = extra_prompt.clone();
    let tasks_bg = tasks.clone();
    let token_for_gen = cancel_token.clone();
    tokio::spawn(async move {
        let validate = move |v: &Value| {
            let task = v.clone();
            let tasks = tasks_bg.clone();
            async move { tasks.validate_task_json(&task).await }
        };
        let token = token_for_gen.clone();
        let outcome = crate::ai::generate::generate_with_stream(
            &capture_ctx,
            extra_prompt_bg.as_deref(),
            validate,
            {
                let settings = settings.clone();
                let api_key = api_key.clone();
                move |messages, on_delta| {
                    let settings = settings.clone();
                    let api_key = api_key.clone();
                    let token = token.clone();
                    async move {
                        crate::ai::llm::chat_completion_with_stream(
                            &settings,
                            api_key.as_str(),
                            messages,
                            Some(on_delta),
                            Some(&token),
                        )
                        .await
                    }
                }
            },
            shared_for_gen,
        );
        let outcome = match tokio::time::timeout(crate::ai::llm::TOTAL_BUDGET, outcome).await {
            Ok(result) => result,
            Err(_) => Err(crate::ai::AiError::TotalBudgetExceeded),
        };
        finalize_generation(&shared, forward_handle, outcome, &capture_warnings).await;
        // 显式放在终态事件排空之后；若中途 panic/abort，RAII Drop 仍会释放并取消。
        drop(generation);
    });

    let stream = async_stream::stream! {
        let start = serde_json::json!({ "type": "started", "model": model, "base_url": base_url });
        yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", start));
        let mut rx = rx;
        let mut idle_ticks: u32 = 0;
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(e) => {
                            idle_ticks = 0;
                            let line = serde_json::to_string(&e).unwrap_or_else(|_| "{}".into());
                            yield Ok(format!("data: {}\n\n", line));
                            if matches!(e, crate::ai::generate::StreamEvent::Done { .. } | crate::ai::generate::StreamEvent::Error { .. }) {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {
                    idle_ticks += 1;
                    yield Ok(": keepalive\n\n".to_string());
                    if idle_ticks >= 40 {
                        let err = serde_json::json!({ "type": "error", "message": "流式空闲超时（10 分钟无输出）" });
                        yield Ok(format!("data: {}\n\n", err));
                        break;
                    }
                }
            }
        }
    };

    let body = axum::body::Body::from_stream(stream);
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream".to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                "no".to_string(),
            ),
        ],
        body,
    ))
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
    let structure = match tokio::fs::read(dir.join("page_structure.json")).await {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                warnings.push(format!("结构化页面材料损坏，已回退原始 HTML: {error}"));
                None
            }
        },
        Err(_) => {
            warnings.push("未找到结构化页面材料，已回退原始 HTML".to_string());
            None
        }
    };

    if let Some(note) = meta.get("note").and_then(Value::as_str) {
        warnings.push(note.to_string());
    }

    let ctx = CaptureContext {
        request_url: field("request_url"),
        final_url: field("final_url"),
        title: field("title"),
        html,
        structure,
        screenshot_png,
        note: None,
    };
    Ok((ctx, warnings))
}

/// GET /api/ai/capture/bundle — 下载最近一次捕获的完整页面文件（zip）
///
/// 内容：MHTML 完整布局（自包含样式/图片）+ page.html + page_structure.json +
/// CSS/JS 资源快照 + 截图 + meta.json。供离线分析或分享适配；需鉴权。
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
        for name in [
            "meta.json",
            "page.html",
            "page_structure.json",
            "page.mhtml",
            "screenshot.png",
        ] {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            let bytes = tokio::fs::read(&path).await?;
            zw.start_file(name, opts).map_err(ApiError::internal)?;
            zw.write_all(&bytes).map_err(ApiError::internal)?;
        }
        // resources/：捕获时经 CDP 抓取的 CSS/JS 快照
        let resources_dir = dir.join("resources");
        if let Ok(mut rd) = tokio::fs::read_dir(&resources_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if let Ok(bytes) = tokio::fs::read(entry.path()).await {
                    zw.start_file(format!("resources/{name}"), opts)
                        .map_err(ApiError::internal)?;
                    zw.write_all(&bytes).map_err(ApiError::internal)?;
                }
            }
        }
        zw.finish().map_err(ApiError::internal)?;
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
                worker_ready: false,
                manifest_current: false,
                playwright_ready: false,
                system_browser_ready: false,
                ocr_enabled: false,
                ocr_ready: false,
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
        fn browser_engine_ready(&self, _engine: &str) -> bool {
            // 测试替身：不接管真实浏览器缓存探测
            false
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
        operations: Arc<WebOperations>,
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
    impl axum::extract::FromRef<TestState> for Arc<WebOperations> {
        fn from_ref(state: &TestState) -> Self {
            state.operations.clone()
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
            operations: Arc::new(WebOperations::new()),
        };
        let app = axum::Router::new()
            .route("/api/ai/llm-config", get(get_llm_config))
            .route("/api/ai/llm-config", put(put_llm_config))
            .route("/api/ai/llm-config/test", post(test_llm_connection))
            .route("/api/ai/capture", post(capture))
            .route(
                "/api/ai/capture/screenshot",
                get(capture_screenshot).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
            )
            .route("/api/ai/capture/bundle", get(capture_bundle))
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

    #[tokio::test]
    async fn test_llm_connection_requires_saved_config() {
        let (app, _, _, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/llm-config/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
            &*ai::decrypt_api_key(settings.active_api_key_enc()).unwrap(),
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
                        json!({"provider": "deepseek", "base_url": "https://api.deepseek.com", "model": "m", "api_key": "sk-1"})
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
                        json!({"provider": "deepseek", "base_url": "https://api.deepseek.com/v1", "model": "m2"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings = ai::load_llm_settings(dir.path());
        assert_eq!(
            &*ai::decrypt_api_key(settings.active_api_key_enc()).unwrap(),
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
                        json!({"provider": "deepseek", "base_url": "https://api.deepseek.com/v1", "model": "m2", "api_key": ""})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings = ai::load_llm_settings(dir.path());
        assert!(!settings.has_active_api_key());
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
            assert!(
                guard.executed[0].1["cancel_id"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("ai-capture-"),
                "cancel_id 应为每请求唯一前缀"
            );
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
}
