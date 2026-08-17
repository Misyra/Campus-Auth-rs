//! 监控路由：系统状态快照、网络测试
//!
//! M1 细粒度 state（engine 域）：handler 声明 `State<Arc<dyn EngineApi>>` 依赖
//! （经 AppState 的 FromRef 委派提取），不再触达 `state.container`，
//! 测试可注入内存实现（见模块测试）。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::engine::{EngineApi, EngineCommand};
use crate::status::StatusManager;
use crate::web::error::{data, ApiError};

/// GET /api/monitor/status — 获取当前系统状态快照
pub async fn get_status(
    State(status): State<Arc<StatusManager>>,
) -> Result<Json<Value>, ApiError> {
    let snapshot = status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
}

/// GET /api/tools/network-interfaces — 列出真实网络接口
///
/// 调用平台对应的 NetworkDetect 实现返回有效网卡列表。
pub async fn list_network_interfaces() -> Result<Json<Value>, ApiError> {
    let detector = crate::network::detect::create_detector();
    let interfaces = detector
        .list_interfaces()
        .await
        .map_err(|e| ApiError::Internal(format!("网络接口检测失败: {e}")))?;
    Ok(data(interfaces))
}

/// POST /api/monitor/test — 网络连通性测试
///
/// 经 EngineApi 派发到「当前活跃」Engine（崩溃重启后自动指向新实例），
/// oneshot 回复与 30s 超时封装在实现内。
pub async fn test_network(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    let result = engine.test_network().await?;
    Ok(data(serde_json::to_value(&result)?))
}

/// POST /api/monitor/start — 启动网络监测
pub async fn start_monitor(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    engine.try_dispatch(EngineCommand::Start)?;
    Ok(data(Value::String("监测已启动".into())))
}

/// POST /api/monitor/stop — 停止网络监测
pub async fn stop_monitor(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    engine.try_dispatch(EngineCommand::Stop)?;
    Ok(data(Value::String("监测已停止".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use tower::ServiceExt; // oneshot

    use crate::engine::{EngineError, ProbeDetails, TestNetworkResult};
    use crate::status::NetworkStatus;

    /// 内存 EngineApi：记录命令名，test_network 返回可配置结果
    /// （TestNetworkResult/EngineError 未派生 Clone，经 Mutex<Option<_>> take 取用）
    struct MockEngineApi {
        commands: std::sync::Mutex<Vec<&'static str>>,
        test_result: std::sync::Mutex<Option<Result<TestNetworkResult, EngineError>>>,
    }

    impl MockEngineApi {
        fn new() -> Self {
            Self {
                commands: Default::default(),
                test_result: Default::default(),
            }
        }

        fn with_result(self, r: Result<TestNetworkResult, EngineError>) -> Self {
            *self.test_result.lock().unwrap() = Some(r);
            self
        }
    }

    #[async_trait::async_trait]
    impl EngineApi for MockEngineApi {
        fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
            let name = match cmd {
                EngineCommand::Start => "Start",
                EngineCommand::Stop => "Stop",
                EngineCommand::Shutdown => "Shutdown",
                EngineCommand::Reload => "Reload",
                EngineCommand::Pause => "Pause",
                EngineCommand::Resume => "Resume",
                EngineCommand::ApplyProfile { .. } => "ApplyProfile",
                EngineCommand::TestNetwork { .. } => "TestNetwork",
            };
            self.commands.lock().unwrap().push(name);
            Ok(())
        }

        async fn test_network(&self) -> Result<TestNetworkResult, EngineError> {
            // 未配置或已取尽：默认引擎已关闭
            self.test_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(EngineError::ChannelClosed))
        }
    }

    fn sample_result() -> TestNetworkResult {
        TestNetworkResult {
            status: NetworkStatus::Online,
            details: ProbeDetails {
                tcp: vec!["Pass".into()],
                http: vec![],
                url: vec![],
            },
            duration_ms: 88,
        }
    }

    fn mock_app(mock: MockEngineApi) -> axum::Router {
        let api: Arc<dyn EngineApi> = Arc::new(mock);
        axum::Router::new()
            .route("/api/monitor/test", post(test_network))
            .route("/api/monitor/start", post(start_monitor))
            .route("/api/monitor/stop", post(stop_monitor))
            .with_state(api)
    }

    async fn post_empty(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// start/stop 派发对应命令并返回提示文案
    #[tokio::test]
    async fn test_start_stop_dispatch_commands() {
        let mock = Arc::new(MockEngineApi::new());
        let api: Arc<dyn EngineApi> = mock.clone();
        let app = axum::Router::new()
            .route("/api/monitor/start", post(start_monitor))
            .route("/api/monitor/stop", post(stop_monitor))
            .with_state(api);
        let (status, v) = post_empty(app.clone(), "/api/monitor/start").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["data"], "监测已启动");
        let (status, v) = post_empty(app, "/api/monitor/stop").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["data"], "监测已停止");
        assert_eq!(*mock.commands.lock().unwrap(), vec!["Start", "Stop"]);
    }

    /// test_network 成功路径：透传探测结果 JSON
    #[tokio::test]
    async fn test_test_network_success() {
        let app = mock_app(MockEngineApi::new().with_result(Ok(sample_result())));
        let (status, v) = post_empty(app, "/api/monitor/test").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["data"]["duration_ms"], 88);
        assert_eq!(v["data"]["status"], "online");
    }

    /// test_network 探测失败：EngineError → 500（非通道类错误不吞）
    #[tokio::test]
    async fn test_test_network_probe_error_maps_internal() {
        let app = mock_app(MockEngineApi::new().with_result(Err(EngineError::ProbeError(
            "探测超时".into(),
        ))));
        let (status, v) = post_empty(app, "/api/monitor/test").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("探测"));
    }

    /// test_network 引擎已关闭：ChannelClosed → 500
    #[tokio::test]
    async fn test_test_network_engine_closed() {
        let app = mock_app(MockEngineApi::new());
        let (status, v) = post_empty(app, "/api/monitor/test").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("引擎已关闭"));
    }
}
