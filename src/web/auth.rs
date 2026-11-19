//! 本地 API 鉴权：token 校验 + bootstrap/管理密码换取
//!
//! 威胁模型：HTTP 服务绑定 127.0.0.1 只能挡住外部网络直连，挡不住
//! 本地任意进程与浏览器中的恶意网页（CORS 不阻止 `text/plain` 等简单
//! 请求的 CSRF）。因此所有 `/api/*` 与 `/ws/*` 请求必须携带启动时
//! 生成的随机 token，否则本地恶意网页即可调用删除任务、关闭应用、
//! 执行脚本等危险接口。
//!
//! token 的分发与信任边界（收口后的新模型）：
//! - 启动时与 bootstrap 握手秘钥**成对**生成并持久化到 `config/.auth_token` /
//!   `config/.auth_bootstrap`（创建即 0600，仅当前用户可读）；任一缺失/损坏
//!   重建时两者一并轮换，保证 fragment、exchange 比对与 token 始终同代；
//! - **回环绑定的本地免引导**：`GET /api/auth/local-token` 在回环监听下直接
//!   下发 token（本机进程视为可信）。这是 2026-11-13 的明确取舍：旧的免鉴权
//!   `GET /api/auth/token` 被删，是因为它把「本机其他用户」升级成「应用用户」
//!   （配合 `POST /api/scripts/run` 即任意脚本执行）；单人机与单一共享账号
//!   场景下这条边界本就不存在，而每次打开都要带 fragment 的代价反而成了纯本地
//!   使用的主障碍。多用户共享机上的跨用户提权风险由此**明确接受**；
//!   非回环绑定默认 404，但显式 `--allow-unauthenticated-bind` 时同样可用
//!   （默认 Docker compose 即此形态：容器内绑 0.0.0.0、宿主只发布到
//!   127.0.0.1——服务端无法区分宿主与局域网的流量，信任边界由部署方声明）；
//!   未豁免的远程仍只认管理密码，不存在远程免鉴权发 token 的入口；
//! - 界面 URL 仍以 `#auth=<bootstrap>` fragment 携带握手秘钥（fragment 不会被
//!   浏览器发往服务器、不进服务器日志）；前端经 `POST /api/auth/exchange`
//!   用 bootstrap 换 token，随后应自行清除地址栏 fragment（本地首次打开失败
//!   时由上面的 local-token 兜底，二者等价）；
//! - bootstrap 比对用常量时间比较，失败 401；管理密码分支仅**非回环绑定**
//!   （Docker/LAN 显式暴露）接受——回环威胁是本地进程与恶意网页，管理密码
//!   面向远程管理员，回环开放该分支只会给本地攻击者多一条可猜解路径；
//!   密码未配置 → 401 `AUTH_NOT_CONFIGURED`；非回环绑定且无凭据由监听器
//!   准备点 fail-closed 拒绝启动（见 `app::check_bind_credential_gate`）；
//! - 本机同用户进程仍可读取秘钥文件，且回环下可直接经 local-token 取 token——
//!   两者都与「能直接读配置/杀进程」的同用户权限等价；其他用户进程在回环绑定下
//!   同样可取 token（2026-11-13 已接受的取舍），非回环绑定不开放该端点。
//!
//! TODO: viewer 只读 token 权限矩阵——将来如需「只读访客」能力，在
//! [`AuthCredentials`] 增加第三类秘钥（viewer_token），授予点即
//! [`load_or_create_auth`] 的成对生成处（同样 0600 落盘、损坏成对轮换）；
//! 权限边界：`auth_middleware` 按携带秘钥类型区分，viewer 仅豁免
//! `GET /api/system/info` / `GET /api/monitor/status` / `GET /api/history` 等
//! 只读白名单端点，绝不放行 `/api/scripts/*`、任务执行、配置写入与 `/ws`。

use std::borrow::Cow;
use std::path::Path;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::error::data;
use super::host::HostPolicy;
use super::state::AppState;

/// token 持久化文件名（相对于 `config/`）
const AUTH_TOKEN_FILE: &str = ".auth_token";
/// bootstrap 握手秘钥持久化文件名（相对于 `config/`，与 token 成对轮换）
const AUTH_BOOTSTRAP_FILE: &str = ".auth_bootstrap";

