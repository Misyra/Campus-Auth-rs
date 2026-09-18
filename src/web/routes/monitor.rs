//! 监控路由：系统状态快照、网络测试
//!
//! M1 细粒度 state（engine 域）：handler 声明 `State<Arc<dyn EngineApi>>` 依赖
//! （经 AppState 的 FromRef 委派提取），不再触达 `state.container`，
//! 测试可注入内存实现（见模块测试）。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::BridgeApi;
use crate::config::{ConfigApi, DEFAULT_TRIGGER_URL};
use crate::engine::{EngineApi, EngineCommand};
use crate::environment::EnvironmentApi;
use crate::monitor::{MonitorConfig, PortalDetectStatus, detect_portal};
use crate::status::StatusManager;
use crate::web::error::{ApiError, data};

/// 可见浏览器重定向检测的总超时（含 Worker 启动、浏览器冷启动与 5 秒页面观察）。
const REDIRECT_TEST_TIMEOUT: Duration = Duration::from_secs(45);

/// `POST /api/monitor/test-redirect` 请求体。
#[derive(Debug, Default, Deserialize)]
pub struct RedirectTestRequest {
    /// 用户尚未保存的自定义触发地址；空值使用内置默认地址。
    #[serde(default)]
    pub trigger_url: String,
}

/// 重定向检测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectTestStatus {
    /// 已识别到校园网认证页面。
    Detected,
    /// 公网探测已确认当前正常联网。
    Online,
    /// 浏览器未识别到认证页面或导航失败。
    NotDetected,
}

/// 重定向检测响应；不包含最终 URL，避免门户临时 token 被前端或日志持久化。
#[derive(Debug, Serialize)]
pub struct RedirectTestResult {
    /// 机器可读结论。
    pub status: RedirectTestStatus,
    /// 前端直接展示的用户提示。
    pub message: &'static str,
}

fn redirect_test_result(status: RedirectTestStatus) -> RedirectTestResult {
    let message = match status {
        RedirectTestStatus::Detected => "已检测到校园网认证页面，认证地址无需填写",
        RedirectTestStatus::Online => "当前已正常联网，请先退出校园网登录后再进行重定向检测",
        RedirectTestStatus::NotDetected => {
            "未检测到认证页面，无法跟随重定向，请重试或手动填入认证地址"
        }
    };
    RedirectTestResult { status, message }
}

