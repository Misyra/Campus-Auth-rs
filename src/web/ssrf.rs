//! SSRF 防护：私网 IP 判定 + DNS 解析钉扎（pin）+ 逐跳重定向校验
//!
//! 旧实现的 TOCTOU 缺口：先 `lookup_host` 校验解析结果，再让 reqwest
//! 发起请求——reqwest 内部会**二次解析**，攻击者控制的权威 DNS 可在两次
//! 解析之间切换 IP（先返回公网 IP 通过校验，再返回 169.254.169.254 等
//! 内网地址命中实际连接），完全绕过校验。
//!
//! 本模块的防护方式：
//! 1. 解析域名并校验全部 IP 为公网地址；
//! 2. 通过 `ClientBuilder::resolve(host, ip)` 把域名钉扎到已校验的 IP，
//!    reqwest 不再自行解析；
//! 3. 禁用自动重定向，手动跟随（最多 5 跳）并对每一跳重新校验，
//!    防止"公网 URL 302 → 内网地址"的二次跳转攻击。

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::http::header;

/// 最大重定向跟随跳数
const MAX_REDIRECTS: usize = 5;

/// 判断 IP 是否属于私有/保留地址段（全端点统一规则）
///
/// 覆盖：回环、未指定、组播、广播、RFC1918 私有段、IPv4 链路本地、
/// CGNAT（100.64.0.0/10）、基准测试段（198.18.0.0/15）、文档示例段、
/// 240.0.0.0/4 保留高地址段、IPv4-mapped IPv6（解包后按 IPv4 规则判定）、
/// IPv6 ULA（fc00::/7）、IPv6 链路本地（fe80::/10）、discard-only、
/// benchmarking 与文档示例前缀。
pub fn is_restricted_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_restricted_ipv4(v4),
        IpAddr::V6(v6) => {
            // IPv4-mapped IPv6（::ffff:a.b.c.d）：先解包成 IPv4 再按 V4 规则判定。
            // 否则 ::ffff:127.0.0.1、::ffff:169.254.169.254 等映射地址既不命中
            // V6 的回环/链路本地判定，也不经过 V4 规则，可直接绕过校验。
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_restricted_ipv4(v4);
            }
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                // fe80::/10 链路本地地址：内网可寻址，必须拦截
                || v6.is_unicast_link_local()
                // 100::/64 discard-only（RFC 6666）：不可作为公网目标
                || (segments[0] == 0x0100
                    && segments[1..].iter().all(|segment| *segment == 0))
                // 2001:2::/48 benchmarking（RFC 5180）
                || (segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0)
                // 2001:db8::/32 documentation（RFC 3849）
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

/// IPv4 保留段判定（IPv4-mapped IPv6 解包后同样走此规则）
fn is_restricted_ipv4(v4: std::net::Ipv4Addr) -> bool {
    v4.is_private()
        || v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_multicast()
        || v4.is_link_local()
        || v4.is_broadcast()
        // 0.0.0.0/8 "this network"：除 UNSPECIFIED 外其余地址标准库不会自动拦截
        || (std::net::Ipv4Addr::new(0, 0, 0, 0)..=std::net::Ipv4Addr::new(0, 255, 255, 255))
            .contains(&v4)
        // 100.64.0.0/10 CGNAT 运营商级 NAT 段：标准库无判定，显式区间检查
        || (std::net::Ipv4Addr::new(100, 64, 0, 0)..=std::net::Ipv4Addr::new(100, 127, 255, 255))
            .contains(&v4)
        // TEST-NET 文档示例地址，不应被 SSRF 端点视作公网目标
        || (std::net::Ipv4Addr::new(192, 0, 2, 0)..=std::net::Ipv4Addr::new(192, 0, 2, 255))
            .contains(&v4)
        || (std::net::Ipv4Addr::new(198, 51, 100, 0)
            ..=std::net::Ipv4Addr::new(198, 51, 100, 255))
            .contains(&v4)
        || (std::net::Ipv4Addr::new(203, 0, 113, 0)
            ..=std::net::Ipv4Addr::new(203, 0, 113, 255))
            .contains(&v4)
        // 198.18.0.0/15 网络基准测试段：保留地址，正常业务不应访问
        || (std::net::Ipv4Addr::new(198, 18, 0, 0)..=std::net::Ipv4Addr::new(198, 19, 255, 255))
            .contains(&v4)
        // 240.0.0.0/4 保留高地址段（含 255.255.255.255；broadcast 已在上方覆盖）
        || (std::net::Ipv4Addr::new(240, 0, 0, 0)..=std::net::Ipv4Addr::BROADCAST)
            .contains(&v4)
}