/// 成对加载/生成的鉴权凭据：token 用于 `/api/*`、`/ws/*` 校验，bootstrap
/// 用于界面 fragment 与 `POST /api/auth/exchange` 的比对
pub struct AuthCredentials {
    /// API 鉴权 token
    pub token: String,
    /// bootstrap 握手秘钥（与 token 同代）
    pub bootstrap: String,
}

/// 生成 64 位十六进制随机秘钥（两个 UUIDv4 拼接，复用现有 uuid 依赖）
fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 成对加载（或首次生成）token 与 bootstrap 握手秘钥
///
/// 两文件均有效即复用；任一缺失/损坏即**成对轮换**（fragment、exchange
/// 比对与 token 必须来自同一代凭据，单独轮换其一会让另一份成为孤儿）。
pub fn load_or_create_auth(base_path: &Path) -> std::io::Result<AuthCredentials> {
    let token_path = base_path.join("config").join(AUTH_TOKEN_FILE);
    let bootstrap_path = base_path.join("config").join(AUTH_BOOTSTRAP_FILE);
    let token = read_secret_file(&token_path, 32..=128);
    let bootstrap = read_secret_file(&bootstrap_path, 64..=64);
    if let (Some(token), Some(bootstrap)) = (token, bootstrap) {
        return Ok(AuthCredentials { token, bootstrap });
    }
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let credentials = AuthCredentials {
        token: generate_token(),
        bootstrap: generate_token(),
    };
    write_secret_file(&token_path, &credentials.token)?;
    write_secret_file(&bootstrap_path, &credentials.bootstrap)?;
    Ok(credentials)
}

/// 读取并校验秘钥文件（十六进制、长度在合法区间）；缺失返回 None，
/// 损坏 warn 留痕（严禁打印内容本身）后返回 None，由调用方成对重建
fn read_secret_file(path: &Path, valid_len: std::ops::RangeInclusive<usize>) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let secret = text.trim();
    // 合法形态：长度在区间内的纯十六进制
    let len_ok = valid_len.contains(&secret.len());
    let valid = len_ok && secret.bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        return Some(secret.to_string());
    }
    // 记录具体校验失败原因便于排查篡改/损坏（严禁打印秘钥内容本身）
    let reason = if len_ok {
        "包含非十六进制字符".to_string()
    } else {
        format!(
            "长度 {} 不在 {}~{} 位合法范围",
            secret.len(),
            valid_len.start(),
            valid_len.end()
        )
    };
    tracing::warn!(
        "秘钥文件 {} 异常（{reason}），将随配对秘钥一并重新生成",
        path.display()
    );
    None
}

/// 以「创建即 0600」的方式写秘钥文件
///
/// Unix 用 `OpenOptions::mode(0o600)`：mode 仅在**创建**新文件时生效，首次
/// 落盘就是收紧后的权限，消灭「先按 umask 0644 落盘再 chmod」两步之间的
/// 竞态窗口（两步间崩溃 → 秘钥永久全机可读）。覆写已存在的文件（损坏重建）
/// 时 mode 不生效，由 [`tighten_file_permissions`] 写后收紧兜底（旧版本创建的
/// 宽权限文件借此升级）。
fn write_secret_file(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
    }
    tighten_file_permissions(path);
    Ok(())
}

/// 收紧密钥类文件权限（尽力而为，失败 warn 留痕不阻断：内容已写入，仅权限未收紧）
///
/// Unix 0600；Windows 借 icacls 移除继承权限并授予当前用户完全控制。
fn tighten_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(path, perms) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "收紧秘钥文件权限为 0600 失败"
            );
        }
    }
    #[cfg(windows)]
    {
        tighten_windows_acl(path);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
    }
}

/// Windows：icacls 移除继承权限并仅授予当前用户完全控制
///
/// icacls 调用的成败必须检查：域账号解析失败等环境问题会让 ACL 收紧静默
/// 失效，warn 留痕（含退出码）便于排查。
#[cfg(windows)]
fn tighten_windows_acl(path: &Path) {
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        tracing::warn!(
            path = %path.display(),
            "无法确定当前用户名（USERNAME 为空），跳过 Windows ACL 收紧"
        );
        return;
    }
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let account = if domain.is_empty() {
        username
    } else {
        format!("{domain}\\{username}")
    };
    match std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg(format!("/grant:r \"{account}\":(F)"))
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(
            path = %path.display(),
            code = ?status.code(),
            "icacls 收紧秘钥文件 ACL 失败"
        ),
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "icacls 调用失败"),
    }
}

