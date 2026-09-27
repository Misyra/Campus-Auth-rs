//! 入站 Host 头校验：DNS rebinding 防线
//!
//! 威胁模型：恶意网页把攻击者域名经 DNS rebinding 解析到 127.0.0.1 后，
//! 页面到本地 Web 服务之间是「同源」请求——CORS 与浏览器 PNA 均不适用，
//! 可直接读走免鉴权的 `/api/auth/token`（见 `web::auth` 模块说明）接管
//! 全部 API。CORS 只防「跨源读」，防不住同源化；Host 头校验是该路径上
//! 的独立防线：rebinding 请求的 Host 恒为攻击者域名，与回环白名单
//! 不匹配即被拒。
//!
//! 策略按实际绑定地址决定（[`HostPolicy::from_bind_ip`]）：
//! - 绑定回环（默认）：强制校验；
//! - 绑定非回环（Docker / LAN 显式暴露）：跳过——暴露面由绑定决定，
//!   且此场景下浏览器经局域网 IP / 域名访问，Host 恒非回环，强制校验
//!   会直接打死合法访问。
//!
//! 兼容性：Host 头缺失（HTTP/1.0、oneshot 单测）放行——rebinding 攻击
//! 必然携带攻击者域名 Host，缺失不构成该威胁路径。

use std::net::IpAddr;

use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Host 校验策略（由监听绑定地址推导）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostPolicy {
    /// 绑定回环地址：Host 必须指向回环（127.0.0.1 / localhost / ::1）
    LoopbackOnly,
    /// 绑定非回环地址：跳过校验（显式暴露场景）
    AllowAny,
}

impl HostPolicy {
    /// 按监听绑定地址推导策略
    pub fn from_bind_ip(ip: IpAddr) -> Self {
        if ip.is_loopback() {
            Self::LoopbackOnly
        } else {
            Self::AllowAny
        }
    }
}

/// 判断 Host 头取值是否指向回环（端口忽略：合法访问端口任意，只看主机部分）
fn is_trusted_loopback_host(raw: &str) -> bool {
    let host = raw.trim();
    if host.is_empty() {
        return false;
    }
    // Host 形态（RFC 7230）：IPv4/注册名可带 ":port"；IPv6 必须方括号
    // 包裹（"[::1]:port"）。裸 IPv6 回环不合规范，宽容接受（不扩大攻击面）。
    if host == "::1" || host.eq_ignore_ascii_case("0:0:0:0:0:0:0:1") {
        return true;
    }
    if let Some(rest) = host.strip_prefix('[') {
        let Some((inner, tail)) = rest.split_once(']') else {
            return false;
        };
        // 方括号后只允许空或 ":数字端口"（拒绝 "[::1].evil.com" 类伪装）
        let tail_ok = tail.is_empty()
            || (tail.len() > 1
                && tail.starts_with(':')
                && tail[1..].bytes().all(|b| b.is_ascii_digit()));
        return tail_ok && (inner == "::1" || inner.eq_ignore_ascii_case("0:0:0:0:0:0:0:1"));
    }
    // 冒号后是纯数字才视为端口分隔（避免把裸 IPv6 地址切坏）
    let host_part = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    // 精确等值匹配：严禁前缀/后缀匹配（localhost.evil.com / 127.0.0.1.evil.com）
    host_part == "127.0.0.1" || host_part == "::1" || host_part.eq_ignore_ascii_case("localhost")
}

