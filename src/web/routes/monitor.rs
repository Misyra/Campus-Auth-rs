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
use utoipa::ToSchema;

use crate::bridge::BridgeApi;
use crate::config::{ConfigApi, DEFAULT_TRIGGER_URL};
use crate::engine::{EngineApi, EngineCommand};
use crate::environment::EnvironmentApi;
use crate::monitor::{MonitorConfig, PortalDetectStatus};
use crate::status::StatusManager;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

/// 可见浏览器重定向检测的总超时（含 Worker 启动、浏览器冷启动与 5 秒页面观察）。
const REDIRECT_TEST_TIMEOUT: Duration = Duration::from_secs(45);

/// `POST /api/monitor/test-redirect` 请求体。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct RedirectTestRequest {
    /// 用户尚未保存的自定义触发地址；空值使用内置默认地址。
    #[serde(default)]
    pub trigger_url: String,
    /// 方案编辑器尚未保存的出口配置（`NetworkSettings`，形状见
    /// `crate::network::settings`，契约快照独立于 ToSchema 体系故以泛型对象标注）。
    #[schema(value_type = Object)]
    pub network: Option<crate::network::settings::NetworkSettings>,
}

// ---- 契约 DTO（T5-1c monitor 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。状态快照/探测结果是既有
// Serialize 类型的线格式镜像（handler 仍走 `data(serde_json::to_value(..))`），
// 不改 handler 返回类型；字符串枚举字段的取值在各字段 doc 中逐项列明。

/// GET /api/monitor/status 响应载荷（`StatusSnapshot` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct MonitorStatusData {
    /// 快照单调版本号（每次 merge +1，前端据此判断状态新旧）
    pub snapshot_version: u64,
    /// 引擎运行状态：running / stopped / dead
    pub engine_state: String,
    /// 是否处于冷却期（连续失败后的等待）
    pub cooling_down: bool,
    /// 冷却剩余秒数
    pub cooling_down_remaining: Option<u32>,
    /// 连续失败次数
    pub consecutive_failures: u32,
    /// 网络状态：online / captive_portal / offline / unknown
    pub network_status: String,
    /// 最近一次连通性解释
    pub connectivity: MonitorConnectivityData,
    /// 最近一次原始探测证据；尚未执行探测时为 null
    pub last_probe_evidence: Option<ProbeEvidenceData>,
    /// 上次网络检测时间（本地时区 ISO 8601）
    pub last_check_time: Option<String>,
    /// 登录状态：idle / running / success / failed / cancelled
    pub login_status: String,
    /// 登录来源：auto / manual / login_once / browser；无在途登录时为 null
    pub login_source: Option<String>,
    /// 登录结果消息
    pub login_message: Option<String>,
    /// 当前登录重试次数
    pub retry_count: u32,
    /// 当前活跃 Profile ID
    pub active_profile: String,
    /// 监测是否启用
    pub monitor_enabled: bool,
    /// 是否处于暂停时段
    pub pause_active: bool,
    /// 是否有新版本
    pub update_available: bool,
    /// 更新下载进度（下载期间有值，结束/失败后为 null）
    pub update_progress: Option<super::system::EnvProgress>,
    /// Python Worker 状态：not_installed / stopped / starting / ready / busy / error
    pub worker_state: String,
    /// 环境安装进度
    pub environment_progress: Option<super::system::EnvProgress>,
    /// 应用运行时长（秒）
    pub uptime_seconds: u64,
    /// 本次监控连续运行时长（秒）；未监控时为 0
    pub monitoring_seconds: u64,
    /// 调度器是否运行中
    pub scheduler_running: bool,
    /// 调度器下次触发时间（ISO 8601 字符串）
    pub scheduler_next_fire_at: Option<String>,
    /// 调度器管理的任务数量
    pub scheduler_task_count: usize,
    /// 累计网络检测次数
    pub probe_total: u64,
    /// 累计登录尝试次数
    pub login_total: u64,
}

