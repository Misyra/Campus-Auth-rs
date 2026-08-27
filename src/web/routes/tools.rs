//! 工具路由：任务录制脚本等静态资源

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::warn;

use std::sync::Arc;

use crate::config::ConfigApi;
use crate::web::error::ApiError;

/// 在候选目录中查找首个存在的任务录制脚本
fn resolve_script_path(base_path: &std::path::Path) -> std::path::PathBuf {
    let rel = std::path::Path::new("resources")
        .join("tools")
        .join("task-recorder.user.js");
    // 便携版布局：<base_path>/resources/...
    let primary = base_path.join(&rel);
    if primary.exists() {
        return primary;
    }
    // 开发布局：<repo>/resources/...（base_path=target/debug）
    if let Some(repo) = base_path.parent().and_then(|p| p.parent()) {
        let fallback = repo.join(&rel);
        if fallback.exists() {
            return fallback;
        }
    }
    // 编译期仓库根（CARGO_MANIFEST_DIR）兜底，供集成测试等非运行目录
    let manifest_fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&rel);
    if manifest_fallback.exists() {
        return manifest_fallback;
    }
    primary
}

/// GET /api/tools/task-recorder.user.js — 任务录制用户脚本
///
/// 从 `resources/tools/task-recorder.user.js` 读取并返回 Tampermonkey 用户脚本。
/// 文件缺失时返回 404。
pub async fn task_recorder(State(config): State<Arc<dyn ConfigApi>>) -> Result<Response, ApiError> {
    let script_path = resolve_script_path(&config.base_path());

    // tokio::fs 异步读取，避免同步 std::fs 阻塞 tokio worker 线程
    match tokio::fs::read_to_string(&script_path).await {
        Ok(script) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(script))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response()
            })),
        Err(e) => {
            warn!("任务录制器脚本加载失败 ({script_path:?}): {e}");
            Err(ApiError::NotFound(
                "任务录制器脚本文件缺失，可能需要重新安装或更新软件".to_string(),
            ))
        }
    }
}
