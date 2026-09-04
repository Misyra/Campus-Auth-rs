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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use super::super::test_support::MockConfigApi;

    fn mock_app_with_base(base: &std::path::Path) -> axum::Router {
        let (config, inner) = MockConfigApi::mocked();
        inner.lock().unwrap().base_path = base.to_path_buf();
        axum::Router::new()
            .route("/api/tools/task-recorder.user.js", get(task_recorder))
            .with_state(config)
    }

    /// 主路径命中：base_path 下 resources/tools/task-recorder.user.js 原样返回
    #[tokio::test]
    async fn recorder_serves_primary_script() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("resources").join("tools");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("task-recorder.user.js"), "// test-recorder").unwrap();
        let app = mock_app_with_base(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tools/task-recorder.user.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()["content-type"],
            "application/javascript; charset=utf-8"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"// test-recorder");
    }

    /// 兜底：base_path 为空时回退到仓库 resources（开发布局），仍 200
    #[tokio::test]
    async fn recorder_falls_back_to_repo_resources() {
        let tmp = tempfile::tempdir().unwrap();
        // 用一个两级深度的空目录：primary 缺失，开发布局回退也缺失，
        // 最终命中 CARGO_MANIFEST_DIR 兜底（仓库内始终存在该脚本）
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        // 若仓库兜底脚本不存在则跳过（打包布局差异），不误报失败
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("tools")
            .join("task-recorder.user.js");
        if !manifest.exists() {
            eprintln!("跳过：仓库 resources/tools/task-recorder.user.js 不存在");
            return;
        }
        let app = mock_app_with_base(&nested);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tools/task-recorder.user.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
