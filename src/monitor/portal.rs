//! 认证门户地址检测：未认证时请求明文探测地址并跟随 302 找到真门户
//!
//! 当前由 `POST /api/monitor/test-redirect` 在拉起可见浏览器前调用，用于快速识别
//! “已经正常联网”的场景；不再把重定向候选地址返回前端或自动填入配置：
//!
//! - 已在线时探测直通 204，无劫持可抓——必须先退出校园网登录再检测；
//! - 未认证时网关劫持探测请求：返回 3xx（`Location` 指向真门户）或直接 200 吐登录页
//!   （无跳转可取，只能提示用户手动复制地址栏）；
//! - 检测目标固定为监测配置中的 `http_targets + url_targets`（内置 generate_204 类明文
//!   地址），不接受客户端传参；重定向目标由网关下发，逐跳做最小目的地址校验
//!   （仅拒环回/链路本地/通配地址，内网门户放行，见 [`probe_target`]）。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::future::join_all;
use reqwest::redirect::Policy;
use serde::Serialize;

/// 单路请求超时上限（秒）：各目标并行请求，总耗时约等于最慢一路
const PER_TARGET_TIMEOUT_SECS: u64 = 10;
/// 手动跟随重定向的最大跳数（与 SSRF 下载路径的 5 跳同口径）
const MAX_REDIRECT_HOPS: usize = 5;
/// 响应体读取上限（URL 探测内容比对用，与 probes 的 64KB 同口径）
const MAX_BODY_BYTES: usize = 64 * 1024;

/// 门户检测结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalDetectStatus {
    /// 抓到跳转并解析出门户地址
    Found,
    /// 探测地址直通 204：当前已在线，请先退出登录再检测
    Online,
    /// 劫持存在但无可用跳转（200 直吐登录页 / 跳转地址非法）：需手动复制
    CaptiveNoRedirect,
    /// 全部目标不可达：可能已断网或探测地址被阻断
    Offline,
}

/// HTTP 门户预检的内部业务负载
#[derive(Debug, Clone, Serialize)]
pub struct PortalDetectResult {
    /// 结论
    pub status: PortalDetectStatus,
    /// 候选门户地址（仅 Found 时有值）
    pub portal_url: Option<String>,
    /// 人类可读说明（前端 toast 直显）
    pub message: String,
    /// 实际请求的探测地址（排障用）
    pub checked: Vec<String>,
}

/// 单个探测目标的判定
#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetOutcome {
    /// 跟随 3xx 链拿到最终地址
    Redirect(String),
    /// 劫持证据但无可用跳转（200 非预期内容 / 缺 Location / 地址非法 / 跳数耗尽）
    DirectCaptive,
    /// 204（HTTP 探测）或 200 + 内容匹配（URL 探测）：在线
    Online,
    /// 连接失败 / 超时 / 无意义状态码
    Failed,
}

/// 检测认证门户地址
///
/// `http_targets` 走 204 语义（204=在线，200/3xx=劫持）；`url_targets` 走内容语义
/// （3xx=劫持，200 按 `url_expected` 内容匹配区分在线与劫持）。各目标并行请求，
/// 按“劫持跳转 > 直接劫持 > 在线 > 不可达”优先级汇总。
pub async fn detect_portal(
    http_targets: &[String],
    http_timeout: Duration,
    url_targets: &[String],
    url_expected: &HashMap<String, String>,
    url_timeout: Duration,
    disable_proxy: bool,
) -> PortalDetectResult {
    let mut builder = reqwest::Client::builder().redirect(Policy::none());
    if disable_proxy {
        // 检测直连：captive 态下系统代理不可达，走代理会把劫持误判为不可达
        builder = builder.no_proxy();
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            return PortalDetectResult {
                status: PortalDetectStatus::Offline,
                portal_url: None,
                message: format!("检测客户端构建失败: {e}"),
                checked: Vec::new(),
            };
        }
    };

    let http_timeout = http_timeout.min(Duration::from_secs(PER_TARGET_TIMEOUT_SECS));
    let url_timeout = url_timeout.min(Duration::from_secs(PER_TARGET_TIMEOUT_SECS));
    let mut futs = Vec::with_capacity(http_targets.len() + url_targets.len());
    for url in http_targets {
        futs.push(probe_target(
            &client,
            url.clone(),
            None,
            http_timeout,
            disable_proxy,
        ));
    }
    for url in url_targets {
        futs.push(probe_target(
            &client,
            url.clone(),
            url_expected.get(url),
            url_timeout,
            disable_proxy,
        ));
    }
    let checked: Vec<String> = http_targets
        .iter()
        .chain(url_targets.iter())
        .cloned()
        .collect();
    let outcomes = join_all(futs).await;

    // 优先级：跳转 > 直接劫持 > 在线 > 不可达
    if let Some(url) = outcomes.iter().find_map(|o| match o {
        TargetOutcome::Redirect(u) => Some(u.clone()),
        _ => None,
    }) {
        return PortalDetectResult {
            status: PortalDetectStatus::Found,
            portal_url: Some(url),
            message: "已检测到认证地址".to_string(),
            checked,
        };
    }
    if outcomes
        .iter()
        .any(|o| matches!(o, TargetOutcome::DirectCaptive))
    {
        return PortalDetectResult {
            status: PortalDetectStatus::CaptiveNoRedirect,
            portal_url: None,
            message: "检测到认证门户，但门户直接返回登录页（无跳转地址），请在浏览器打开任意网页后手动复制地址栏填入".to_string(),
            checked,
        };
    }
    if outcomes.iter().any(|o| matches!(o, TargetOutcome::Online)) {
        return PortalDetectResult {
            status: PortalDetectStatus::Online,
            portal_url: None,
            message: "当前网络已在线（探测直通），请先退出校园网登录后再检测".to_string(),
            checked,
        };
    }
    PortalDetectResult {
        status: PortalDetectStatus::Offline,
        portal_url: None,
        message: "无法连接探测目标：可能已断网或探测地址被阻断，请检查网络后重试".to_string(),
        checked,
    }
}