/// 解析域名并校验全部结果为公网地址，返回钉扎用地址列表
///
/// IP 字面量直接校验；域名解析失败或任一结果命中私网段即拒绝。
async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_restricted_ip(ip) {
            return Err(format!("禁止访问内网/保留地址: {host}"));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| format!("DNS 解析失败，拒绝访问: {host}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("DNS 解析无结果: {host}"));
    }
    for addr in &addrs {
        if is_restricted_ip(addr.ip()) {
            return Err(format!("禁止访问内网/保留地址: {host}"));
        }
    }
    Ok(addrs)
}

/// 构建钉扎到指定 IP 的客户端（禁用自动重定向，由调用方逐跳校验）
///
/// `proxy` 为可选的代理 URL（如 `http://127.0.0.1:7890`）：设置后请求经代理
/// 转发。注意此场景下实际连接目标是代理地址（本机），目标主机的公网校验
/// 仍由 [`resolve_public`] 在发起前完成——代理侧的出口 IP 由代理自身决定。
fn pinned_client(
    host: &str,
    addr: SocketAddr,
    timeout: Duration,
    ua: &str,
    proxy: Option<&str>,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(ua)
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, addr);
    if let Some(p) = proxy {
        // 代理地址来自用户配置（updater.proxy_url），非法值转错误而非 panic：
        // 此处 panic 会直接砸掉当次请求任务（连接重置而非 400）
        builder = builder.proxy(reqwest::Proxy::all(p).map_err(|e| format!("代理 URL 非法: {e}"))?);
    }
    builder
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))
}

/// SSRF 安全的 GET 请求：校验 scheme → DNS 解析校验并钉扎 → 手动跟随重定向
///
/// 每一跳重定向都会重新解析并校验目标地址；返回最终响应与最终 URL
///（调用方可能需要判断重定向后的地址）。
pub async fn secure_get(
    url: &str,
    timeout: Duration,
    ua: &str,
) -> Result<(reqwest::Response, String), String> {
    secure_get_proxied(url, timeout, ua, None).await
}

