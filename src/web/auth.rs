//! 本地 API 鉴权：随机 token 校验
//!
//! 威胁模型：HTTP 服务绑定 127.0.0.1 只能挡住外部网络直连，挡不住
//! 本地任意进程与浏览器中的恶意网页（CORS 不阻止 `text/plain` 等简单
//! 请求的 CSRF）。因此所有 `/api/*` 与 `/ws/*` 请求必须携带启动时
//! 生成的随机 token，否则本地恶意网页即可调用删除任务、关闭应用、
//! 执行脚本等危险接口。
//!
//! token 的分发与信任边界：
//! - 启动时生成并持久化到 `config/.auth_token`（仅当前用户可读的目录内）；
//! - 前端通过 `GET /api/auth/token` 获取：跨域恶意网页虽能发送该请求，
//!   但响应读取受 CORS 限制（仅放行 localhost Origin），无法获得 token；
//! - 本机同用户进程可读取 token 文件——这与"能直接读配置/杀进程"的
//!   权限等价，不构成额外暴露面。

use std::path::Path;

use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::error::data;
use super::state::AppState;

/// token 持久化文件名（相对于 `config/`）
const AUTH_TOKEN_FILE: &str = ".auth_token";

/// 生成 64 位十六进制随机 token（两个 UUIDv4 拼接，复用现有 uuid 依赖）
fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 加载（或首次生成）API 鉴权 token
///
/// 文件为空或含非法字符（意外被篡改/损坏）时重新生成并覆写。
pub fn load_or_create_token(base_path: &Path) -> std::io::Result<String> {
    let path = base_path.join("config").join(AUTH_TOKEN_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let token = text.trim();
        // 合法形态：32~128 位十六进制
        let valid =
            (32..=128).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit());
        if valid {
            return Ok(token.to_string());
        }
        tracing::warn!("鉴权 token 文件异常，已重新生成");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = generate_token();
    std::fs::write(&path, &token)?;
    Ok(token)
}