/// 请求单个探测目标并手动跟随重定向（最多 [`MAX_REDIRECT_HOPS`] 跳）
///
/// `expected` 为 URL 探测的期望内容（HTTP 204 探测传 `None`）。
///
/// 首跳（未跟随跳转）的语义沿用 204/内容探测：204=在线，200=门户直吐登录页；
/// 一旦跟随过至少一跳，最终 URL 即候选门户地址——落地页状态码不再重要
/// （302 链后多为 200 登录页），后续请求失败也不丢弃已拿到的 Location。
///
/// 跳转目标逐跳校验（MON-4）：首跳 URL 来自用户配置，属信任边界内不强校验；
/// 跨主机跳转解析后仅拒绝环回/链路本地/通配地址（不拦 RFC1918——校园门户普遍部署
/// 在内网段），域名钉扎到已校验 IP 防 reqwest 二次解析 TOCTOU；同主机跳转
/// （门户自身相对路径跳转）不引入新目的地址，免校验。
async fn probe_target(
    client: &reqwest::Client,
    url: String,
    expected: Option<&String>,
    timeout: Duration,
    disable_proxy: bool,
) -> TargetOutcome {
    let mut current = url;
    let mut followed = false;
    // 钉扎客户端：跨主机跳转目标为域名时，替换为解析校验后钉扎的 client
    let mut pinned: Option<reqwest::Client> = None;
    for _ in 0..MAX_REDIRECT_HOPS {
        let hop_client = pinned.as_ref().unwrap_or(client);
        let resp = match hop_client.get(&current).timeout(timeout).send().await {
            Ok(r) => r,
            Err(_) => {
                // 已跟随过跳转：Location 本身就是候选门户地址，不因后续请求失败而丢弃
                return if followed {
                    TargetOutcome::Redirect(current)
                } else {
                    TargetOutcome::Failed
                };
            }
        };
        let status = resp.status().as_u16();
        if (300..=399).contains(&status) {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok());
            let next_url = loc.and_then(|loc| {
                current.parse::<url::Url>().ok().and_then(|base| {
                    base.join(loc).ok().filter(|next| {
                        matches!(next.scheme(), "http" | "https")
                            && next.host_str().is_some_and(|h| !h.is_empty())
                    })
                })
            });
            let next_url = match next_url {
                Some(u) => u,
                // Location 缺失/非法：首跳为劫持证据但无地址；链中则保留已拿到的地址
                None => {
                    return if followed {
                        TargetOutcome::Redirect(current)
                    } else {
                        TargetOutcome::DirectCaptive
                    };
                }
            };
            // 跟随前校验目标地址。被拒时跳转本身已是劫持证据，按既有语义以
            // 最后安全地址收尾，不请求危险目标
            let same_host = current
                .parse::<url::Url>()
                .ok()
                .and_then(|base| {
                    base.host_str()
                        .map(|h| h.eq_ignore_ascii_case(next_url.host_str().unwrap_or("")))
                })
                .unwrap_or(false);
            if !same_host {
                match ensure_redirect_target(&next_url, disable_proxy).await {
                    Ok(pinned_client) => pinned = pinned_client,
                    Err(()) => {
                        return if followed {
                            TargetOutcome::Redirect(current)
                        } else {
                            TargetOutcome::DirectCaptive
                        };
                    }
                }
            }
            current = next_url.to_string();
            followed = true;
            continue;
        }
        if followed {
            // 落地：最终 URL 即候选门户地址（302 链后的 204 极罕见，同样视为找到）
            return TargetOutcome::Redirect(current);
        }
        // 首跳直达：最终 URL 仍是公网探测地址而非门户，无可用跳转
        if status == 204 {
            return TargetOutcome::Online;
        }
        if status == 200 {
            if let Some(exp) = expected {
                // URL 探测：内容匹配=在线，不匹配=门户直吐登录页
                return if read_body_limited(resp).await.contains(exp.as_str()) {
                    TargetOutcome::Online
                } else {
                    TargetOutcome::DirectCaptive
                };
            }
            // HTTP 204 探测返回 200：门户直吐登录页（Android captive 事实标准）
            return TargetOutcome::DirectCaptive;
        }
        // 其余状态码（4xx/5xx 等）无判定意义
        return TargetOutcome::Failed;
    }
    // 跳数耗尽但一路有 Location：最后 URL 仍是候选
    TargetOutcome::Redirect(current)
}