/// 拒绝非回环 Host 的 403 响应（信封格式与 `web::auth` 的 401 一致）
fn forbidden_host() -> Response {
    let body = r#"{"error":{"code":"FORBIDDEN_HOST","message":"Host 头不在允许范围（127.0.0.1 / localhost / ::1），疑似 DNS rebinding，已拦截"}}"#;
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Host 校验中间件主体（`web::build_router` 经 `middleware::from_fn` 挂载，
/// 策略随闭包捕获）
pub(crate) async fn validate_host(
    policy: HostPolicy,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if policy == HostPolicy::LoopbackOnly {
        if let Some(host) = req.headers().get(header::HOST) {
            if !is_trusted_loopback_host(host.to_str().unwrap_or("")) {
                return forbidden_host();
            }
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::{Router, middleware};
    use tower::ServiceExt; // oneshot

    /// Host 取值判定：回环各形态（含大小写、端口、IPv6 方括号）放行
    #[test]
    fn test_trusted_loopback_hosts() {
        for trusted in [
            "127.0.0.1:50721",
            "127.0.0.1",
            "localhost:50721",
            "LOCALHOST",
            "[::1]:50721",
            "[::1]",
            "[0:0:0:0:0:0:0:1]:50721",
            "::1",
        ] {
            assert!(is_trusted_loopback_host(trusted), "应放行: {trusted}");
        }
    }

    /// Host 取值判定：攻击者域名及其伪装形态（前缀/后缀/子域陷阱）全部拒绝
    #[test]
    fn test_untrusted_hosts() {
        for evil in [
            "attacker.com:50721",
            "attacker.com",
            "localhost.evil.com:50721",
            "localhost.evil.com",
            "127.0.0.1.evil.com",
            "127.0.0.2:50721",
            "evil-localhost:50721",
            "localhost:50721.evil.com",
            "[::1].evil.com",
            "",
            "   ",
        ] {
            assert!(!is_trusted_loopback_host(evil), "应拒绝: {evil}");
        }
    }

    /// 策略推导：回环绑定强制校验，非回环绑定（Docker/LAN）跳过
    #[test]
    fn test_policy_from_bind_ip() {
        assert_eq!(
            HostPolicy::from_bind_ip(IpAddr::from([127, 0, 0, 1])),
            HostPolicy::LoopbackOnly
        );
        assert_eq!(
            HostPolicy::from_bind_ip(IpAddr::from([127, 5, 5, 5])),
            HostPolicy::LoopbackOnly
        );
        assert_eq!(
            HostPolicy::from_bind_ip("::1".parse::<IpAddr>().unwrap()),
            HostPolicy::LoopbackOnly
        );
        assert_eq!(
            HostPolicy::from_bind_ip(IpAddr::from([0, 0, 0, 0])),
            HostPolicy::AllowAny
        );
        assert_eq!(
            HostPolicy::from_bind_ip(IpAddr::from([192, 168, 1, 5])),
            HostPolicy::AllowAny
        );
        assert_eq!(
            HostPolicy::from_bind_ip("::".parse::<IpAddr>().unwrap()),
            HostPolicy::AllowAny
        );
    }

    /// 挂载校验层的最小路由，返回探针路由的状态码
    async fn probe(policy: HostPolicy, host: Option<&str>) -> u16 {
        let app = Router::new()
            .route("/probe", get(|| async { "ok" }))
            .layer(middleware::from_fn(move |req, next| {
                validate_host(policy, req, next)
            }));
        let mut builder = Request::builder().uri("/probe");
        if let Some(h) = host {
            builder = builder.header(header::HOST, h);
        }
        let resp = app
            .oneshot(builder.body(Body::empty()).expect("构造请求"))
            .await
            .expect("oneshot");
        resp.status().as_u16()
    }

    /// 回环绑定策略：攻击者域名 403，回环 Host 与缺失 Host 放行
    #[tokio::test]
    async fn test_loopback_policy_rejects_foreign_host() {
        assert_eq!(
            probe(HostPolicy::LoopbackOnly, Some("attacker.com:50721")).await,
            403
        );
        assert_eq!(
            probe(HostPolicy::LoopbackOnly, Some("localhost.evil.com")).await,
            403
        );
        assert_eq!(
            probe(HostPolicy::LoopbackOnly, Some("127.0.0.1:50721")).await,
            200
        );
        assert_eq!(
            probe(HostPolicy::LoopbackOnly, Some("[::1]:50721")).await,
            200
        );
        // 缺失 Host（HTTP/1.0、oneshot 测试基建）放行
        assert_eq!(probe(HostPolicy::LoopbackOnly, None).await, 200);
    }

    /// 非回环绑定策略（Docker/LAN）：不校验，任意 Host 放行
    #[tokio::test]
    async fn test_allow_any_policy_skips_validation() {
        assert_eq!(
            probe(HostPolicy::AllowAny, Some("attacker.com:50721")).await,
            200
        );
        assert_eq!(
            probe(HostPolicy::AllowAny, Some("box.lan:50721")).await,
            200
        );
    }

    /// 拒绝响应使用统一错误信封（code=FORBIDDEN_HOST）
    #[tokio::test]
    async fn test_rejection_body_uses_error_envelope() {
        let app = Router::new()
            .route("/probe", get(|| async { "ok" }))
            .layer(middleware::from_fn(move |req, next| {
                validate_host(HostPolicy::LoopbackOnly, req, next)
            }));
        let req = Request::builder()
            .uri("/probe")
            .header(header::HOST, "attacker.com:50721")
            .body(Body::empty())
            .expect("构造请求");
        let resp = app.oneshot(req).await.expect("oneshot");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("读取响应体");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON 信封");
        assert_eq!(value["error"]["code"], "FORBIDDEN_HOST");
    }
}