/// [`secure_get`] 的代理版本：请求经 `proxy`（如 `http://127.0.0.1:7890`）转发。
///
/// 用于仓库任务/背景图等下载场景（国内访问 GitHub raw 常需代理）。
/// 校验流程与 [`secure_get`] 完全一致。
pub async fn secure_get_proxied(
    url: &str,
    timeout: Duration,
    ua: &str,
    proxy: Option<&str>,
) -> Result<(reqwest::Response, String), String> {
    let mut current = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        let parsed = url::Url::parse(&current).map_err(|e| format!("无效的 URL: {e}"))?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(format!("不支持的 URL 协议: {scheme}，仅支持 http/https"));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| "URL 缺少主机名".to_string())?
            .to_string();
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| "URL 缺少端口".to_string())?;

        let addrs = resolve_public(&host, port).await?;
        // 逐个尝试解析地址（首个连通即用），全部失败才报错
        let mut last_err = String::from("无可用地址");
        let mut response: Option<reqwest::Response> = None;
        for addr in &addrs {
            // 代理非法等构造失败直接返回（换地址重试无意义，同代理必同错）
            let client = pinned_client(&host, *addr, timeout, ua, proxy)?;
            match client.get(&current).send().await {
                Ok(resp) => {
                    response = Some(resp);
                    break;
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        let Some(resp) = response else {
            return Err(format!("请求失败: {last_err}"));
        };

        // 手动跟随重定向：Location 相对当前 URL 解析后重新走完整校验
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "重定向缺少 Location".to_string())?;
            let next = parsed
                .join(loc)
                .map_err(|e| format!("重定向地址无效: {e}"))?;
            current = next.to_string();
            continue;
        }
        return Ok((resp, current));
    }
    Err(format!("重定向超过 {MAX_REDIRECTS} 跳，放弃请求"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_is_restricted_rejects_private_and_reserved_ipv4() {
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::BROADCAST)));
    }

    #[test]
    fn test_is_restricted_rejects_reserved_ipv6() {
        assert!(is_restricted_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_restricted_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
        assert!(is_restricted_ip(IpAddr::V6(Ipv6Addr::new(
            0xfc00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(is_restricted_ip(IpAddr::V6("fe80::1".parse().unwrap())));
        assert!(is_restricted_ip(IpAddr::V6("ff02::1".parse().unwrap())));
        assert!(is_restricted_ip(IpAddr::V6("100::".parse().unwrap())));
        assert!(is_restricted_ip(IpAddr::V6("2001:2::1".parse().unwrap())));
        assert!(is_restricted_ip(IpAddr::V6("2001:db8::1".parse().unwrap())));
    }

    /// IPv4-mapped IPv6 必须解包后按 IPv4 规则拦截，
    /// 否则 ::ffff:127.0.0.1 等映射地址可绕过校验直连内网
    #[test]
    fn test_is_restricted_rejects_ipv4_mapped_ipv6() {
        assert!(is_restricted_ip(IpAddr::V6(
            "::ffff:127.0.0.1".parse().unwrap()
        )));
        assert!(is_restricted_ip(IpAddr::V6(
            "::ffff:169.254.169.254".parse().unwrap()
        )));
        assert!(is_restricted_ip(IpAddr::V6(
            "::ffff:10.0.0.1".parse().unwrap()
        )));
        assert!(is_restricted_ip(IpAddr::V6(
            "::ffff:192.168.1.1".parse().unwrap()
        )));
        assert!(is_restricted_ip(IpAddr::V6(
            "::ffff:100.64.1.1".parse().unwrap()
        )));
        // 映射公网地址不受影响
        assert!(!is_restricted_ip(IpAddr::V6(
            "::ffff:8.8.8.8".parse().unwrap()
        )));
    }

    /// CGNAT（100.64.0.0/10）与基准测试段（198.18.0.0/15）：保留段边界与外侧
    #[test]
    fn test_is_restricted_rejects_cgnat_and_benchmark_ranges() {
        // CGNAT 段内（含首尾边界）受限
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(
            100, 127, 255, 255
        ))));
        // 基准测试段内（含首尾边界）受限
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 5))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 0))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(
            198, 19, 255, 255
        ))));
        // 段外侧的公网地址不受影响
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(
            100, 63, 255, 255
        ))));
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))));
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(
            198, 17, 255, 255
        ))));
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(198, 20, 0, 0))));
    }

    #[test]
    fn test_is_restricted_rejects_documentation_ipv4_ranges() {
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))));
        assert!(is_restricted_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    #[test]
    fn test_is_restricted_allows_public_addresses() {
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_restricted_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_restricted_ip(IpAddr::V6(
            "2606:4700:4700::1111".parse().unwrap()
        )));
        assert!(!is_restricted_ip(IpAddr::V6(
            "2001:4860:4860::8888".parse().unwrap()
        )));
    }
}

#[cfg(test)]
mod proxy_client_tests {
    use super::*;
    use std::net::SocketAddr;

    fn loopback() -> SocketAddr {
        "127.0.0.1:9".parse().unwrap()
    }

    /// 非法代理 URL 转错误而非 panic（代理值来自用户配置，曾 expect 直接砸掉请求任务）
    #[test]
    fn pinned_client_rejects_invalid_proxy_url() {
        for bad in ["http://", "http://[::1", "not a url!!!", "http://a b"] {
            let err = pinned_client(
                "example.com",
                loopback(),
                Duration::from_secs(1),
                "t",
                Some(bad),
            )
            .unwrap_err();
            assert!(err.contains("代理 URL 非法"), "{bad}: {err}");
        }
    }

    /// 合法代理正常构造（不发起连接）
    #[test]
    fn pinned_client_accepts_valid_proxy() {
        assert!(
            pinned_client(
                "example.com",
                loopback(),
                Duration::from_secs(1),
                "t",
                Some("http://127.0.0.1:7890")
            )
            .is_ok()
        );
        assert!(
            pinned_client("example.com", loopback(), Duration::from_secs(1), "t", None).is_ok()
        );
    }
}