/// 读取 token 文件（供本机调用方如 stop_instance 使用；不存在返回 None）
pub fn read_token_file(base_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(base_path.join("config").join(AUTH_TOKEN_FILE)).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// 读取 bootstrap 秘钥文件（供托盘/launcher 拼接界面 fragment；不存在返回 None）
pub fn read_bootstrap_file(base_path: &Path) -> Option<String> {
    read_secret_file(&base_path.join("config").join(AUTH_BOOTSTRAP_FILE), 64..=64)
}

/// 构建打开管理界面的 URL：携带 `#auth=<bootstrap>` fragment（纯函数，便于单测）
///
/// fragment 不会被浏览器发往服务器（随请求发送的是 href 去掉 fragment 后的
/// 部分），因此 bootstrap 不进服务器访问日志；`bootstrap` 为 None/空（文件
/// 缺失/损坏且尚未重建，或跨版本场景）时退化为不带 fragment 的普通 URL。
pub fn ui_url(port: u16, bootstrap: Option<&str>) -> String {
    let mut url = format!("http://127.0.0.1:{port}/");
    if let Some(secret) = bootstrap
        && !secret.is_empty()
    {
        url.push_str("#auth=");
        url.push_str(secret);
    }
    url
}

/// 计算生效管理密码（纯函数，便于单测）
///
/// 覆盖值（CLI `--management-password` / env `CAMPUS_AUTH_MANAGEMENT_PASSWORD`）
/// 非空时优先；否则回退配置值（exchange 每次请求实时从 ConfigService 读取，
/// UI 修改即时生效）。返回空串表示管理密码未配置。
pub fn effective_management_password<'a>(
    override_value: Option<&'a str>,
    config_value: &'a str,
) -> Cow<'a, str> {
    match override_value {
        Some(value) if !value.is_empty() => Cow::Borrowed(value),
        _ => Cow::Borrowed(config_value),
    }
}

/// exchange 请求体：两字段均可选，「恰含其一」的组合校验在
/// `respond_exchange` 完成；未知键宽容忽略（对本契约无安全影响）
#[derive(Debug, Default, Deserialize)]
pub struct ExchangeBody {
    /// bootstrap 握手秘钥（界面 URL fragment 携带）
    pub bootstrap: Option<String>,
    /// 管理密码（仅非回环绑定接受）
    pub password: Option<String>,
}

/// 本地代发端点是否可用：回环监听，或显式豁免无凭据非回环绑定
///
/// 后者（`--allow-unauthenticated-bind`）服务的是「容器内绑 0.0.0.0、宿主只发布到
/// 127.0.0.1」的默认 Docker 部署：容器里看不到宿主端口映射，无法自行区分宿主回环
/// 与局域网访问，只能由部署方显式声明信任边界。把端口发布到 LAN 时必须去掉该豁免。
fn local_token_allowed(policy: HostPolicy, allow_unauthenticated_bind: bool) -> bool {
    policy == HostPolicy::LoopbackOnly || allow_unauthenticated_bind
}

/// GET /api/auth/local-token — 回环监听（或显式豁免的非回环绑定）下直接下发 token
///
/// 取舍见模块文档：回环绑定视为「本机可信」（与能读配置/执行脚本的同用户权限
/// 等价）；非回环且未显式豁免时一律 404——远程仍只认管理密码，不存在
/// 远程免鉴权入口。响应同样是 `{"data":{"token":"..."}}` + `Cache-Control: no-store`。
pub async fn local_token_handler(State(state): State<AppState>) -> Response {
    if !local_token_allowed(state.host_policy, state.allow_unauthenticated_bind) {
        return error_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "本地代发端点仅在回环监听或显式豁免无凭据绑定时可用",
        );
    }
    token_response(&state.auth_token)
}

