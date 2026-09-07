//! 认证门户地址检测：未认证时请求明文探测地址并跟随 302 找到真门户
//!
//! 入口为 `POST /api/monitor/detect-portal`（见 [`crate::web::routes::monitor::detect_portal`]），
//! 供认证地址输入框旁的“自动检测”按钮调用：
//!
//! - 已在线时探测直通 204，无劫持可抓——必须先退出校园网登录再检测；
//! - 未认证时网关劫持探测请求：返回 3xx（`Location` 指向真门户）或直接 200 吐登录页
//!   （无跳转可取，只能提示用户手动复制地址栏）；
//! - 检测目标固定为监测配置中的 `http_targets + url_targets`（内置 generate_204 类明文
//!   地址），不接受客户端传参，无 SSRF 面；结果仅填入表单，由用户确认保存，不自动落盘。

use std::collections::HashMap;
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

/// `POST /api/monitor/detect-portal` 的业务负载
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
        futs.push(probe_target(&client, url.clone(), None, http_timeout));
    }
    for url in url_targets {
        futs.push(probe_target(
            &client,
            url.clone(),
            url_expected.get(url),
            url_timeout,
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
async fn probe_target(
    client: &reqwest::Client,
    url: String,
    expected: Option<&String>,
    timeout: Duration,
) -> TargetOutcome {
    let mut current = url;
    let mut followed = false;
    for _ in 0..MAX_REDIRECT_HOPS {
        let resp = match client.get(&current).timeout(timeout).send().await {
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
            let next = loc.and_then(|loc| {
                current.parse::<url::Url>().ok().and_then(|base| {
                    base.join(loc).ok().and_then(|next| {
                        let s = next.to_string();
                        (matches!(next.scheme(), "http" | "https")
                            && next.host_str().is_some_and(|h| !h.is_empty()))
                        .then_some(s)
                    })
                })
            });
            match next {
                Some(next) => {
                    current = next;
                    followed = true;
                    continue;
                }
                // Location 缺失/非法：首跳为劫持证据但无地址；链中则保留已拿到的地址
                None => {
                    return if followed {
                        TargetOutcome::Redirect(current)
                    } else {
                        TargetOutcome::DirectCaptive
                    };
                }
            }
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

    /// 302 相对 Location → 按当前 URL join 后 Found
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
