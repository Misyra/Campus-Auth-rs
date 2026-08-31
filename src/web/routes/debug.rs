//! 调试路由：调试会话管理（通过 Bridge 执行调试命令）
//!
//! M1 细粒度 state：`start_debug` 经 `State<Arc<dyn TaskApi>>` 嵌入任务配置，
//! Bridge 命令派发经 `State<Arc<dyn BridgeApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};
use crate::web::state::AppState;

/// POST /api/debug/start — 启动调试会话
///
/// 前端仅提供 `task_id` 时，按 id 加载浏览器任务配置并嵌入 `task_config` 键，
/// 与 Python Worker 的 `debug_start` 契约一致（步骤载体为 `task_config`）。
/// 同时注入活跃 Profile 的系统保留变量，使登录类任务的 {{USERNAME}}/{{LOGIN_URL}}
/// 等模板在调试时的行为与真实执行一致（此前不注入会导航到 `{{LOGIN_URL}}` 字面量）。
pub async fn start_debug(
    State(tasks): State<Arc<dyn TaskApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn crate::environment::EnvironmentApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 环境就绪门槛与登录/任务执行对齐：此前调试直接走 Bridge，spawn 前只检查
    // .venv/python.exe 文件存在——环境面板显示"未就绪"时调试却仍能启动浏览器。
    // ensure_capability 的引导前快速检查会顺带刷新 EnvironmentStatus（面板自愈）；
    // 环境真缺失时自动触发引导（与手动登录同语义），失败以 503 明确回报。
    environment
        .ensure_capability()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("Python 环境未就绪: {e}")))?;
    let mut params = body.clone();
    if let Some(task_id) = body.get("task_id").and_then(|v| v.as_str()) {
        // 显式传入 task_config 时不覆盖；否则按 id 嵌入浏览器任务配置
        if !task_id.is_empty() && body.get("task_config").is_none() {
            tasks.embed_task_config(task_id, &mut params).await;
        }
    }
    let rt = config.runtime_snapshot();
    let profile = &rt.profile;
    let entry = params
        .as_object_mut()
        .ok_or_else(|| ApiError::BadRequest("调试参数必须为 JSON 对象".into()))?;
    // 下发浏览器设置（与登录/任务执行一致）：缺失时 Worker 按默认 headless=true
    // 启动，导致调试会话永远无窗口，用户的"关闭无头模式"设置对调试不生效
    entry
        .entry("browser_settings".to_string())
        .or_insert_with(|| serde_json::to_value(&rt.browser).unwrap_or(Value::Null));
    // 显式提供的字段不被覆盖（允许调试时临时替换单个变量）
    entry
        .entry("username".to_string())
        .or_insert_with(|| profile.username.clone().into());
    entry
        .entry("password".to_string())
        .or_insert_with(|| profile.password.to_string().into());
    entry
        .entry("isp".to_string())
        .or_insert_with(|| profile.isp.clone().into());
    entry
        .entry("auth_url".to_string())
        .or_insert_with(|| profile.auth_url.clone().into());
    let resp = bridge.execute("debug_start", params).await?;
    // 只取 IPC 载荷（result.data）：序列化整个 IpcResponse 会带上 id/result 包装，
    // 前端 request() 只解一层 data，syncSession 拿到包装结构后 steps/running 全丢
    Ok(data(resp.result.data))
}

/// POST /api/debug/step — 调试单步执行
///
/// 透传请求体（可携带 `step` 完整配置或 `step_index`）。前端“下一步”无显式索引时，
/// 由 Python Worker 依据会话内游标自动执行下一个尚未运行的步骤。
pub async fn step_debug(
    State(bridge): State<Arc<dyn BridgeApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let params = if body.is_null() {
        serde_json::json!({})
    } else {
        body
    };
    let resp = bridge.execute("debug_step", params).await?;
    Ok(data(resp.result.data))
}

/// POST /api/debug/stop — 停止调试会话
pub async fn stop_debug(State(bridge): State<Arc<dyn BridgeApi>>) -> Result<Json<Value>, ApiError> {
    let resp = bridge.execute("debug_stop", Value::Null).await?;
    Ok(data(resp.result.data))
}

/// GET /api/debug/status — 查询是否存在活跃调试会话
///
/// 调试会话是 Worker 侧的持久状态：前端刷新/换页后内存状态丢失，界面上无法
/// 停止会话，登录等命令会一直撞上"Worker 忙: 调试会话进行中"。此端点供前端
/// 启动时恢复会话感知（配合"停止调试"入口自助解除占用）。
pub async fn debug_status(State(bridge): State<Arc<dyn BridgeApi>>) -> Json<Value> {
    let screenshot_url = bridge.last_screenshot_url();
    if !bridge.debug_session_active() {
        return Json(json!({ "data": { "active": false, "screenshot_url": screenshot_url } }));
    }
    // 会话活跃：向 Worker 查询完整会话详情（步骤列表/执行结果），无副作用；
    // 前端刷新后据此恢复面板，否则只剩"0/0 无步骤数据"骨架
    let session = match bridge
        .execute_with_timeout("debug_status", json!({}), std::time::Duration::from_secs(5))
        .await
    {
        Ok(resp) => resp.result.data,
        Err(e) => {
            tracing::warn!(target: "python_worker", "调试会话详情查询失败: {e}");
            Value::Null
        }
    };
    Json(json!({
        "data": {
            "active": true,
            "screenshot_url": screenshot_url,
            "session": session,
        }
    }))
}