/// 连通性解释（`ConnectivityAssessment` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct MonitorConnectivityData {
    /// 网络连通性状态：online / captive_portal / offline / unknown
    pub status: String,
    /// 判断置信度：high / medium / low
    pub confidence: String,
    /// 主判断原因：not_checked / internet_verified / captive_detected /
    /// external_failed_auth_reachable / link_up_login_assumed / redirect_login_assumed /
    /// all_probes_failed / weak_evidence_only / inconclusive_evidence /
    /// conflicting_evidence / no_probes_enabled
    pub reason: String,
    /// 认证入口补充证据：not_checked / reachable / unreachable / invalid / missing /
    /// skipped_redirect_mode
    pub auth_endpoint: String,
    /// 自动恢复建议：no_action / attempt_login / attempt_login_once / wait_for_network /
    /// wait_for_more_evidence / fix_configuration / no_probe_evidence / not_evaluated
    pub recovery_advice: String,
}

/// 原始探测证据（`ProbeEvidence` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct ProbeEvidenceData {
    /// TCP 弱连通探测：pass / captive / inconclusive / fail / disabled
    pub tcp: String,
    /// HTTP 204 门户探测：同 `tcp` 枚举
    pub http: String,
    /// URL 内容探测：同 `tcp` 枚举
    pub url: String,
    /// 本地链路诊断：not_checked / available / unavailable / probe_failed
    pub local_link: String,
}

/// POST /api/monitor/test 响应载荷（`TestNetworkResult` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct TestNetworkResultData {
    /// 探测结论：online / captive_portal / offline / unknown
    pub status: String,
    /// 判断置信度：high / medium / low
    pub confidence: String,
    /// 主判断原因（取值同连通性解释的 `reason` 枚举）
    pub reason: String,
    /// 本地链路诊断：not_checked / available / unavailable / probe_failed
    pub local_link: String,
    /// 认证入口状态（取值同连通性解释的 `auth_endpoint` 枚举）
    pub auth_endpoint: String,
    /// 各探测方法的详细结果
    pub details: ProbeDetailsData,
    /// 探测耗时（毫秒）
    pub duration_ms: u64,
}

/// 各探测方法的详细结果（`ProbeDetails` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct ProbeDetailsData {
    /// TCP 探测结果描述
    pub tcp: Vec<String>,
    /// HTTP 探测结果描述
    pub http: Vec<String>,
    /// URL 探测结果描述
    pub url: Vec<String>,
}

/// POST /api/monitor/test-redirect 响应载荷（`RedirectTestResult` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct RedirectTestResultData {
    /// 机器可读结论
    pub status: RedirectTestStatus,
    /// 前端直接展示的用户提示
    pub message: String,
}

/// GET /api/network/interfaces 响应条目（`EgressAdapter` 线格式镜像）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct NetworkInterfaceData {
    /// 稳定标识：Windows 使用接口 GUID，Unix 优先使用 MAC
    pub id: String,
    /// 用户可识别的接口名称
    pub name: String,
    /// 当前接口索引
    pub index: u32,
    /// 当前 IPv4 地址
    pub ipv4: String,
    /// 当前接口 MAC，供门户凭据变换使用
    pub mac: String,
    /// 当前接口配置的 IPv4 DNS
    pub dns_servers: Vec<String>,
}

/// 重定向检测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
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
#[utoipa::path(
    get,
    path = "/api/monitor/status",
    operation_id = "get_api_monitor_status",
    tag = "monitor",
    responses(
        (status = 200, description = "全局状态快照（引擎/网络/登录/调度/Worker/环境进度）", body = DataEnvelope<MonitorStatusData>),
        (status = 500, description = "快照序列化失败", body = ErrorResponse),
    )
)]
pub async fn get_status(State(status): State<Arc<StatusManager>>) -> Result<Json<Value>, ApiError> {
    let snapshot = status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
}

/// POST /api/monitor/test — 网络连通性测试
///
/// 经 EngineApi 派发到「当前活跃」Engine（崩溃重启后自动指向新实例），
/// oneshot 回复与 30s 超时封装在实现内。
#[utoipa::path(
    post,
    path = "/api/monitor/test",
    operation_id = "post_api_monitor_test",
    tag = "monitor",
    responses(
        (status = 200, description = "一轮手动网络探测结果（结论 + 置信度 + 分项探测描述）", body = DataEnvelope<TestNetworkResultData>),
        (status = 503, description = "引擎命令通道不可用（队列饱和或 Engine 已退出/崩溃重启窗口，稍后重试）", body = ErrorResponse),
        (status = 500, description = "探测执行失败（ProbeError / 网络测试超时）", body = ErrorResponse),
    )
)]
pub async fn test_network(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    let result = engine.test_network().await?;
    Ok(data(serde_json::to_value(&result)?))
}

