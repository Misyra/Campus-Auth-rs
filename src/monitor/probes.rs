//! 三种探测实现：TCP race、HTTP 204/200、URL 内容匹配
//!
//! 三类探测为轻量无状态数据载体，各自提供 `run()` 并发执行所有目标并汇总为
//! [`ProbeOutcome`]。HTTP/URL 探测复用调用方传入的长生命周期 `reqwest::Client` 连接池。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use futures::future::{join_all, select_all};
use reqwest::Client;
use tokio::net::{TcpSocket, TcpStream};
use tracing::instrument;

/// 解析 "host:port" 为 (host, port)；非法格式返回 None
fn parse_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    // 去除 IPv6 方括号
    let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    Some((host, port))
}

/// TCP 连接（可选绑定本地出口 IP）
async fn tcp_connect(host: &str, port: u16, bind: Option<IpAddr>, timeout: Duration) -> std::io::Result<()> {
    match bind {
        Some(addr) => {
            let socket = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            socket.bind(SocketAddr::new(addr, 0))?;
            // TcpSocket::connect 仅接受已解析的 SocketAddr，需先异步 DNS 解析
            let mut addrs = tokio::net::lookup_host((host, port)).await?;
            let sockaddr = addrs.next().ok_or_else(|| {
                std::io::Error::other("DNS 解析无结果")
            })?;
            match tokio::time::timeout(timeout, socket.connect(sockaddr)).await {
                Ok(Ok(_stream)) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "tcp connect timeout",
                )),
            }
        }
        None => match tokio::time::timeout(timeout, TcpStream::connect((host, port))).await {
            Ok(Ok(_stream)) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "tcp connect timeout",
            )),
        },
    }
}

/// TCP 探测：Race 语义，任一目标连通即视为 `Pass`
pub struct TcpProbe;

impl TcpProbe {
    /// 并发连接所有目标，取首个成功。
    ///
    /// `bind_addr` 非空时将出站 socket 绑定到该本地 IP（用于指定网卡出口）。
    #[instrument(skip_all)]
    pub async fn run(
        targets: &[String],
        timeout: Duration,
        bind_addr: Option<IpAddr>,
    ) -> (ProbeOutcome, Vec<PerProbeDetail>) {
        if targets.is_empty() {
            return (ProbeOutcome::Disabled, Vec::new());
        }
        // 并发连接所有目标（race 语义），join_all 同时启动全部 future，
        // 每个目标独立超时，整体耗时 ≈ max(单目标耗时) 而非 sum
        let futs: Vec<_> = targets
            .iter()
            .map(|target| {
                let target = target.clone();
                Box::pin(async move {
                    let start = Instant::now();
                    let (success, err) = match parse_host_port(&target) {
                        Some((host, port)) => {
                            match tcp_connect(&host, port, bind_addr, timeout).await {
                                Ok(()) => (true, None),
                                Err(e) => (false, Some(e.to_string())),
                            }
                        }
                        None => (false, Some("非法地址格式".to_string())),
                    };
                    PerProbeDetail {
                        target,
                        success,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        http_status: None,
                        error: err,
                    }
                })
            })
            .collect();
        // Race 语义：第一个成功连接即视为通过，避免等待慢速失败
        let mut details = Vec::with_capacity(futs.len());
        let mut remaining = futs;
        loop {
            let (result, _idx, rest) = select_all(remaining).await;
            let success = result.success;
            details.push(result);
            remaining = rest;
            if success || remaining.is_empty() {
                break;
            }
        }
        let any_pass = details.iter().any(|d| d.success);
        let outcome = if any_pass {
            ProbeOutcome::Pass
        } else {
            ProbeOutcome::Fail
        };
        (outcome, details)
    }
}

/// HTTP 探测：状态码语义（204=Pass，200/3xx=Captive，其他/超时=Fail）
pub struct HttpProbe;

impl HttpProbe {
    /// 并发请求所有目标，乐观汇总。
    #[instrument(skip_all)]
    pub async fn run(
        client: &Client,
        targets: &[String],
        timeout: Duration,
    ) -> (ProbeOutcome, Vec<PerProbeDetail>) {
        if targets.is_empty() {
            return (ProbeOutcome::Disabled, Vec::new());
        }
        let futs = targets.iter().map(|t| probe_http_one(client, t, timeout));
        let results = join_all(futs).await;
        let (outcome, details) = summarize(results);
        (outcome, details)
    }
}