/// POST /api/debug/run-all — 执行调试会话中全部剩余步骤
pub async fn run_all(State(bridge): State<Arc<dyn BridgeApi>>) -> Result<Json<Value>, ApiError> {
    let resp = bridge.execute("debug_run_all", Value::Null).await?;
    Ok(data(resp.result.data))
}

/// GET /api/debug/screenshot/{filename} — 读取调试截图文件
///
/// `<img>` 引用无法携带自定义鉴权头（同背景图 GET 豁免先例），且 Worker 侧
/// 截图落盘路径对浏览器不可达，必须经 HTTP 暴露；文件名做防穿越校验。
pub async fn debug_screenshot(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use crate::environment::resolve_worker_project_path;
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let dir = resolve_worker_project_path(&state.config.base_path()).join("debug");
    let path = dir.join(&filename);
    if !path.starts_with(&dir) {
        return Err(ApiError::BadRequest("非法文件路径".into()));
    }
    if !path.exists() {
        return Err(ApiError::NotFound(format!("截图 {} 不存在", filename)));
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes))
}

/// POST /api/debug/feedback-bundle — 导出问题报告（zip）
///
/// 收集：日志尾段 + 当前活动任务 JSON + 脱敏配置快照 + 调试页 MHTML/page.html/
/// 截图/CSS-JS 资源快照（若有会话；Chromium MHTML 不含 JS，资源由 Worker 经 CDP 补齐）。
/// 失败项写占位 txt，不以 500 打断整包；无活跃调试会话时页面项为占位说明。
pub async fn feedback_bundle(
    State(bridge): State<std::sync::Arc<dyn crate::bridge::BridgeApi>>,
    State(tasks): State<std::sync::Arc<dyn crate::tasks::TaskApi>>,
    State(config): State<std::sync::Arc<dyn crate::config::ConfigApi>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let rt = config.runtime_snapshot();
    let active_task_id = tasks.get_active_task().await;
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();

    // 1) 日志尾段（spawn_blocking 避免阻塞）
    let base = config.base_path();
    let log_tail: Option<String> = tokio::task::spawn_blocking(move || {
        let logs_dir = base.join("logs");
        let latest = std::fs::read_dir(&logs_dir).ok().and_then(|entries| {
            let mut files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.starts_with("app.log"))
                        .unwrap_or(false)
                })
                .collect();
            files.sort_by_key(|a| std::cmp::Reverse(a.file_name()));
            files.into_iter().next()
        });
        latest.and_then(|e| super::system::read_log_tail(&e.path()))
    })
    .await
    .ok()
    .flatten();

    // 2) 活动任务 JSON（不存在则占位）
    let (task_json, task_filename) = if active_task_id.is_empty() {
        (None, "active_task-missing.txt".to_string())
    } else {
        match tasks.load_task(&active_task_id).await {
            Ok(kind) => {
                let v = serde_json::to_value(&kind).unwrap_or(serde_json::Value::Null);
                let s = serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("{e}"));
                (Some(s), format!("tasks/{}.json", active_task_id))
            }
            Err(e) => (
                Some(format!("加载活动任务 {active_task_id} 失败: {e}")),
                "active_task-missing.txt".to_string(),
            ),
        }
    };

    // 3) 脱敏配置快照
    let meta = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": now.to_rfc3339(),
        "active_task": active_task_id,
        "active_profile": rt.profile.id,
        "browser_channel": rt.browser.browser_channel,
        "has_password": !rt.profile.password.as_str().is_empty(),
        "auth_url": rt.profile.auth_url,
        "isp": rt.profile.isp,
    });
    let meta_str = serde_json::to_string_pretty(&meta).unwrap_or_default();

    // 4) 页面捕获（有调试会话时经 Worker 拿 MHTML/HTML + 截图 + CSS/JS 资源）
    let mut page_html: Option<String> = None;
    let mut page_mhtml: Option<Vec<u8>> = None;
    let mut page_png: Option<Vec<u8>> = None;
    // CSS/JS 资源快照（debug/resources/，Chromium MHTML 不含 JS 故由 Worker 补齐）
    let mut page_resources: Vec<(String, Vec<u8>)> = Vec::new();
    let mut page_note: Option<String> = None;
    if bridge.debug_session_active() {
        match bridge
            .execute_with_timeout(
                "feedback_capture",
                serde_json::json!({}),
                std::time::Duration::from_secs(15),
            )
            .await
        {
            Ok(resp) => {
                let mhtml_path = resp.result.data.get("mhtml_path").and_then(|v| v.as_str());
                let html_path = resp.result.data.get("html_path").and_then(|v| v.as_str());
                let png_path = resp.result.data.get("png_path").and_then(|v| v.as_str());
                let resources_dir = resp.result.data.get("resources_dir").and_then(|v| v.as_str());
                if let Some(path) = mhtml_path {
                    match tokio::fs::read(path).await {
                        Ok(b) => page_mhtml = Some(b),
                        Err(e) => page_note = Some(format!("读取落盘 MHTML 失败 {path}: {e}")),
                    }
                }
                // 有资源快照时 HTML 与 MHTML 并存：MHTML 供视觉还原，page.html
                // + resources/ 供源码级离线还原（JS 仅存在于后者）
                if let Some(path) = html_path {
                    match tokio::fs::read_to_string(path).await {
                        Ok(s) => page_html = Some(s),
                        Err(e) if page_note.is_none() => {
                            page_note = Some(format!("读取落盘 HTML 失败 {path}: {e}"))
                        }
                        Err(_) => {}
                    }
                } else if page_mhtml.is_none() {
                    if let Some(s) = resp.result.data.get("html_b64").and_then(|v| v.as_str()) {
                        if let Ok(bytes) =
                            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s)
                        {
                            page_html = String::from_utf8(bytes).ok();
                        }
                    }
                }
                if let Some(dir) = resources_dir {
                    // 排序保证 zip 内容确定；单文件读取失败跳过不中断
                    let mut entries: Vec<tokio::fs::DirEntry> = Vec::new();
                    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
                        while let Ok(Some(e)) = rd.next_entry().await {
                            entries.push(e);
                        }
                    }
                    entries.sort_by_key(|e| e.file_name());
                    for e in entries {
                        let name = e.file_name();
                        let Some(name) = name.to_str() else { continue };
                        if let Ok(bytes) = tokio::fs::read(e.path()).await {
                            page_resources.push((name.to_string(), bytes));
                        }
                    }
                }
                if let Some(path) = png_path {
                    match tokio::fs::read(path).await {
                        Ok(b) => page_png = Some(b),
                        Err(e) if page_note.is_none() => {
                            page_note = Some(format!("读取落盘截图失败 {path}: {e}"))
                        }
                        Err(_) => {}
                    }
                } else if let Some(s) = resp.result.data.get("png_b64").and_then(|v| v.as_str()) {
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s)
                    {
                        page_png = Some(bytes);
                    }
                }
                if let Some(note) = resp.result.data.get("resources_note").and_then(|v| v.as_str())
                {
                    page_note = Some(match page_note {
                        Some(p) => format!("{p}\n{note}"),
                        None => note.to_string(),
                    });
                }
                let cleanup_path = mhtml_path.or(html_path).or(png_path).or(resources_dir);
                if let Some(p) = cleanup_path {
                    if let Some(dir) = std::path::Path::new(p).parent() {
                        let _ = tokio::fs::remove_dir_all(dir).await;
                    }
                }
                if page_html.is_none()
                    && page_png.is_none()
                    && page_mhtml.is_none()
                    && page_resources.is_empty()
                    && page_note.is_none()
                {
                    page_note = Some(format!("feedback_capture 返回空: {}", resp.result.data));
                }
            }
            Err(e) => page_note = Some(format!("feedback_capture 失败: {e}")),
        }
    } else {
        page_note = Some("无活跃调试会话，页面捕获跳过（先启动调试再导出可含页面）".into());
    }

    // 5) 打 zip（内存）
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        // logs
        if let Some(tail) = log_tail {
            let t = if tail.len() > 1024 * 1024 {
                tail[tail.len() - 1024 * 1024..].to_string()
            } else {
                tail
            };
            zw.start_file("logs/app-tail.log", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(t.as_bytes())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        } else {
            zw.start_file("logs/app-tail.log", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all("(无日志)".as_bytes())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        // task
        if let Some(s) = task_json {
            zw.start_file(&task_filename, opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(s.as_bytes())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        // meta
        zw.start_file("meta.json", opts)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        zw.write_all(meta_str.as_bytes())
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        // page：MHTML（视觉离线还原，含样式与图片）；page.html（引用已改写为
        // resources/ 本地路径，与 CSS/JS 资源快照配合供源码级离线还原）
        if let Some(mhtml) = page_mhtml {
            zw.start_file("debug/page.mhtml", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(&mhtml)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        if let Some(html) = page_html {
            zw.start_file("debug/page.html", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(html.as_bytes())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        for (name, bytes) in &page_resources {
            zw.start_file(format!("debug/resources/{name}"), opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(bytes)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        if let Some(png) = page_png {
            zw.start_file("debug/screenshot.png", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(&png)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        if let Some(note) = page_note {
            // 无会话或捕获失败时留说明，避免解压后疑惑缺文件
            zw.start_file("debug/README.txt", opts)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zw.write_all(note.as_bytes())
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        zw.finish().map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let bytes = buf.into_inner();
    let filename = format!("campus-auth-feedback-{stamp}.zip");
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/zip".to_string(),
            ),
            (axum::http::header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    ))
}