/// POST /api/monitor/start — 启动网络监测
#[utoipa::path(
    post,
    path = "/api/monitor/start",
    operation_id = "post_api_monitor_start",
    tag = "monitor",
    responses(
        (status = 200, description = "检测已启动", body = DataEnvelope<String>),
        (status = 503, description = "引擎命令通道不可用（队列饱和或 Engine 已退出/崩溃重启窗口）", body = ErrorResponse),
        (status = 500, description = "其他引擎内部错误", body = ErrorResponse),
    )
)]
pub async fn start_monitor(
    State(engine): State<Arc<dyn EngineApi>>,
) -> Result<Json<Value>, ApiError> {
    engine.try_dispatch(EngineCommand::Start)?;
    tracing::info!("网络检测已启动");
    Ok(data(Value::String("检测已启动".into())))
}

/// POST /api/monitor/stop — 停止网络监测
#[utoipa::path(
    post,
    path = "/api/monitor/stop",
    operation_id = "post_api_monitor_stop",
    tag = "monitor",
    responses(
        (status = 200, description = "检测已停止", body = DataEnvelope<String>),
        (status = 503, description = "引擎命令通道不可用（队列饱和或 Engine 已退出/崩溃重启窗口）", body = ErrorResponse),
        (status = 500, description = "其他引擎内部错误", body = ErrorResponse),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/monitor/test-redirect",
    operation_id = "post_api_monitor_test_redirect",
    tag = "monitor",
    request_body = RedirectTestRequest,
    responses(
        (status = 200, description = "重定向检测结论（不含最终 URL，防门户临时 token 外泄）", body = DataEnvelope<RedirectTestResultData>),
        (status = 400, description = "触发地址过长 / 格式无效 / 非 http(s) / 内嵌用户名或密码", body = ErrorResponse),
        (status = 409, description = "Worker 忙（调试会话或浏览器任务占用）", body = ErrorResponse),
        (status = 503, description = "出口网络准备失败、Python 环境未就绪、Worker 不可用（未安装/启动超时/网络出口受限）或浏览器检测失败", body = ErrorResponse),
        (status = 500, description = "浏览器返回未知结果或内部错误", body = ErrorResponse),
    )
)]
pub async fn test_redirect(
    State(config): State<Arc<dyn ConfigApi>>,
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    Json(body): Json<RedirectTestRequest>,
) -> Result<Json<Value>, ApiError> {
    let rt = config.runtime_snapshot();
    let m = &rt.monitor;
    let cfg = MonitorConfig::from_runtime(&rt);
    let egress = crate::network::egress::PreparedEgress::prepare(
        body.network.as_ref().unwrap_or(&rt.network),
    )
    .await
    .map_err(|e| ApiError::ServiceUnavailable(e.to_string()))?;
    let preflight = crate::monitor::portal::detect_portal_via(
        &m.http_targets,
        cfg.http_timeout,
        &m.url_targets,
        &m.url_expected_responses,
        cfg.url_timeout,
        m.disable_proxy,
        Some(egress.clone()),
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
    egress.apply_browser(&mut browser_settings);
    let response = bridge
        .execute_with_timeout(
            "test_redirect",
            serde_json::json!({
                "trigger_url": trigger_url,
                "browser_settings": browser_settings,
                "network": body.network.as_ref().unwrap_or(&rt.network),
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

/// GET /api/network/interfaces：物理网卡及当前 IPv4 / DNS，供方案选择出口。
#[utoipa::path(
    get,
    path = "/api/network/interfaces",
    operation_id = "get_api_network_interfaces",
    tag = "monitor",
    responses(
        (status = 200, description = "可用于校园网出口的 IPv4 物理网卡清单", body = DataEnvelope<Vec<NetworkInterfaceData>>),
        (status = 503, description = "网卡枚举失败或超时", body = ErrorResponse),
    )
)]
pub async fn get_network_interfaces() -> Result<Json<Value>, ApiError> {
    let adapters = crate::network::adapters::list_adapters()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("网卡读取失败: {e}")))?;
    Ok(data(serde_json::to_value(adapters)?))
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