fn validate_redirect_test_url(raw: &str) -> Result<String, ApiError> {
    let value = raw.trim();
    let value = if value.is_empty() {
        DEFAULT_TRIGGER_URL
    } else {
        value
    };
    if value.len() > 2048 {
        return Err(ApiError::BadRequest("重定向触发地址过长".into()));
    }
    let parsed = url::Url::parse(value)
        .map_err(|_| ApiError::BadRequest("重定向触发地址格式无效".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ApiError::BadRequest(
            "重定向触发地址必须是包含主机名的 http/https URL".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::BadRequest(
            "重定向触发地址不能包含用户名或密码".into(),
        ));
    }
    Ok(value.to_string())
}

/// GET /api/monitor/status — 获取当前系统状态快照
pub async fn get_status(State(status): State<Arc<StatusManager>>) -> Result<Json<Value>, ApiError> {
    let snapshot = status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
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
    tracing::info!("网络检测已启动");
    Ok(data(Value::String("检测已启动".into())))
}

/// POST /api/monitor/stop — 停止网络监测
pub async fn stop_monitor(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    engine.try_dispatch(EngineCommand::Stop)?;
    tracing::info!("网络检测已停止");
    Ok(data(Value::String("检测已停止".into())))
}

/// POST /api/monitor/test-redirect — 用可见浏览器验证校园网重定向。
///
/// 先用既有 HTTP 探测确认“已正常联网”，命中时直接提示先退出登录；其余情况
/// 强制 `headless=false` 启动独立临时浏览器。Worker 只返回页面分类，不返回或保存
/// 最终 URL / 页面正文，避免门户一次性 token 进入配置、日志或前端状态。
pub async fn test_redirect(
    State(config): State<Arc<dyn ConfigApi>>,
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    Json(body): Json<RedirectTestRequest>,
) -> Result<Json<Value>, ApiError> {
    let rt = config.runtime_snapshot();
    let m = &rt.monitor;
    let cfg = MonitorConfig::from_runtime(&rt);
    let preflight = detect_portal(
        &m.http_targets,
        cfg.http_timeout,
        &m.url_targets,
        &m.url_expected_responses,
        cfg.url_timeout,
        m.disable_proxy,
    )
    .await;
    if preflight.status == PortalDetectStatus::Online {
        let result = redirect_test_result(RedirectTestStatus::Online);
        tracing::info!(status = ?result.status, "重定向检测完成");
        return Ok(data(serde_json::to_value(result)?));
    }

    let trigger_url = validate_redirect_test_url(&body.trigger_url)?;
    environment
        .ensure_capability()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("浏览器环境未就绪: {e}")))?;

    let mut browser_settings = serde_json::to_value(&rt.browser)?;
    let settings = browser_settings
        .as_object_mut()
        .ok_or_else(|| ApiError::Internal("浏览器设置序列化结果无效".into()))?;
    // 本功能的目的就是让用户亲眼确认跳转，不能继承全局无头模式。
    settings.insert("headless".into(), Value::Bool(false));
    let response = bridge
        .execute_with_timeout(
            "test_redirect",
            serde_json::json!({
                "trigger_url": trigger_url,
                "browser_settings": browser_settings,
            }),
            REDIRECT_TEST_TIMEOUT,
        )
        .await?;
    if !response.result.success {
        return Err(ApiError::ServiceUnavailable(
            response
                .result
                .error
                .unwrap_or_else(|| "浏览器重定向检测失败".into()),
        ));
    }
    let status = match response.result.data.get("status").and_then(Value::as_str) {
        Some("detected") => RedirectTestStatus::Detected,
        Some("online") => RedirectTestStatus::Online,
        Some("not_detected") => RedirectTestStatus::NotDetected,
        _ => {
            return Err(ApiError::Internal("浏览器重定向检测返回了未知结果".into()));
        }
    };
    let result = redirect_test_result(status);
    tracing::info!(status = ?result.status, "重定向检测完成");
    Ok(data(serde_json::to_value(result)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use tower::ServiceExt; // oneshot

    use crate::engine::{EngineError, ProbeDetails, TestNetworkResult};
    use crate::monitor::{
        AssessmentConfidence, AssessmentReason, AuthEndpointState, LocalLinkState,
    };
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
            confidence: AssessmentConfidence::High,
            reason: AssessmentReason::InternetVerified,
            local_link: LocalLinkState::Available,
            auth_endpoint: AuthEndpointState::NotChecked,
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
        assert_eq!(v["data"], "检测已启动");
        let (status, v) = post_empty(app, "/api/monitor/stop").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["data"], "检测已停止");
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
        let app = mock_app(
            MockEngineApi::new().with_result(Err(EngineError::ProbeError("探测超时".into()))),
        );
        let (status, v) = post_empty(app, "/api/monitor/test").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("探测")
        );
    }

    /// test_network 引擎已关闭：ChannelClosed → 503（「引擎暂不可用」，非服务端故障）
    #[tokio::test]
    async fn test_test_network_engine_closed() {
        let app = mock_app(MockEngineApi::new());
        let (status, v) = post_empty(app, "/api/monitor/test").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("引擎已关闭")
        );
    }

    #[test]
    fn 重定向检测空地址使用内置默认值() {
        assert_eq!(
            validate_redirect_test_url("  ").unwrap(),
            DEFAULT_TRIGGER_URL
        );
    }

    #[test]
    fn 重定向检测拒绝非http与内嵌凭据() {
        assert!(validate_redirect_test_url("file:///tmp/page.html").is_err());
        assert!(validate_redirect_test_url("http://user:secret@example.com/").is_err());
    }

    #[test]
    fn 重定向检测文案覆盖三种结论() {
        let detected = redirect_test_result(RedirectTestStatus::Detected);
        assert!(detected.message.contains("无需填写"));
        let online = redirect_test_result(RedirectTestStatus::Online);
        assert!(online.message.contains("退出校园网登录"));
        let missing = redirect_test_result(RedirectTestStatus::NotDetected);
        assert!(missing.message.contains("手动填入认证地址"));
    }
}
