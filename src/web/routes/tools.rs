//! 工具路由：任务录制脚本等静态资源

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::warn;

use std::sync::Arc;

use crate::config::ConfigApi;
use crate::web::error::ApiError;

/// GET /api/tools/task-recorder.user.js — 任务录制用户脚本
///
/// 从 `resources/tools/task-recorder.user.js` 读取并返回 Tampermonkey 用户脚本。
/// 文件缺失时返回 404。
pub async fn task_recorder(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Response, ApiError> {
    let script_path = config
        .base_path()
        .join("resources")
        .join("tools")
        .join("task-recorder.user.js");

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
