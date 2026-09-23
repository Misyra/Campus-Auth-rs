//! 直连任务路由：发送一次无状态直连测试请求
//!
//! 直连配置整体搬进任务（`tasks/http/<id>.json`）后，测试端点也必须按任务建模：
//! 任务编辑器用未保存草稿（`task`）测，方案编辑器用已保存任务（`task_id`）+ 来源
//! 方案（`profile_id`）测。请求的构造与正式登录**同一个入口**
//! （[`HttpLoginRequest::from_task`]），响应字段也与旧端点逐字一致（前端按此消费），
//! 只有"参数从哪来"变了——否则会出现"测试通过、正式登录失败"这种无从判断该信哪边的组合。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::config::ConfigApi;
use crate::login::http_login::{HttpLoginRequest, run_once as run_http_login_once};
use crate::tasks::{HttpTaskConfig, TaskApi, TaskKind};
use crate::web::error::{ApiError, data};
use crate::web::operations::{RegisterError, WebOperations};

/// POST /api/http-tasks/test 请求体
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct HttpTaskTestBody {
    /// 已保存任务 ID（方案编辑器用：直接测方案绑定的那个任务）
    pub task_id: Option<String>,
    /// 未保存草稿（任务编辑器用）；存在时优先于 `task_id`
    pub task: Option<HttpTaskConfig>,
    /// 凭据与认证地址的来源方案（可省：账号密码手填时不需要）
    pub profile_id: Option<String>,
    /// 登录账号
    pub username: String,
    /// 登录密码；留空时若给了 `profile_id` 则用该方案已保存的密码
    pub password: Zeroizing<String>,
    /// 是否在运行脚本前抓取认证页原文（脚本 `ctx.page` 的来源）
    pub fetch_page: bool,
}

