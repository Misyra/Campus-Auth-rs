//! OCR 路由：OCR 识别（通过 Bridge 执行）
//!
//! M1 细粒度 state：Bridge 经 `State<Arc<dyn BridgeApi>>`、环境能力经
//! `State<Arc<dyn EnvironmentApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::bridge::BridgeApi;
use crate::config::ConfigApi;
use crate::environment::EnvironmentApi;
use crate::web::error::{data, ApiError};

/// POST /api/ocr/recognize — 执行 OCR 识别
pub async fn ocr_recognize(
    State(bridge): State<Arc<dyn BridgeApi>>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 注入 cancel_id="ocr"，使 /api/ocr/uninstall 的 bridge.cancel("ocr") 能命中
    // execute_inner 注册的取消令牌（execute_inner 从 params["cancel_id"] 读取并注册）
    if let Some(obj) = body.as_object_mut() {
        obj.insert("cancel_id".into(), Value::String("ocr".into()));
    }
    let result = bridge.execute("ocr_recognize", body).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/ocr/status — 获取 OCR 状态
///
/// 返回 `installed`（OCR 运行环境是否就绪）与 `size_mb`（环境目录估算体积）。
pub async fn ocr_status(
    State(environment): State<Arc<dyn EnvironmentApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let env = environment.status();
    let installed = env.python_ready && env.playwright_ready;
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

/// POST /api/ocr/uninstall — 卸载 OCR（取消在途任务并释放资源）
pub async fn ocr_uninstall(
    State(bridge): State<Arc<dyn BridgeApi>>,
) -> Result<Json<Value>, ApiError> {
    bridge.cancel("ocr");
    Ok(data(Value::String("ok".into())))
}

/// POST /api/ocr/install — 触发 OCR 环境安装
///
/// 后台执行环境能力安装（uv/Python/Playwright），进度通过 StatusManager 推送。
pub async fn ocr_install(
    State(environment): State<Arc<dyn EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    let env = environment.clone();
    tokio::spawn(async move {
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("OCR 环境安装失败: {e}");
        }
    });
    Ok(data(serde_json::json!({
        "message": "OCR 环境安装已启动",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use tower::ServiceExt; // oneshot

    use crate::bridge::{BridgeError, IpcResponse, IpcResult};

    #[derive(Default)]
    struct MockInner {
        executed: Vec<(String, Value)>,
        cancelled: Vec<String>,
    }

    /// 内存 BridgeApi：记录 execute 的 method/params 与 cancel 的 cancel_id
    struct MockBridgeApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl BridgeApi for MockBridgeApi {
        async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
            self.0
                .lock()
                .unwrap()
                .executed
                .push((method.to_string(), params));
            Ok(IpcResponse {
                id: 1,
                result: IpcResult {
                    success: true,
                    data: Value::Null,
                    error: None,
                },
            })
        }

        fn cancel(&self, cancel_id: &str) {
            self.0.lock().unwrap().cancelled.push(cancel_id.to_string());
        }

        async fn shutdown(&self) {}
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner::default()));
        let bridge: Arc<dyn BridgeApi> = Arc::new(MockBridgeApi(inner.clone()));
        let app = axum::Router::new()
            .route("/api/ocr/recognize", post(ocr_recognize))
            .route("/api/ocr/uninstall", post(ocr_uninstall))
            .with_state(bridge);
        (app, inner)
    }

    /// recognize 注入 cancel_id="ocr" 后派发 ocr_recognize 命令
    #[tokio::test]
    async fn test_ocr_recognize_injects_cancel_id() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ocr/recognize")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"image": "x.png"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let calls = inner.lock().unwrap().executed.clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "ocr_recognize");
        assert_eq!(calls[0].1["cancel_id"], "ocr");
        assert_eq!(calls[0].1["image"], "x.png");
    }

    /// uninstall 派发 cancel("ocr")
    #[tokio::test]
    async fn test_ocr_uninstall_cancels_ocr_slot() {
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
        assert_eq!(inner.lock().unwrap().cancelled, vec!["ocr"]);
    }
}