/// 重定向跳转目标的最小目的地址校验（MON-4）
///
/// 仅拒绝环回、链路本地与通配地址——**不拦 RFC1918 私网**：校园门户普遍部署在
/// 内网段，全私网拦截会破坏核心场景（判定规则独立于 `web::ssrf` 的全私网
/// 口径）。域名先经系统解析器展开、对全部候选地址判定（防「IP 字面量白名单」
/// 被 DNS 解析绕过），并把域名钉扎到首个已校验地址（`ClientBuilder::resolve`，
/// 与 `web::ssrf` 同手法），杜绝 reqwest 二次解析 TOCTOU。IP 字面量无解析面，
/// 返回 `Ok(None)` 沿用共享客户端。解析失败或无结果一律拒绝跟随。
async fn ensure_redirect_target(
    url: &url::Url,
    disable_proxy: bool,
) -> Result<Option<reqwest::Client>, ()> {
    let host = url.host_str().ok_or(())?;
    let port = url.port_or_known_default().unwrap_or(80);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_blocked_redirect_ip(&ip) {
            Err(())
        } else {
            Ok(None)
        };
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ())?
        .collect();
    if !all_addrs_allowed(&addrs) {
        return Err(());
    }
    build_pinned_client(host, addrs[0], disable_proxy).map(Some)
}

/// 按已校验地址构造钉扎客户端（域名 → 指定地址）
///
/// 与 `web::ssrf` 同手法：`ClientBuilder::resolve` 让 reqwest 不再自行解析域名，
/// 杜绝「校验用 A 地址、连接用 B 地址」的 TOCTOU。独立成函数是为了能在无真实 DNS
/// 的前提下锁定「解析通过后确实按该地址连接」这条放行分支（B5）。
fn build_pinned_client(
    host: &str,
    addr: SocketAddr,
    disable_proxy: bool,
) -> Result<reqwest::Client, ()> {
    let mut builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .resolve(host, addr);
    if disable_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| ())
}

/// 全部解析结果的放行判定：空结果（解析无输出）或任一地址被拒即不放行。
///
/// 独立成纯函数便于直接锁定“放行/拒绝”决策，无需依赖真实 DNS。
fn all_addrs_allowed(addrs: &[SocketAddr]) -> bool {
    !addrs.is_empty() && !addrs.iter().any(|a| is_blocked_redirect_ip(&a.ip()))
}

/// 环回、链路本地与通配地址判定（最小集合，口径见 [`ensure_redirect_target`]）：
/// IPv4 127.0.0.0/8、169.254.0.0/16、0.0.0.0；IPv6 ::1、fe80::/10、::；
/// IPv4-mapped IPv6（::ffff:a.b.c.d）解包后按 IPv4 规则判定。
///
/// 0.0.0.0/:: 与 [`crate::web::ssrf::is_restricted_ip`] 同口径：二者在部分平台
/// 会被内核解析为回环（Linux 上 connect 0.0.0.0 即访问本机），属同一类目的地址，
/// 不能因“Windows 上不可用”而放行。
fn is_blocked_redirect_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.is_loopback() || v4.is_link_local() || v4.is_unspecified();
            }
            v6.is_loopback() || v6.is_unicast_link_local() || v6.is_unspecified()
        }
    }
}

