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
    tracing::info!("调试会话已启动");
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
    tracing::info!("调试会话已停止");
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
    State(config): State<Arc<dyn ConfigApi>>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use crate::utils::paths::worker_project_dir;
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(ApiError::BadRequest("非法文件名".into()));
    }
    let dir = worker_project_dir(&config.base_path()).join("debug");
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
    // Worker 返回路径必须位于当前 debug 会话目录内（防 IPC 信任边界逃逸）
    let allowed_debug_dir =
        crate::utils::paths::worker_project_dir(&config.base_path()).join("debug");
    let path_allowed = |p: &str| -> bool {
        let path = std::path::Path::new(p);
        let (Ok(canon), Ok(base)) = (path.canonicalize(), allowed_debug_dir.canonicalize()) else {
            // 文件尚不存在时退化为词法前缀检查（父目录必须在 debug 内）
            return path
                .parent()
                .map(|parent| parent.starts_with(&allowed_debug_dir))
                .unwrap_or(false);
        };
        canon.starts_with(&base)
    };
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
                let resources_dir = resp
                    .result
                    .data
                    .get("resources_dir")
                    .and_then(|v| v.as_str());
                if let Some(path) = mhtml_path {
                    if !path_allowed(path) {
                        page_note = Some("拒绝读取 debug 目录外的 MHTML 路径".to_string());
                    } else {
                        match tokio::fs::read(path).await {
                            Ok(b) => page_mhtml = Some(b),
                            Err(e) => page_note = Some(format!("读取落盘 MHTML 失败 {path}: {e}")),
                        }
                    }
                }
                // 有资源快照时 HTML 与 MHTML 并存：MHTML 供视觉还原，page.html
                // + resources/ 供源码级离线还原（JS 仅存在于后者）
                if let Some(path) = html_path {
                    if !path_allowed(path) {
                        if page_note.is_none() {
                            page_note = Some("拒绝读取 debug 目录外的 HTML 路径".to_string());
                        }
                    } else {
                        match tokio::fs::read_to_string(path).await {
                            Ok(s) => page_html = Some(s),
                            Err(e) if page_note.is_none() => {
                                page_note = Some(format!("读取落盘 HTML 失败 {path}: {e}"))
                            }
                            Err(_) => {}
                        }
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
                // 反馈资源总量上限 50MiB（防 200×5MiB≈1GiB 放大 + 内存 zip）
                const MAX_FEEDBACK_RESOURCES_BYTES: usize = 50 * 1024 * 1024;
                let mut resources_bytes: usize = 0;
                if let Some(dir) = resources_dir {
                    if !path_allowed(dir) {
                        if page_note.is_none() {
                            page_note = Some("拒绝读取 debug 目录外的资源目录".to_string());
                        }
                    } else {
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
                                resources_bytes += bytes.len();
                                if resources_bytes > MAX_FEEDBACK_RESOURCES_BYTES {
                                    page_note = Some("反馈资源超 50MiB 上限，已截断".to_string());
                                    break;
                                }
                                page_resources.push((name.to_string(), bytes));
                            }
                        }
                    }
                }
                if let Some(path) = png_path {
                    if !path_allowed(path) {
                        if page_note.is_none() {
                            page_note = Some("拒绝读取 debug 目录外的截图路径".to_string());
                        }
                    } else {
                        match tokio::fs::read(path).await {
                            Ok(b) => page_png = Some(b),
                            Err(e) if page_note.is_none() => {
                                page_note = Some(format!("读取落盘截图失败 {path}: {e}"))
                            }
                            Err(_) => {}
                        }
                    }
                } else if let Some(s) = resp.result.data.get("png_b64").and_then(|v| v.as_str()) {
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s)
                    {
                        page_png = Some(bytes);
                    }
                }
                if let Some(note) = resp
                    .result
                    .data
                    .get("resources_note")
                    .and_then(|v| v.as_str())
                {
                    page_note = Some(match page_note {
                        Some(p) => format!("{p}\n{note}"),
                        None => note.to_string(),
                    });
                }
                let cleanup_path = mhtml_path.or(html_path).or(png_path).or(resources_dir);
                if let Some(p) = cleanup_path {
                    // 仅 debug 目录内才清理，防任意目录删除
                    if path_allowed(p) {
                        if let Some(dir) = std::path::Path::new(p).parent() {
                            // 二次确认父目录仍在允许区内
                            if let (Ok(canon), Ok(base)) =
                                (dir.canonicalize(), allowed_debug_dir.canonicalize())
                            {
                                if canon.starts_with(&base) {
                                    let _ = tokio::fs::remove_dir_all(dir).await;
                                }
                            } else if dir.starts_with(&allowed_debug_dir) {
                                let _ = tokio::fs::remove_dir_all(dir).await;
                            }
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt; // oneshot

    use crate::bridge::{BridgeError, IpcResponse, IpcResult};
    use crate::environment::{BootstrapStage, EnvironmentApi, EnvironmentError, EnvironmentStatus};
    use crate::tasks::{OrderData, TaskApi, TaskDetail, TaskError, TaskKind, TaskSummary};

    use super::super::test_support::{MockConfigApi, body_json};

    struct MockInner {
        executed: Vec<(String, Value)>,
        respond_data: Value,
        session_active: bool,
        screenshot_url: Option<String>,
        ensure_fails: bool,
        embedded: bool,
    }

    impl Default for MockInner {
        fn default() -> Self {
            Self {
                executed: Vec::new(),
                respond_data: serde_json::json!({"ok": true}),
                session_active: false,
                screenshot_url: None,
                ensure_fails: false,
                embedded: false,
            }
        }
    }

    struct MockBridgeApi(Arc<std::sync::Mutex<MockInner>>);

    fn ipc_ok(data: Value) -> IpcResponse {
        IpcResponse {
            id: 1,
            result: IpcResult {
                success: true,
                data,
                error: None,
            },
        }
    }

    #[async_trait::async_trait]
    impl BridgeApi for MockBridgeApi {
        async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
            let mut inner = self.0.lock().unwrap();
            inner.executed.push((method.to_string(), params));
            Ok(ipc_ok(inner.respond_data.clone()))
        }

        fn cancel(&self, _cancel_id: &str) {}

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

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}

        fn debug_session_active(&self) -> bool {
            self.0.lock().unwrap().session_active
        }

        fn last_screenshot_url(&self) -> Option<String> {
            self.0.lock().unwrap().screenshot_url.clone()
        }
    }

    struct MockTaskApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<TaskSummary> {
            Vec::new()
        }

        async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
            Err(TaskError::TaskNotFound(task_id.to_string()))
        }

        async fn embed_task_config(&self, _task_id: &str, params: &mut Value) -> bool {
            let mut inner = self.0.lock().unwrap();
            inner.embedded = true;
            if let Some(obj) = params.as_object_mut() {
                obj.insert("embedded".to_string(), Value::Bool(true));
            }
            true
        }

        async fn save_task(&self, _task_id: &str, _task: &TaskKind) -> Result<(), TaskError> {
            Ok(())
        }

        async fn delete_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_active_task(&self) -> String {
            String::new()
        }

        async fn set_active_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
            Err(TaskError::TaskNotFound(task_id.to_string()))
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

        fn has_task(&self, _task_id: &str) -> bool {
            false
        }
    }

    struct MockEnvironmentApi(Arc<std::sync::Mutex<MockInner>>);

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
            std::path::PathBuf::new()
        }
        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            if self.0.lock().unwrap().ensure_fails {
                return Err(EnvironmentError::BootstrapFailedShared(
                    "mock 环境缺失".to_string(),
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

    #[derive(Clone)]
    struct TestState {
        tasks: Arc<dyn TaskApi>,
        config: Arc<dyn ConfigApi>,
        bridge: Arc<dyn BridgeApi>,
        env: Arc<dyn crate::environment::EnvironmentApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &TestState) -> Self {
            state.tasks.clone()
        }
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

    impl axum::extract::FromRef<TestState> for Arc<dyn crate::environment::EnvironmentApi> {
        fn from_ref(state: &TestState) -> Self {
            state.env.clone()
        }
    }

    fn mock_app() -> (
        axum::Router,
        Arc<std::sync::Mutex<MockInner>>,
        Arc<std::sync::Mutex<super::super::test_support::MockConfigInner>>,
    ) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner::default()));
        let (config, cfg_inner) = MockConfigApi::mocked();
        let state = TestState {
            tasks: Arc::new(MockTaskApi(inner.clone())),
            config,
            bridge: Arc::new(MockBridgeApi(inner.clone())),
            env: Arc::new(MockEnvironmentApi(inner.clone())),
        };
        let app = axum::Router::new()
            .route("/api/debug/start", post(start_debug))
            .route("/api/debug/step", post(step_debug))
            .route("/api/debug/stop", post(stop_debug))
            .route("/api/debug/status", get(debug_status))
            .route("/api/debug/run-all", post(run_all))
            .route("/api/debug/screenshot/{filename}", get(debug_screenshot))
            .route("/api/debug/feedback-bundle", post(feedback_bundle))
            .with_state(state);
        (app, inner, cfg_inner)
    }

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// 环境未就绪 → 503，不触达 Bridge
    #[tokio::test]
    async fn start_requires_ready_environment() {
        let (app, inner, _) = mock_app();
        inner.lock().unwrap().ensure_fails = true;
        let resp = app
            .oneshot(post_json("/api/debug/start", r#"{"task_id":"t1"}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(inner.lock().unwrap().executed.is_empty());
    }

    /// 启动注入 Profile 变量 + 浏览器设置，并嵌入任务配置后下发 debug_start
    #[tokio::test]
    async fn start_injects_profile_and_embeds_task() {
        let (app, inner, cfg) = mock_app();
        {
            let mut g = cfg.lock().unwrap();
            g.runtime.profile.username = "dbguser".into();
            g.runtime.profile.auth_url = "http://127.0.0.1:18765/".into();
        }
        let resp = app
            .oneshot(post_json("/api/debug/start", r#"{"task_id":"t1"}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let guard = inner.lock().unwrap();
        assert_eq!(guard.executed.len(), 1);
        let (method, params) = &guard.executed[0];
        assert_eq!(method, "debug_start");
        assert_eq!(params["username"], "dbguser");
        assert_eq!(params["auth_url"], "http://127.0.0.1:18765/");
        assert!(params.get("browser_settings").is_some());
        assert_eq!(params["embedded"], true);
        assert!(guard.embedded);
    }

    /// 非对象体 → 400（环境检查之后、Bridge 之前）
    #[tokio::test]
    async fn start_rejects_non_object_body() {
        let (app, inner, _) = mock_app();
        let resp = app
            .oneshot(post_json("/api/debug/start", r#"[1,2]"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(inner.lock().unwrap().executed.is_empty());
    }

    /// step 空体转 {} 并透传 debug_step
    #[tokio::test]
    async fn step_passes_empty_object_on_null() {
        let (app, inner, _) = mock_app();
        let resp = app
            .oneshot(post_json("/api/debug/step", r#"null"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let guard = inner.lock().unwrap();
        assert_eq!(guard.executed[0].0, "debug_step");
        assert_eq!(guard.executed[0].1, serde_json::json!({}));
    }

    /// stop/run-all 透传对应命令
    #[tokio::test]
    async fn stop_and_run_all_forward_commands() {
        let (app, inner, _) = mock_app();
        for (uri, method) in [
            ("/api/debug/stop", "debug_stop"),
            ("/api/debug/run-all", "debug_run_all"),
        ] {
            let resp = app
                .clone()
                .oneshot(post_json(uri, r#"null"#))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "uri={uri}");
            assert_eq!(inner.lock().unwrap().executed.last().unwrap().0, method);
        }
    }

    /// 无会话 → active:false（不查询 Worker）
    #[tokio::test]
    async fn status_inactive_skips_worker() {
        let (app, inner, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/debug/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["active"], false);
        assert!(inner.lock().unwrap().executed.is_empty());
    }

    /// 有会话 → 查 Worker 回会话详情并带截图 URL
    #[tokio::test]
    async fn status_active_queries_worker() {
        let (app, inner, _) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.session_active = true;
            g.screenshot_url = Some("/api/debug/screenshot/s.png".into());
            g.respond_data = serde_json::json!({"steps": []});
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/debug/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["active"], true);
        assert_eq!(v["data"]["screenshot_url"], "/api/debug/screenshot/s.png");
        assert_eq!(v["data"]["session"], serde_json::json!({"steps": []}));
        assert_eq!(inner.lock().unwrap().executed[0].0, "debug_status");
    }

    /// 截图路径穿越 → 400（落盘前拒绝）
    #[tokio::test]
    async fn screenshot_rejects_traversal() {
        let (app, _, _) = mock_app();
        for name in ["..%2Fsecret", "..%5Csecret"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/debug/screenshot/{name}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            // `%2F` 进 Path 后为字面 `%2F`（不含 '/')，走文件名不存在分支；
            // 此断言只锁“不 500、不越目录读盘”：允许 400 或 404
            assert!(
                resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND,
                "name={name}: {}",
                resp.status()
            );
        }
    }

    /// 缺失截图 → 404；存在 → 200 image/png
    #[tokio::test]
    async fn screenshot_missing_and_hit() {
        let tmp = tempfile::tempdir().unwrap();
        // worker 工程目录：base_path 下 python_worker/（resolve 优先命中）
        let dbg = tmp.path().join("python_worker").join("debug");
        std::fs::create_dir_all(&dbg).unwrap();
        std::fs::write(dbg.join("s1.png"), b"\x89PNG-hit").unwrap();
        let (app, _, cfg) = mock_app();
        cfg.lock().unwrap().base_path = tmp.path().to_path_buf();
        let uri = "/api/debug/screenshot/missing.png";
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/debug/screenshot/s1.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "image/png");
    }

    /// 无会话反馈包：200 zip，含 meta.json（has_password 真但无明文）与占位说明
    #[tokio::test]
    async fn feedback_bundle_without_session_has_meta_and_note() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, cfg) = mock_app();
        {
            let mut g = cfg.lock().unwrap();
            g.base_path = tmp.path().to_path_buf();
            g.runtime.profile.password = zeroize::Zeroizing::new("s3cr3t-pw".to_string());
        }
        let resp = app
            .oneshot(post_json("/api/debug/feedback-bundle", r#"null"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "application/zip");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"meta.json".to_string()), "{names:?}");
        assert!(names.contains(&"debug/README.txt".to_string()), "{names:?}");
        let mut meta = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("meta.json").unwrap(), &mut meta).unwrap();
        assert!(meta.contains("\"has_password\":true") || meta.contains("\"has_password\": true"));
        assert!(!meta.contains("s3cr3t-pw"), "密码明文不得进包: {meta}");
    }
}