/// POST /api/auth/exchange — 以 bootstrap 秘钥或管理密码换取鉴权 token
///
/// 请求体恰含其一：`{"bootstrap": "<64hex>"}` 或 `{"password": "<管理密码>"}`；
/// 两者同现 / 均缺 / 非法 JSON 或字段类型错误 → 400。
///
/// 成功 200 `{"data":{"token":"..."}}`（`Cache-Control: no-store`）；失败 401
/// 统一错误信封（管理密码未配置时 `error.code = "AUTH_NOT_CONFIGURED"`）。
/// 绝不记录请求体或任何秘钥/token 到日志。
pub async fn exchange_handler(
    State(state): State<AppState>,
    payload: Result<Json<ExchangeBody>, JsonRejection>,
) -> Response {
    let body = match payload {
        Ok(Json(body)) => body,
        Err(_) => {
            // 仅记结果，绝不记录请求体（可能含 bootstrap/管理密码）
            tracing::debug!(path = "/api/auth/exchange", "请求体非法，返回 400");
            return bad_request("请求体必须是合法 JSON 对象，且恰含 bootstrap 或 password 之一");
        }
    };
    // 配置值每次请求实时读取：UI 修改 management_password 后无需重启即生效
    let password_config = state
        .config
        .load_settings_async()
        .await
        .global
        .app
        .management_password;
    respond_exchange(
        &state.auth_token,
        &state.auth_bootstrap,
        state.management_password_override.as_deref(),
        &password_config,
        state.host_policy == HostPolicy::LoopbackOnly,
        body,
    )
}

/// exchange 换取逻辑核心（与 AppState 解耦，便于单测）
///
/// `loopback_bind` 为真表示当前监听绑定在回环地址（`HostPolicy::LoopbackOnly`），
/// 此时管理密码分支一律拒绝。
fn respond_exchange(
    token: &str,
    expected_bootstrap: &str,
    password_override: Option<&str>,
    password_config: &str,
    loopback_bind: bool,
    body: ExchangeBody,
) -> Response {
    match (body.bootstrap, body.password) {
        (Some(_), Some(_)) => bad_request("bootstrap 与 password 只能提供其一"),
        (None, None) => bad_request("请求体必须提供 bootstrap 或 password 之一"),
        (Some(bootstrap), None) => {
            // 常量时间比较：防止逐字节短路比较泄露秘钥前缀匹配程度
            if constant_time_eq(&bootstrap, expected_bootstrap) {
                token_response(token)
            } else {
                unauthorized_with_code("UNAUTHORIZED", "bootstrap 秘钥不正确")
            }
        }
        (None, Some(password)) => {
            if loopback_bind {
                return unauthorized_with_code(
                    "UNAUTHORIZED",
                    "回环绑定下不接受管理密码换取 token（管理密码面向 Docker/局域网部署）",
                );
            }
            let effective = effective_management_password(password_override, password_config);
            if effective.is_empty() {
                return unauthorized_with_code(
                    "AUTH_NOT_CONFIGURED",
                    "管理密码未配置：请在设置中配置 management_password，或以 --management-password 启动参数提供",
                );
            }
            if constant_time_eq(&password, &effective) {
                token_response(token)
            } else {
                unauthorized_with_code("UNAUTHORIZED", "管理密码不正确")
            }
        }
    }
}