/// 读取 token 文件（供本机调用方如 stop_instance 使用；不存在返回 None）
pub fn read_token_file(base_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(base_path.join("config").join(AUTH_TOKEN_FILE)).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// GET /api/auth/token — 向前端发放鉴权 token
///
/// 响应读取受 CORS 保护（仅 localhost Origin 可读），跨域恶意网页
/// 无法获取 token，因此该端点可以豁免鉴权开放。
pub async fn token_handler(State(state): State<AppState>) -> Response {
    let mut response = data(serde_json::json!({ "token": &*state.auth_token })).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// 无 token 时的 401 响应（沿用统一错误信封格式）
fn unauthorized() -> Response {
    let body = r#"{"error":{"code":"UNAUTHORIZED","message":"缺少或错误的鉴权 token"}}"#;
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// 常量时间字符串比较（token 专用，防时间侧信道）
///
/// `==` 对字符串逐字节短路比较：token 前缀匹配越多耗时越长，攻击者可据此
/// 逐段猜解。本实现把长度差异与逐字节 XOR 差异累积到同一个 `diff`，
/// 无论内容差异出现在第几个字节，循环都完整走完最长长度，耗时只与
/// 输入长度相关、与内容无关。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // 长度差异用 usize 记录（避免截断为 u8 后恰好归零），不提前返回，
    // 保证长度不同时也走完整个循环
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        // 越界侧取 0：另一侧的非零字节必然污染 diff
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// 从 query string 提取 `token=` 参数（token 为纯十六进制，无需 URL 解码）
fn token_from_query(query: Option<&str>) -> Option<&str> {
    let query = query?;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// 鉴权中间件：校验 `/api/*` 与 `/ws/*` 请求携带的 token
///
/// 仅依赖 token 本身（`Arc<str>`），与 AppState 解耦，便于独立测试
///
/// 豁免规则：
/// - 非 `/api`、`/ws` 前缀（静态资源 / openapi.json）
/// - `OPTIONS` 请求（CORS 预检不携带自定义头）
/// - `/api/auth/token`（token 发放端点，受 CORS 读保护）
/// - `/api/health`（无信息量的存活探测）
/// - `GET /api/system/info` / `GET /api/monitor/status`（B 方案只读状态开放，轮询/探活）
/// - `GET /api/background/*`（CSS `url()` / `<img>` 引用无法携带自定义头，
///   背景图为只读图片资源；写操作（upload/fetch-url/delete）仍需鉴权）
///
/// token 来源：
/// - HTTP：`X-Auth-Token` 头或 `Authorization: Bearer <token>`
/// - WebSocket：浏览器 WS 无法携带自定义头，使用 `?token=` 查询参数
pub async fn auth_middleware(
    State(expected): State<std::sync::Arc<str>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api") && !path.starts_with("/ws") {
        return next.run(req).await;
    }
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    if path == "/api/auth/token" || path == "/api/health" {
        return next.run(req).await;
    }
    if req.method() == Method::GET
        && (path.starts_with("/api/background/")
            || path == "/api/tools/task-recorder.user.js"
            || path == "/api/docs/task-writing-guide"
            || path == "/api/docs/task-manual"
            || path == "/api/system/info"
            || path == "/api/monitor/status")
    {
        return next.run(req).await;
    }

    let expected: &str = &expected;
    let provided = if path.starts_with("/ws") {
        token_from_query(req.uri().query())
    } else {
        let headers = req.headers();
        headers
            .get("x-auth-token")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or_else(|| {
                headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(str::trim)
            })
    };

    match provided {
        // 常量时间比较：防止逐字节短路比较泄露 token 前缀匹配程度
        Some(token) if constant_time_eq(token, expected) => next.run(req).await,
        _ => unauthorized(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成的 token 应为 64 位十六进制
    #[test]
    fn test_generate_token_format() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// query 参数提取：正常 / 缺失 / 空 / 多参数混合
    #[test]
    fn test_token_from_query() {
        assert_eq!(token_from_query(Some("token=abc123")), Some("abc123"));
        assert_eq!(token_from_query(Some("foo=1&token=abc")), Some("abc"));
        assert_eq!(token_from_query(Some("token=&foo=1")), None);
        assert_eq!(token_from_query(Some("foo=1")), None);
        assert_eq!(token_from_query(None), None);
    }

    /// 常量时间比较：相等 / 不等 / 大小写敏感 / 长度差异
    #[test]
    fn test_constant_time_eq() {
        // 完全相等 → true
        assert!(constant_time_eq(TEST_TOKEN, TEST_TOKEN));
        assert!(constant_time_eq("", ""));
        // 内容不等 → false（含仅首字节/末字节不同的最坏情形）
        assert!(!constant_time_eq(
            &format!("X{}", &TEST_TOKEN[1..]),
            TEST_TOKEN
        ));
        assert!(!constant_time_eq(
            &format!("{}X", &TEST_TOKEN[..TEST_TOKEN.len() - 1]),
            TEST_TOKEN
        ));
        // 大小写敏感：十六进制 token 中 a 与 A 必须不等
        assert!(!constant_time_eq("abcdef0123456789", "ABCDEF0123456789"));
        // 长度不同 → false（长度差异不得被截断归零，长输入覆盖短输入）
        assert!(!constant_time_eq("short", TEST_TOKEN));
        assert!(!constant_time_eq(TEST_TOKEN, "short"));
        // 互为前缀也不相等
        assert!(!constant_time_eq(TEST_TOKEN, &TEST_TOKEN[..16]));
    }

    /// load_or_create_token：首次生成 → 持久化复用 → 损坏时重建
    #[test]
    fn test_load_or_create_token_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ca-token-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 清理历史残留
        let _ = std::fs::remove_file(dir.join("config").join(AUTH_TOKEN_FILE));

        let t1 = load_or_create_token(&dir).unwrap();
        let t2 = load_or_create_token(&dir).unwrap();
        assert_eq!(t1, t2, "二次加载应复用已持久化的 token");

        // 写入损坏内容后应重建
        std::fs::write(dir.join("config").join(AUTH_TOKEN_FILE), "!!!invalid!!!").unwrap();
        let t3 = load_or_create_token(&dir).unwrap();
        assert_ne!(t1, t3);
        assert_eq!(t3.len(), 64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ============ 鉴权中间件行为（端到端，oneshot 直连 Router） ============

    const TEST_TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

    /// 构建仅含鉴权中间件与测试路由的迷你 Router（中间件只依赖 token）
    fn test_router() -> axum::Router {
        use axum::routing::get;
        use std::sync::Arc;
        let state: Arc<str> = Arc::from(TEST_TOKEN);
        axum::Router::new()
            .route("/api/ping", get(|| async { "pong" }))
            .route("/ws/logs", get(|| async { "ws" }))
            .fallback(|| async { "static" })
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::auth_middleware,
            ))
            .with_state(state)
    }

    #[tokio::test]
    async fn auth_middleware_blocks_api_without_token() {
        use axum::http::Request;
        use tower::ServiceExt;

        let app = test_router();
        // 无 token → 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ping")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

        // 错误 token → 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ping")
                    .header("X-Auth-Token", "wrong-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

        // 正确 token → 200
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ping")
                    .header("X-Auth-Token", TEST_TOKEN)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Bearer 形式 → 200
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ping")
                    .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_exemptions() {
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        let app = test_router();
        // 静态资源（非 /api、/ws）→ 放行
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/index.html")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // CORS 预检 OPTIONS → 放行（预检不带自定义头）。
        // 测试 Router 未挂 CorsLayer，放行后由路由层返回 405（无 OPTIONS handler）；
        // 只要不是 401 即证明中间件放行（生产环境预检由 CorsLayer 直接响应）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/ping")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

        // WS：query token 正确 → 放行；缺失 → 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/ws/logs?token={TEST_TOKEN}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/ws/logs")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    /// GET /api/background/* 豁免：CSS url()/img 引用无法携带自定义头；
    /// 非背景路径与其他方法仍需鉴权
    #[tokio::test]
    async fn auth_middleware_background_get_exempt() {
        use axum::http::{Method, Request, StatusCode};
        use tower::ServiceExt;

        let app = test_router();

        // GET 背景图：无 token → 放行（路由层 405 只证明过了中间件）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/background/bg.png")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);

        // POST /api/background/upload：无 token → 401（写操作不豁免）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/background/upload")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // GET 非背景 API：无 token → 401（豁免不扩大化）
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/backgrounds")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