/// 限长读取响应体（URL 探测内容比对用，超长截断）
async fn read_body_limited(resp: reqwest::Response) -> String {
    let mut body = Vec::with_capacity(8 * 1024);
    let mut resp = resp;
    while body.len() < MAX_BODY_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(MAX_BODY_BYTES - body.len());
                body.extend_from_slice(&chunk[..take]);
                if body.len() >= MAX_BODY_BYTES {
                    break;
                }
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 本地回环测试服务：按序返回给定响应（读掉请求头后回包并关连接）
    async fn serve_responses(responses: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(body.as_bytes()).await;
            }
        });
        format!("http://{addr}/generate_204")
    }

    fn http_targets_of(url: &str) -> Vec<String> {
        vec![url.to_string()]
    }

    /// 302 绝对 Location → Found
    #[tokio::test]
    async fn test_absolute_redirect_found() {
        let url = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://10.1.1.55/login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Found);
        assert_eq!(r.portal_url.as_deref(), Some("http://10.1.1.55/login"));
    }

    /// 302 相对 Location → 按当前 URL join 后 Found（同主机跳转免目的地址校验）
    #[tokio::test]
    async fn test_relative_redirect_joined() {
        let url = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: /login.html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Found);
        let portal = r.portal_url.expect("应有候选地址");
        assert!(
            portal.ends_with("/login.html"),
            "相对跳转应拼接主机: {portal}"
        );
    }

    /// MON-4：跨主机跳转指向环回 IP → 拒绝跟随，按直接劫持收尾。
    /// 目标用 127.0.0.2（同属 127/8 环回段）而非 127.0.0.1——mock 服务器本身
    /// 就在 127.0.0.1，同主机跳转按设计免校验
    #[tokio::test]
    async fn test_redirect_to_loopback_blocked() {
        let url = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.2:1/blocked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::CaptiveNoRedirect);
        assert!(r.portal_url.is_none());
    }

    /// MON-4：跨主机跳转指向链路本地地址（云元数据）→ 拒绝跟随
    #[tokio::test]
    async fn test_redirect_to_link_local_blocked() {
        let url = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::CaptiveNoRedirect);
        assert!(r.portal_url.is_none());
    }

    /// MON-4：跳转目标为解析到环回的域名（localhost）→ 解析后判定，拒绝跟随
    #[tokio::test]
    async fn test_redirect_to_loopback_hostname_blocked() {
        let url = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://localhost:1/blocked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::CaptiveNoRedirect);
        assert!(r.portal_url.is_none());
    }

    /// MON-4 单元：环回/链路本地/通配地址判定（IPv4/IPv6/IPv4-mapped；RFC1918 放行）
    #[test]
    fn test_blocked_redirect_ip() {
        assert!(is_blocked_redirect_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"::1".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"fe80::1".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_blocked_redirect_ip(
            &"::ffff:169.254.1.1".parse().unwrap()
        ));
        // B2 补漏：通配地址与 web::ssrf 同口径拒绝——Linux 上 connect 0.0.0.0/:: 即
        // 访问本机，等同环回，不能因 Windows 上 connect 会直接失败而放行
        assert!(is_blocked_redirect_ip(&"0.0.0.0".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"::".parse().unwrap()));
        assert!(is_blocked_redirect_ip(&"::ffff:0.0.0.0".parse().unwrap()));
        // 内网门户与公网地址放行
        assert!(!is_blocked_redirect_ip(&"10.1.1.55".parse().unwrap()));
        assert!(!is_blocked_redirect_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_blocked_redirect_ip(&"fc00::1".parse().unwrap()));
        assert!(!is_blocked_redirect_ip(&"8.8.8.8".parse().unwrap()));
    }

    /// MON-4 单元：解析结果放行判定（B5——此前只测拒绝，放行分支无锁定）
    ///
    /// 决策抽成纯函数后可直接锁定，不依赖真实 DNS。
    #[test]
    fn test_all_addrs_allowed() {
        let public: SocketAddr = "8.8.8.8:80".parse().unwrap();
        let internal: SocketAddr = "10.1.1.55:80".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let wildcard: SocketAddr = "0.0.0.0:80".parse().unwrap();
        let v6_loopback: SocketAddr = "[::1]:80".parse().unwrap();
        // 放行分支：内网门户与公网地址（含 ULA）均放行
        assert!(all_addrs_allowed(&[public]));
        assert!(all_addrs_allowed(&[internal, public]));
        assert!(all_addrs_allowed(&["[fc00::1]:443".parse().unwrap()]));
        // 拒绝分支：空结果（解析无输出）与「任一地址越界」——双栈混排时不得只判首个
        assert!(!all_addrs_allowed(&[]));
        assert!(!all_addrs_allowed(&[loopback]));
        assert!(!all_addrs_allowed(&[wildcard]));
        assert!(!all_addrs_allowed(&[internal, loopback]));
        assert!(!all_addrs_allowed(&[public, v6_loopback]));
    }

    /// MON-4 单元：`ensure_redirect_target` 的 IP 字面量分支（无解析面，可离线锁定）
    ///
    /// IPv6 字面量在本函数中走 `lookup_host` 分支，其拒绝由
    /// [`test_blocked_redirect_ip`] 与解析失败共同保证，不在此重复。
    #[tokio::test]
    async fn test_ensure_redirect_target_ip_literal() {
        let parse = |s: &str| url::Url::parse(s).unwrap();
        // 放行分支：内网门户 IP 字面量 → Ok(None)（沿用共享客户端，不钉扎）
        assert!(matches!(
            ensure_redirect_target(&parse("http://10.1.1.55/login"), true).await,
            Ok(None)
        ));
        // 拒绝分支：环回 / 链路本地 / 通配一律 Err，不请求危险目标
        for s in [
            "http://127.0.0.1:8080/login",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/login",
        ] {
            assert!(
                ensure_redirect_target(&parse(s), true).await.is_err(),
                "{s} 应被拒绝跟随"
            );
        }
    }

    /// MON-4 单元：B5——放行分支的钉扎客户端确实按「已校验地址」连接
    ///
    /// 手工把 mock 服务的回环地址当作某域名的解析结果喂给钉扎构造器，绕开真实 DNS：
    /// 这样「解析通过 → 钉扎 → 按该地址连接」整条链路可离线锁定，避免回归退化为
    /// 静默拒绝跟随。地址放行判定由 [`test_all_addrs_allowed`] 单独锁定，此处不重复。
    #[tokio::test]
    async fn test_build_pinned_client_connects_to_resolved_addr() {
        let url = serve_responses(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ])
        .await;
        // serve_responses 返回 http://127.0.0.1:<port>/generate_204
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let client = build_pinned_client("portal.example.com", addr, true)
            .expect("已校验地址应能构造出钉扎客户端");
        let resp = client
            .get(format!(
                "http://portal.example.com:{}/generate_204",
                addr.port()
            ))
            .send()
            .await
            .expect("钉扎客户端应连到已校验地址而非重新解析域名");
        assert_eq!(resp.status().as_u16(), 200);
    }

    /// 204 直通 → Online（提示先退出登录）
    #[tokio::test]
    async fn test_204_means_online() {
        let url = serve_responses(vec![
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Online);
        assert!(r.portal_url.is_none());
        assert!(r.message.contains("退出"));
    }

    /// 200 直吐登录页（无跳转）→ CaptiveNoRedirect
    #[tokio::test]
    async fn test_200_without_redirect_is_captive_no_redirect() {
        let url = serve_responses(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nlogin page!",
        ])
        .await;
        let r = detect_portal(
            &http_targets_of(&url),
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::CaptiveNoRedirect);
        assert!(r.portal_url.is_none());
    }

    /// URL 探测：200 + 内容匹配 → Online；内容不匹配 → CaptiveNoRedirect
    #[tokio::test]
    async fn test_url_probe_content_match() {
        let ok = serve_responses(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nsuccess",
        ])
        .await;
        let mut expected = HashMap::new();
        expected.insert(ok.clone(), "success".to_string());
        let r = detect_portal(
            &[],
            Duration::from_secs(5),
            &[ok],
            &expected,
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Online);

        let hijacked = serve_responses(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin",
        ])
        .await;
        let mut expected = HashMap::new();
        expected.insert(hijacked.clone(), "success".to_string());
        let r = detect_portal(
            &[],
            Duration::from_secs(5),
            &[hijacked],
            &expected,
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::CaptiveNoRedirect);
    }

    /// 全部目标不可达 → Offline
    #[tokio::test]
    async fn test_unreachable_is_offline() {
        let r = detect_portal(
            &["http://127.0.0.1:9/generate_204".to_string()],
            Duration::from_secs(2),
            &[],
            &HashMap::new(),
            Duration::from_secs(2),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Offline);
    }

    /// 跳转优先于直接劫持：多目标并行时有 Location 的胜出
    #[tokio::test]
    async fn test_redirect_wins_over_direct_captive() {
        let direct = serve_responses(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin",
        ])
        .await;
        let jumped = serve_responses(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://10.9.9.9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let r = detect_portal(
            &[direct, jumped],
            Duration::from_secs(5),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert_eq!(r.status, PortalDetectStatus::Found);
        assert_eq!(r.portal_url.as_deref(), Some("http://10.9.9.9/"));
    }
}