/// 200 成功响应：`{"data":{"token":"..."}}` + `no-store`（token 不得被缓存落盘）
fn token_response(token: &str) -> Response {
    let mut response = data(serde_json::json!({ "token": token })).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// 400 响应（沿用统一错误信封格式）
fn bad_request(message: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
}

/// 指定 code 的 401 响应（沿用统一错误信封格式，message 中文且不泄露秘钥信息）
fn unauthorized_with_code(code: &str, message: &str) -> Response {
    error_response(StatusCode::UNAUTHORIZED, code, message)
}

/// 无 token 时的 401 响应（沿用统一错误信封格式）
fn unauthorized() -> Response {
    unauthorized_with_code("UNAUTHORIZED", "缺少或错误的鉴权 token")
}

/// 统一错误信封响应构造
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "error": { "code": code, "message": message } }).to_string();
    let mut resp = Response::new(body.into());
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// 常量时间字符串比较（秘钥比对专用，防时间侧信道）
///
/// `==` 对字符串逐字节短路比较：秘钥前缀匹配越多耗时越长，攻击者可据此
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
/// - `POST /api/auth/exchange`（bootstrap/管理密码换取端点：请求方必须已持有
///   不经网络发放的凭据——fragment 秘钥或管理密码——豁免它不构成绕过；
///   旧 `GET /api/auth/token` 已删除，任何端点都不再免鉴权发放 token）
/// - `/api/health`（无信息量的存活探测）
/// - `GET /api/system/info` / `GET /api/monitor/status`（B 方案只读状态开放，轮询/探活）
/// - `GET /api/background/*`（CSS `url()` / `<img>` 引用无法携带自定义头，
///   背景图为只读图片资源；写操作（upload/fetch-url/delete）仍需鉴权）
/// - `GET /api/debug/screenshot/*`（同理：调试面板 `<img>` 预览截图，只读 PNG）
/// - `GET /api/ai/capture/screenshot`（同理：AI 生成页 `<img>` 预览捕获截图，只读 PNG）
/// - `GET /api/repo/image`（同理：仓库导入 `<img>` 预览任务站截图；
///   出站目标限死任务站 raw 三 host，见 `routes::repo::repo_image`）
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
    if path == "/api/auth/exchange" || path == "/api/auth/local-token" || path == "/api/health" {
        return next.run(req).await;
    }
    if req.method() == Method::GET
        && (path.starts_with("/api/background/")
            || path.starts_with("/api/debug/screenshot/")
            || path == "/api/ai/capture/screenshot"
            || path == "/api/repo/image"
            || path == "/api/tools/task-recorder.user.js"
            || path == "/api/docs/task-writing-guide"
            || path == "/api/docs/task-manual"
            || path == "/api/docs/http-login-guide"
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
        _ => {
            // 仅记 method/path 便于定位调用方，绝不记录请求提供的 token 值
            tracing::debug!(method = %req.method(), path, "鉴权失败，返回 401");
            unauthorized()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成的秘钥应为 64 位十六进制
    #[test]
    fn test_generate_token_format() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// 本地代发端点：回环监听，或显式豁免无凭据的非回环绑定（Docker 默认 compose）
    #[test]
    fn test_local_token_allowed() {
        assert!(local_token_allowed(HostPolicy::LoopbackOnly, false));
        assert!(!local_token_allowed(HostPolicy::AllowAny, false));
        assert!(local_token_allowed(HostPolicy::AllowAny, true));
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
        // 大小写敏感：十六进制秘钥中 a 与 A 必须不等
        assert!(!constant_time_eq("abcdef0123456789", "ABCDEF0123456789"));
        // 长度不同 → false（长度差异不得被截断归零，长输入覆盖短输入）
        assert!(!constant_time_eq("short", TEST_TOKEN));
        assert!(!constant_time_eq(TEST_TOKEN, "short"));
        // 互为前缀也不相等
        assert!(!constant_time_eq(TEST_TOKEN, &TEST_TOKEN[..16]));
    }

    /// load_or_create_auth：首次成对生成 → 持久化复用
    #[test]
    fn test_load_or_create_auth_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let c1 = load_or_create_auth(dir.path()).unwrap();
        assert_eq!(c1.token.len(), 64);
        assert_eq!(c1.bootstrap.len(), 64);
        let c2 = load_or_create_auth(dir.path()).unwrap();
        assert_eq!(c1.token, c2.token, "二次加载应复用已持久化的 token");
        assert_eq!(
            c1.bootstrap, c2.bootstrap,
            "二次加载应复用已持久化的 bootstrap"
        );
    }

    /// 成对轮换：token 或 bootstrap 任一损坏/缺失，两者一并重新生成
    #[test]
    fn test_paired_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let c1 = load_or_create_auth(dir.path()).unwrap();

        // token 损坏 → 两者都换（bootstrap 不能留在上一代）
        std::fs::write(
            dir.path().join("config").join(AUTH_TOKEN_FILE),
            "!!!invalid!!!",
        )
        .unwrap();
        let c2 = load_or_create_auth(dir.path()).unwrap();
        assert_ne!(c1.token, c2.token);
        assert_ne!(c1.bootstrap, c2.bootstrap);
        assert_eq!(c2.token.len(), 64);
        assert_eq!(c2.bootstrap.len(), 64);

        // bootstrap 损坏 → 同样成对轮换
        std::fs::write(dir.path().join("config").join(AUTH_BOOTSTRAP_FILE), "zzz").unwrap();
        let c3 = load_or_create_auth(dir.path()).unwrap();
        assert_ne!(c2.token, c3.token);
        assert_ne!(c2.bootstrap, c3.bootstrap);

        // bootstrap 文件缺失 → 同样成对轮换
        std::fs::remove_file(dir.path().join("config").join(AUTH_BOOTSTRAP_FILE)).unwrap();
        let c4 = load_or_create_auth(dir.path()).unwrap();
        assert_ne!(c3.token, c4.token);
    }

    /// 秘钥文件在 unix 上创建即 0600（首启创建与损坏重建两条路径）
    #[cfg(unix)]
    #[test]
    fn test_secret_files_created_with_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create_auth(dir.path()).unwrap();
        for name in [AUTH_TOKEN_FILE, AUTH_BOOTSTRAP_FILE] {
            let mode = std::fs::metadata(dir.path().join("config").join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name} 首次创建即应为 0600");
        }

        // 损坏重建：先把文件权限放宽到 0644 再触发重建，覆写后必须被收紧
        let token_path = dir.path().join("config").join(AUTH_TOKEN_FILE);
        std::fs::write(&token_path, "bad!").unwrap();
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create_auth(dir.path()).unwrap();
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "覆写已存在的宽权限文件后应收紧为 0600");
    }

    /// ui_url：fragment 拼接与退化形态
    #[test]
    fn test_ui_url_fragment() {
        assert_eq!(
            ui_url(50721, Some("abc123")),
            "http://127.0.0.1:50721/#auth=abc123"
        );
        // bootstrap 缺失/为空 → 不带 fragment 的普通 URL
        assert_eq!(ui_url(50721, None), "http://127.0.0.1:50721/");
        assert_eq!(ui_url(50721, Some("")), "http://127.0.0.1:50721/");
    }

    /// 生效管理密码：覆盖值非空优先，空覆盖与未覆盖回退配置值
    #[test]
    fn test_effective_management_password() {
        assert_eq!(
            effective_management_password(Some("cli-pass"), "cfg-pass").into_owned(),
            "cli-pass"
        );
        assert_eq!(
            effective_management_password(Some(""), "cfg-pass").into_owned(),
            "cfg-pass"
        );
        assert_eq!(
            effective_management_password(None, "cfg-pass").into_owned(),
            "cfg-pass"
        );
        assert_eq!(effective_management_password(None, "").into_owned(), "");
    }

    // ============ exchange 换取逻辑（解耦核心函数直测） ============

    const TEST_TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";
    /// 64 位十六进制 bootstrap 形态
    const TEST_BOOTSTRAP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// 读取响应体为 JSON（测试辅助）
    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("响应体应为 JSON")
    }

    #[tokio::test]
    async fn exchange_bootstrap_success() {
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "",
            false,
            ExchangeBody {
                bootstrap: Some(TEST_BOOTSTRAP.to_string()),
                password: None,
            },
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "token 响应必须 no-store"
        );
        let value = body_json(resp).await;
        assert_eq!(value["data"]["token"], TEST_TOKEN);
    }

    #[tokio::test]
    async fn exchange_bootstrap_failure_is_401_envelope() {
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "",
            false,
            ExchangeBody {
                bootstrap: Some("deadbeef".to_string()),
                password: None,
            },
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(resp).await;
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");
        assert!(
            !value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("deadbeef"),
            "错误信息不得回显秘钥内容"
        );
    }

    #[tokio::test]
    async fn exchange_rejects_both_and_neither() {
        // 双字段同现 → 400
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "",
            false,
            ExchangeBody {
                bootstrap: Some(TEST_BOOTSTRAP.to_string()),
                password: Some("pw".to_string()),
            },
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 均缺 → 400
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "",
            false,
            ExchangeBody::default(),
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn exchange_password_rejected_on_loopback() {
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "some-password",
            true,
            ExchangeBody {
                bootstrap: None,
                password: Some("some-password".to_string()),
            },
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(resp).await;
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");
    }

    #[tokio::test]
    async fn exchange_password_flow_on_non_loopback() {
        let make_body = || ExchangeBody {
            bootstrap: None,
            password: Some("cfg-pass".to_string()),
        };
        // 未配置 → 401 AUTH_NOT_CONFIGURED
        let resp = respond_exchange(TEST_TOKEN, TEST_BOOTSTRAP, None, "", false, make_body());
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(resp).await;
        assert_eq!(value["error"]["code"], "AUTH_NOT_CONFIGURED");

        // 配置 + 匹配 → 200（响应体含 token，且 no-store）
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "cfg-pass",
            false,
            make_body(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let value = body_json(resp).await;
        assert_eq!(value["data"]["token"], TEST_TOKEN);

        // 配置 + 不匹配 → 401（错误信息不回显密码）
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            None,
            "cfg-pass",
            false,
            ExchangeBody {
                bootstrap: None,
                password: Some("wrong-pass".to_string()),
            },
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(resp).await;
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");
        assert!(
            !value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cfg-pass"),
            "错误信息不得回显密码内容"
        );
    }

    #[tokio::test]
    async fn exchange_password_override_wins_over_config() {
        // 覆盖值匹配 → 200；配置值反而被拒绝
        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            Some("cli-pass"),
            "cfg-pass",
            false,
            ExchangeBody {
                bootstrap: None,
                password: Some("cli-pass".to_string()),
            },
        );
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = respond_exchange(
            TEST_TOKEN,
            TEST_BOOTSTRAP,
            Some("cli-pass"),
            "cfg-pass",
            false,
            ExchangeBody {
                bootstrap: None,
                password: Some("cfg-pass".to_string()),
            },
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ============ 鉴权中间件行为（端到端，oneshot 直连 Router） ============

    /// 构建仅含鉴权中间件与测试路由的迷你 Router（中间件只依赖 token）
    fn test_router() -> axum::Router {
        use axum::routing::{get, post};
        use std::sync::Arc;
        let state: Arc<str> = Arc::from(TEST_TOKEN);
        axum::Router::new()
            .route("/api/ping", get(|| async { "pong" }))
            .route("/ws/logs", get(|| async { "ws" }))
            // 旧 token 端点：路由仍存在与否不影响中间件豁免判定，此处用探针
            // 路由验证「不再豁免」（T0a-1 收口回归）
            .route("/api/auth/token", get(|| async { "leak" }))
            .route("/api/auth/exchange", post(|| async { "exchange" }))
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

    /// 旧 `GET /api/auth/token` 不再豁免：无 token / 错 token 均 401（T0a-1 回归）
    #[tokio::test]
    async fn auth_middleware_no_longer_exempts_token_endpoint() {
        use axum::http::Request;
        use tower::ServiceExt;

        let app = test_router();
        // 无 token → 401（收口前此处免鉴权裸发 token）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/auth/token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);

        // 错误 token → 401
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/token")
                    .header("X-Auth-Token", "wrong-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
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

    /// `POST /api/auth/exchange` 豁免：无 token 也放行（换取凭据由 handler 自行校验）；
    /// 旧 token 端点同路径形态已不豁免，豁免不扩大化
    #[tokio::test]
    async fn auth_middleware_exempts_exchange_endpoint() {
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        let app = test_router();
        // 无 token POST exchange → 中间件放行（探针路由 200，非 401）
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/exchange")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
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

    /// GET /api/repo/image 豁免：仓库导入 `<img>` 预览无法携带自定义头；
    /// POST /api/repo/fetch 等 JSON 代理仍需鉴权（豁免仅图片一端）
    #[tokio::test]
    async fn auth_middleware_repo_image_get_exempt() {
        use axum::http::{Method, Request, StatusCode};
        use tower::ServiceExt;

        let app = test_router();

        // GET 截图代理：无 token → 放行（路由层 405 只证明过了中间件）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/repo/image?url=https%3A%2F%2Fexample.com%2Fa.png")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);

        // JSON 代理仍需鉴权：无 token → 401
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/repo/fetch?url=https%3A%2F%2Fexample.com%2Findex.json")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 豁免不扩大化：同前缀不同路径仍需鉴权
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/repo/image")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