/// POST /api/http-tasks/test — 发送一次无状态直连测试请求
///
/// 解析顺序（每条错误的文案都直接指向用户该去改的地方）：
/// 单飞登记 → 取任务（草稿优先）/ 账号 / 密码 / 认证地址 / 证书策略 →
/// 地址校验 → 构造请求 → 执行。
pub async fn test_http_task(
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
    State(operations): State<Arc<WebOperations>>,
    Json(body): Json<HttpTaskTestBody>,
) -> Result<Json<Value>, ApiError> {
    // 单飞闸门沿用旧端点那套：测试可能执行用户 JS 并发起网关请求，并发堆积会
    // 同时拉起多个 boa 执行线程。登记守卫 Drop 即释放，早退路径不会漏放。
    let registration = operations
        .http_login_test()
        .register("http-tasks-test")
        .map_err(|error| match error {
            RegisterError::Paused => {
                ApiError::ServiceUnavailable("服务正在停止，请稍后重试".into())
            }
            RegisterError::CapacityReached | RegisterError::DuplicateId => {
                ApiError::Conflict("已有直连测试正在进行，请稍候再试".into())
            }
        })?;

    // 2. 取任务：未保存草稿优先（任务编辑器边改边测），否则按 ID 取已保存任务
    let task = match body.task {
        Some(task) => task,
        None => match body
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(id) => match tasks.get_task_detail(id).await {
                Ok(detail) => match detail.config {
                    TaskKind::Http(cfg) => cfg,
                    // 类型不对与"不存在"分开报（400/404）：前者改任务类型即可，
                    // 后者要去任务页重新选。实际类型进日志而不进文案，保持
                    // 错误文案与其它直连入口一致，便于前端统一匹配。
                    other => {
                        tracing::warn!(
                            task_id = id,
                            task_type = other.type_name(),
                            "直连测试指定的任务类型不是 http"
                        );
                        return Err(ApiError::BadRequest(format!("任务 {id} 不是直连任务")));
                    }
                },
                Err(_) => return Err(ApiError::NotFound(format!("任务 {id} 不存在"))),
            },
            None => return Err(ApiError::BadRequest("请先选择或填写直连任务".into())),
        },
    };

    // 3. 账号：直连测试不从方案回退账号——测的就是当前输入框里的那对凭据，
    // 静默换成方案里的账号会让"我明明改了账号为什么还是旧账号的结果"无从解释
    if body.username.trim().is_empty() {
        return Err(ApiError::BadRequest("请填写账号".into()));
    }

    let profile_id = body
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());

    // 4/5 共用一次方案快照解析（密码回退与认证地址回退同源）。只有真正需要时才
    // 加载：草稿测试可以完全不传 profile_id，此时连一次方案读取都不做。
    let profile_runtime = match profile_id {
        Some(id) if body.password.is_empty() || task.auth_url.trim().is_empty() => {
            Some(config.runtime_config_for_profile(id)?)
        }
        _ => None,
    };

    let mut password = body.password;
    if password.is_empty() {
        if let Some(runtime) = &profile_runtime {
            password = Zeroizing::new(runtime.profile.password.to_string());
        }
    }
    if password.is_empty() {
        return Err(ApiError::BadRequest(
            "请输入密码；编辑已有方案时也可留空以使用已保存密码".into(),
        ));
    }

    // 5. 认证地址回退链与正式登录（LoginOrchestrator::resolve_http_task）逐字一致：
    // 任务的 auth_url 优先，留空才回退方案的 auth_url；都没有则空串（脚本拿不到页面）
    let auth_url = if task.auth_url.trim().is_empty() {
        profile_runtime
            .as_ref()
            .map(|runtime| runtime.profile.auth_url.trim().to_string())
            .unwrap_or_default()
    } else {
        task.auth_url.trim().to_string()
    };

    // 6. 证书策略：任务级显式值优先，缺省跟随全局 browser.ignore_https_errors。
    // 测试端点必须与正式登录同口径，否则会出现「测试报证书错误、实际登录成功」
    // （或反之）这种无从判断该信哪边的组合。
    let ignore_https_errors = task
        .ignore_https_errors
        .unwrap_or_else(|| config.runtime_snapshot().browser.ignore_https_errors);

    // 7. 地址基础校验先做：空地址给的是"请填写"而不是任务页文案——用户在测试面板
    // 里填的任务草稿还没保存，指向任务页会让人先去保存再回来改
    let url = task.url.trim();
    if url.is_empty() {
        return Err(ApiError::BadRequest("请填写直连请求地址".into()));
    }
    HttpLoginRequest::validate_url(url).map_err(ApiError::BadRequest)?;

    let request = HttpLoginRequest::from_task(
        &task,
        &body.username,
        password.as_str(),
        &auth_url,
        body.fetch_page,
        ignore_https_errors,
    )
    .map_err(ApiError::BadRequest)?;

    // 测试端点与正式登录同源：地址用得到本机 IP 时才查（脚本读 ctx.local_ip，
    // 或模板里直接写了 {local_ip}）；否则白跑一次网卡探测。测试端点无
    // MonitorService 注入，每次自建检测器（与 detect_profile 同口径）。
    let request = if request.needs_local_address() {
        let detector = crate::network::detect::create_detector();
        let addr = match detector.list_interfaces().await {
            Ok(list) => crate::network::local_address_from(&list),
            Err(e) => {
                tracing::debug!("测试端点查询本机地址失败（脚本将收到空 local_ip）: {e}");
                crate::network::LocalAddress::default()
            }
        };
        request.with_local_address(&addr)
    } else {
        request
    };

    let report = run_http_login_once(&request).await;
    registration.finish();
    Ok(data(serde_json::json!({
        "rendered_url": report.rendered_url,
        "rendered_headers": report.rendered_headers,
        "rendered_body": report.rendered_body,
        "status": report.status,
        "response_headers": report.response_headers,
        "response_snippet": report.response_snippet,
        "outcome": report.outcome,
        "message": report.message,
        "script_error": report.script_error,
        "duration_ms": report.duration_ms,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt; // oneshot

    use crate::config::ProfileData;
    use crate::tasks::{CommonFields, OrderData, TaskDetail, TaskError, TaskSummary};
    use crate::web::routes::test_support::{
        MockConfigApi, MockConfigInner, body_json, test_runtime_config,
    };

    /// 内存 TaskApi：直连任务 `portal-http`（地址由构造参数注入，便于指向 mock 门户）、
    /// 浏览器任务 `portal-browser`，其余 ID 一律 NotFound（覆盖"任务不存在"分支）
    struct MockTaskApi {
        /// `portal-http` 的请求地址
        http_url: String,
    }

    impl MockTaskApi {
        fn config_for(&self, task_id: &str) -> Result<TaskKind, TaskError> {
            match task_id {
                "portal-http" => Ok(TaskKind::Http(HttpTaskConfig {
                    common: CommonFields {
                        task_id: "portal-http".into(),
                        name: "门户直连".into(),
                        description: String::new(),
                    },
                    url: self.http_url.clone(),
                    success_pattern: "登录成功".into(),
                    failure_pattern: "密码错误".into(),
                    ..HttpTaskConfig::default()
                })),
                "portal-browser" => Ok(TaskKind::Browser(crate::tasks::TaskConfig::default())),
                other => Err(TaskError::TaskNotFound(other.to_string())),
            }
        }
    }

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<TaskSummary> {
            Vec::new()
        }

        async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
            self.config_for(task_id)
        }

        async fn embed_task_config(&self, _task_id: &str, _params: &mut Value) -> bool {
            false
        }

        async fn save_task(&self, _task_id: &str, _task: &TaskKind) -> Result<(), TaskError> {
            Ok(())
        }

        async fn delete_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
            let config = self.config_for(task_id)?;
            Ok(TaskDetail {
                summary: TaskSummary {
                    id: task_id.to_string(),
                    name: config.common().name.clone(),
                    description: config.common().description.clone(),
                    task_type: config.type_name().to_string(),
                    url: config.summary_url().to_string(),
                    http_method: config.http_request_method(),
                },
                config,
            })
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

        fn has_task(&self, task_id: &str) -> bool {
            matches!(task_id, "portal-http" | "portal-browser")
        }
    }

    /// 三域 state：ConfigApi（方案密码/认证地址回退）+ TaskApi + WebOperations（单飞闸门）
    #[derive(Clone)]
    struct TestState {
        config: Arc<dyn ConfigApi>,
        tasks: Arc<dyn TaskApi>,
        operations: Arc<WebOperations>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn ConfigApi> {
        fn from_ref(state: &TestState) -> Self {
            state.config.clone()
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

    /// 方案 `dorm` 的运行时快照预置已保存密码与认证地址（供回退用例断言）。
    /// `task_url` 注入 `portal-http` 任务的请求地址（含 `{username}`/`{password}`
    /// 等占位符，便于断言"实际用了哪份凭据/哪个地址"）。
    fn mock_app(task_url: &str) -> (axum::Router, Arc<std::sync::Mutex<MockConfigInner>>) {
        let (config, inner) = MockConfigApi::mocked();
        {
            let mut guard = inner.lock().unwrap();
            let mut runtime = test_runtime_config();
            runtime.profile.id = "dorm".into();
            runtime.profile.name = "宿舍".into();
            runtime.profile.auth_url = "http://profile.example/".into();
            runtime.profile.password = zeroize::Zeroizing::new("saved-secret".into());
            guard.runtime = runtime;
        }
        let state = TestState {
            config,
            tasks: Arc::new(MockTaskApi {
                http_url: task_url.to_string(),
            }),
            operations: Arc::new(WebOperations::new()),
        };
        let app = axum::Router::new()
            .route("/api/http-tasks/test", axum::routing::post(test_http_task))
            .with_state(state);
        (app, inner)
    }

    /// 起一个极简门户：请求报文里出现 `needle` 才回「登录成功」，否则回「密码错误」。
    /// 用响应结论反证"实际用了哪份凭据/哪个地址"，无需解析请求报文
    /// （也顺带避免断言写到凭据本身）。`connections` 为本用例要应答的连接数。
    async fn spawn_gate_portal_for(needle: &str, connections: usize) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let needle = needle.to_string();
        tokio::spawn(async move {
            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0_u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let received = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if received.contains(&needle) {
                    "登录成功"
                } else {
                    "密码错误"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{addr}/login")
    }

    /// 单连接门户（绝大多数用例只发一次请求）
    async fn spawn_gate_portal(needle: &str) -> String {
        spawn_gate_portal_for(needle, 1).await
    }

    fn post(body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/http-tasks/test")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// 主干一：内联草稿任务（不传 profile_id，账号密码手填）→ 200 且命中内联地址
    #[tokio::test]
    async fn test_inline_draft_task_is_used() {
        let portal = spawn_gate_portal("p=hand-typed").await;
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app
            .oneshot(post(serde_json::json!({
                "task": {
                    "type": "http",
                    "task_id": "draft",
                    "name": "草稿直连",
                    "method": "GET",
                    "url": format!("{portal}?u={{username}}&p={{password}}"),
                    "success_pattern": "登录成功",
                    "failure_pattern": "密码错误"
                },
                "username": "student",
                "password": "hand-typed",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["outcome"], "success", "{json}");
        // 渲染后的地址必须来自内联草稿（而不是任何已保存任务/方案里的地址）
        assert!(
            json["data"]["rendered_url"]
                .as_str()
                .unwrap_or_default()
                .starts_with(&portal),
            "rendered_url 未命中内联地址: {json}"
        );
    }

    /// 主干二：task_id + profile_id 且密码留空 → 用方案已保存密码，且响应不回显密码
    #[tokio::test]
    async fn test_saved_password_used_when_password_omitted() {
        // 先起门户拿到地址，再把它注入已保存任务的 url（只有此时才知道端口）
        let portal = spawn_gate_portal("p=saved-secret").await;
        let (app, _inner) = mock_app(&format!("{portal}?u={{username}}&p={{password}}"));
        let resp = app
            .oneshot(post(serde_json::json!({
                "task_id": "portal-http",
                "profile_id": "dorm",
                "username": "student",
                "password": "",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(
            json["data"]["outcome"], "success",
            "应使用方案已保存密码: {json}"
        );
        // 地址来自已保存任务（不是"请填写直连请求地址"）
        assert!(
            json["data"]["rendered_url"]
                .as_str()
                .unwrap_or_default()
                .starts_with(&portal),
            "地址应取自已保存任务: {json}"
        );
        // 回显同样脱敏：方案密码不得出现在响应任何位置
        let serialized = json.to_string();
        assert!(
            !serialized.contains("saved-secret"),
            "响应不得回显方案密码: {serialized}"
        );
    }

    /// 主干三：既没有草稿也没有 task_id → 400
    #[tokio::test]
    async fn test_missing_task_is_rejected() {
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app
            .oneshot(post(serde_json::json!({
                "username": "student",
                "password": "pw",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("请先选择或填写直连任务"),
            "{json}"
        );
    }

    /// 任务不存在 → 404；任务存在但非直连类型 → 400（两种修复动作不同，不能混）
    #[tokio::test]
    async fn test_task_id_errors_are_distinguished() {
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app
            .clone()
            .oneshot(post(serde_json::json!({
                "task_id": "ghost",
                "username": "student",
                "password": "pw"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .oneshot(post(serde_json::json!({
                "task_id": "portal-browser",
                "username": "student",
                "password": "pw"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不是直连任务"),
            "{json}"
        );
    }

    /// 账号/密码缺失各有明确文案（沿用旧端点文案）
    #[tokio::test]
    async fn test_username_and_password_are_required() {
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let task = serde_json::json!({
            "type": "http",
            "url": "http://10.1.1.55/login",
            "success_pattern": "登录成功"
        });

        let resp = app
            .clone()
            .oneshot(post(serde_json::json!({
                "task": task,
                "username": "   ",
                "password": "pw"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("请填写账号"),
            "{json}"
        );

        let resp = app
            .oneshot(post(serde_json::json!({
                "task": task,
                "username": "student",
                "password": ""
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("请输入密码"),
            "{json}"
        );
    }

    /// 草稿空地址 → 400「请填写直连请求地址」：任务尚未保存，文案不该指向任务页
    #[tokio::test]
    async fn test_empty_draft_url_is_rejected_with_edit_hint() {
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app
            .oneshot(post(serde_json::json!({
                "task": { "type": "http", "url": "   " },
                "username": "student",
                "password": "pw"
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("请填写直连请求地址"),
            "{json}"
        );
    }

    /// 认证地址回退链：任务 auth_url 优先，留空才回退方案的 auth_url。
    ///
    /// 用 `{auth_url}` 占位符把实际解析结果带进请求，由门户按 needle 决定成败——
    /// 无需脚本即可证明"究竟用了哪个认证地址"（与正式登录
    /// `LoginOrchestrator::resolve_http_task` 同一契约，也与前端编辑器同口径）。
    #[tokio::test]
    async fn test_auth_url_prefers_task_then_falls_back_to_profile() {
        // 1. 任务自带 auth_url → 用它
        let portal = spawn_gate_portal("auth=http://task.example/").await;
        let task_url = format!("{portal}?auth={{auth_url}}");
        let (app, _inner) = mock_app(&task_url);
        let resp = app
            .clone()
            .oneshot(post(serde_json::json!({
                "task": {
                    "type": "http",
                    "method": "GET",
                    "url": task_url,
                    "auth_url": "http://task.example/",
                    "success_pattern": "登录成功",
                    "failure_pattern": "密码错误"
                },
                "profile_id": "dorm",
                "username": "student",
                "password": "pw",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        // 模板替换是逐字替换（不转义），故断言按原文形式比对
        assert!(
            json["data"]["rendered_url"]
                .as_str()
                .unwrap_or_default()
                .contains("auth=http://task.example/"),
            "任务 auth_url 必须优先: {json}"
        );

        // 2. 任务留空 → 回退方案的 auth_url
        let portal = spawn_gate_portal("auth=http://profile.example/").await;
        let task_url = format!("{portal}?auth={{auth_url}}");
        let (app, _inner) = mock_app(&task_url);
        let resp = app
            .oneshot(post(serde_json::json!({
                "task": {
                    "type": "http",
                    "method": "GET",
                    "url": task_url,
                    "success_pattern": "登录成功",
                    "failure_pattern": "密码错误"
                },
                "profile_id": "dorm",
                "username": "student",
                "password": "pw",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(
            json["data"]["rendered_url"]
                .as_str()
                .unwrap_or_default()
                .contains("auth=http://profile.example/"),
            "任务留空时应回退方案的认证地址: {json}"
        );
    }

    /// 无 profile_id 且任务 auth_url 留空时认证地址为空串（脚本 ctx.auth_url 为空），
    /// 不得回退到全局默认值或别的方案的地址
    #[tokio::test]
    async fn test_auth_url_empty_without_profile_or_task() {
        let portal = spawn_gate_portal("login").await;
        // 末尾再带一个参数，便于断言 `a=` 被判为空（`a=&p=...`）
        let task_url = format!("{portal}?a={{auth_url}}&p={{password}}");
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app
            .oneshot(post(serde_json::json!({
                "task": {
                    "type": "http",
                    "method": "GET",
                    "url": task_url,
                    "success_pattern": "登录成功",
                    "failure_pattern": "密码错误"
                },
                "username": "student",
                "password": "pw",
                "fetch_page": false
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        // `{auth_url}` 被替换为空串 → URL 里留下 "a=&"
        assert!(
            json["data"]["rendered_url"]
                .as_str()
                .unwrap_or_default()
                .contains("a=&"),
            "无方案且任务未填时应为空串: {json}"
        );
    }

    /// 单飞闸门必须随请求结束释放：连续两次请求都成功，第二次不得被 409 永久挡住
    #[tokio::test]
    async fn test_registration_releases_capacity_between_requests() {
        // 两次请求各建一条连接，故门户需能连续应答两轮
        let portal = spawn_gate_portal_for("p=pw", 2).await;
        let (app, _inner) = mock_app(&format!("{portal}?u={{username}}&p={{password}}"));
        for round in 0..2 {
            let resp = app
                .clone()
                .oneshot(post(serde_json::json!({
                    "task_id": "portal-http",
                    "username": "student",
                    "password": "pw",
                    "fetch_page": false
                })))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "第 {} 次请求被闸门挡住（登记守卫未释放容量）",
                round + 1
            );
            let json = body_json(resp).await;
            assert_eq!(json["data"]["outcome"], "success", "第 {} 轮", round + 1);
        }
    }

    /// 空请求体（默认值）按"未选择任务"处理，不 panic
    #[tokio::test]
    async fn test_default_body_is_handled() {
        let (app, _inner) = mock_app("http://10.1.1.55/login");
        let resp = app.oneshot(post(serde_json::json!({}))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 编译期护栏：方案的直连请求参数已不存在，只剩绑定关系
    #[test]
    fn profile_data_only_carries_http_task_binding() {
        let profile = ProfileData::default();
        assert!(profile.active_http_task.is_empty());
    }
}