async fn probe_http_one(client: &Client, url: &str, timeout: Duration) -> (ProbeOutcome, PerProbeDetail) {
    let start = Instant::now();
    match client.get(url).timeout(timeout).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // 不读取 body，仅按状态码分类（客户端已配置 redirect=none）
            let outcome = match status {
                204 => ProbeOutcome::Pass,
                200 => ProbeOutcome::Captive,
                301..=308 => ProbeOutcome::Captive,
                _ => ProbeOutcome::Fail,
            };
            (
                outcome,
                PerProbeDetail {
                    target: url.to_string(),
                    success: matches!(outcome, ProbeOutcome::Pass),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    http_status: Some(status),
                    error: None,
                },
            )
        }
        Err(e) => (
            ProbeOutcome::Fail,
            PerProbeDetail {
                target: url.to_string(),
                success: false,
                elapsed_ms: start.elapsed().as_millis() as u64,
                http_status: None,
                error: Some(e.to_string()),
            },
        ),
    }
}

/// URL 标题探测：内容匹配语义（3xx=Captive，200 且内容匹配=Pass，否则 Fail）
pub struct UrlProbe;

impl UrlProbe {
    /// 并发请求所有目标，匹配预期响应内容。
    #[instrument(skip_all)]
    pub async fn run(
        client: &Client,
        targets: &[String],
        expected: &HashMap<String, String>,
        timeout: Duration,
    ) -> (ProbeOutcome, Vec<PerProbeDetail>) {
        if targets.is_empty() {
            return (ProbeOutcome::Disabled, Vec::new());
        }
        let futs = targets
            .iter()
            .map(|t| probe_url_one(client, t, expected, timeout));
        let results = join_all(futs).await;
        let (outcome, details) = summarize(results);
        (outcome, details)
    }
}

async fn probe_url_one(
    client: &Client,
    url: &str,
    expected: &HashMap<String, String>,
    timeout: Duration,
) -> (ProbeOutcome, PerProbeDetail) {
    let start = Instant::now();
    match client.get(url).timeout(timeout).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if (300..=399).contains(&status) {
                // 重定向即视为门户劫持
                return (
                    ProbeOutcome::Captive,
                    PerProbeDetail {
                        target: url.to_string(),
                        success: false,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        http_status: Some(status),
                        error: None,
                    },
                );
            }
            if status == 200 {
                // 读取 body（限长 64KB）做内容匹配
                let body = match resp.bytes().await {
                    Ok(bytes) => {
                        if bytes.len() > 64 * 1024 {
                            String::from_utf8_lossy(&bytes[..64 * 1024]).into_owned()
                        } else {
                            String::from_utf8_lossy(&bytes).into_owned()
                        }
                    }
                    Err(e) => {
                        tracing::warn!("读取 URL 探测响应体失败: {e}");
                        String::new()
                    }
                };
                let expected_str = expected.get(url).map(|s| s.as_str());
                let matched = match expected_str {
                    Some(exp) => body.contains(exp),
                    // 未配置预期字符串则视为通过
                    None => true,
                };
                let outcome = if matched {
                    ProbeOutcome::Pass
                } else {
                    ProbeOutcome::Captive
                };
                return (
                    outcome,
                    PerProbeDetail {
                        target: url.to_string(),
                        success: matched,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        http_status: Some(status),
                        error: None,
                    },
                );
            }
            (
                ProbeOutcome::Fail,
                PerProbeDetail {
                    target: url.to_string(),
                    success: false,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    http_status: Some(status),
                    error: None,
                },
            )
        }
        Err(e) => (
            ProbeOutcome::Fail,
            PerProbeDetail {
                target: url.to_string(),
                success: false,
                elapsed_ms: start.elapsed().as_millis() as u64,
                http_status: None,
                error: Some(e.to_string()),
            },
        ),
    }
}

/// 汇总多目标结果（乐观优先级：Pass > Captive > Fail）
fn summarize(results: Vec<(ProbeOutcome, PerProbeDetail)>) -> (ProbeOutcome, Vec<PerProbeDetail>) {
    let pass = results
        .iter()
        .any(|(o, _)| matches!(o, ProbeOutcome::Pass));
    let captive = results
        .iter()
        .any(|(o, _)| matches!(o, ProbeOutcome::Captive));
    let outcome = if pass {
        ProbeOutcome::Pass
    } else if captive {
        ProbeOutcome::Captive
    } else {
        ProbeOutcome::Fail
    };
    let details = results.into_iter().map(|(_, d)| d).collect();
    (outcome, details)
}

/// 探测类别标识（用于 `decision::evaluate` 的输入配对）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// TCP 裸连接探测
    Tcp,
    /// HTTP 204/200 探测
    Http,
    /// URL 标题内容匹配探测
    Url,
}

/// 单类探测结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// 连通 / 204 / 内容匹配
    Pass,
    /// 200 / 3xx（门户劫持）
    Captive,
    /// 超时或连接失败
    Fail,
    /// 该类别未启用
    Disabled,
}

/// 单个目标的探测细节（用于日志与调试）
#[derive(Debug, Clone)]
pub struct PerProbeDetail {
    /// 目标地址
    pub target: String,
    /// 是否成功
    pub success: bool,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// HTTP 状态码（仅 HTTP/URL 探测）
    pub http_status: Option<u16>,
    /// 失败原因
    pub error: Option<String>,
}
