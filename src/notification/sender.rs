//! 通道发送：reqwest 出站 POST + 系统通知（os 通道）+ 重试退避（注入时钟可测）+ 错误脱敏
//!
//! # 信任口径
//!
//! 通道 URL / 凭据均为用户在配置里的显式选择（与 `proxy_url` / 管理密码同级
//! 明文，暴露面由配置接口的 token 鉴权兜底），因此**不做 SSRF 钉扎**——本机
//! / 内网 webhook（自建 ntfy、企业微信机器人等）是合法目标，与 AI 模块的
//! `client_for` 同口径：本地/私网基址强制直连（系统代理会把 `127.0.0.1` 转给
//! 代理并回 502），公网基址沿用系统代理。
//!
//! # os 通道特殊口径
//!
//! 系统通知（`NotificationChannel::Os`）走 `notify-rust`（Windows toast /
//! macOS 通知中心 / Linux freedesktop 三平台统一实现）：调用属**阻塞 IO**
//! （winrt/freedesktop 可能百 ms 级），包在 [`tokio::task::spawn_blocking`]
//! 里执行，外层再套共字段 `timeout_seconds` 超时——超时即放弃该次通知。
//! 失败**静默且不重试**：无桌面会话 / 权限拒绝 / SSH 环境下重发同样失败，
//! 系统通知重试无意义，与 webhook 等远端通道的瞬时网络错误语义不同。
//! 安卓构建不编译 `notify-rust`（无桌面通知基础设施，依赖门为
//! `cfg(not(target_os = "android"))`），os 通道发送恒失败静默。
//!
//! # 错误脱敏红线
//!
//! reqwest 的错误 Display 会携带完整请求 URL——Bark 的 device_key、Telegram
//! 的 bot_token 都在 **URL 路径**里，webhook 的密钥可能出现在 query。因此
//! 本模块绝不把 `reqwest::Error` 原文透出，一律经 `describe_reqwest_error`
//! 归类为不含 URL 的短语；需要展示 URL 的地方（测试端点响应）经
//! [`sanitize_url`] 去 query/userinfo/fragment。

use std::time::Duration;

use crate::config::NotificationChannel;

use super::template::{self, MessageContent};

/// Bark 官方服务器（通道 server 留空时使用）
const BARK_DEFAULT_SERVER: &str = "https://api.day.app";
/// Server酱 Turbo 推送端点前缀
const SERVERCHAN_BASE: &str = "https://sctapi.ftqq.com";
/// 出站请求 User-Agent
const USER_AGENT: &str = concat!("Campus-Auth/", env!("CARGO_PKG_VERSION"));
/// 系统通知应用名（Windows toast / macOS 通知中心 / Linux freedesktop 展示用）
///
/// 仅非安卓的生产构建存在（测试构建走 `os_stub` 桩、安卓构建无 `notify-rust`，
/// 均不触达）
#[cfg(all(not(test), not(target_os = "android")))]
const OS_NOTIFICATION_APP_NAME: &str = "认证喵";

/// 测试桩：隔离 `notify-rust` 真发调用点（本机/CI 弹真通知会打扰用户）
///
/// 仅测试构建存在，生产构建零开销。进程级共享状态，触碰桩的测试须先取
/// [`OS_STUB_LOCK`] 串行化。
#[cfg(test)]
mod os_stub {
    use std::sync::Mutex;

    /// 注入的发送结果（`None` = 成功，`Some` = 返回该错误文本）
    pub static RESULT: Mutex<Option<String>> = Mutex::new(None);
    /// 注入的阻塞时长（毫秒；模拟 winrt/freedesktop 的阻塞 IO，供超时测试）
    pub static DELAY_MS: Mutex<u64> = Mutex::new(0);
    /// 已发生的（标题, 正文）调用记录
    pub static CALLS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    /// 重置桩并注入本次测试的返回结果与阻塞时长
    pub fn configure(result: Option<String>, delay_ms: u64) {
        *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = result;
        *DELAY_MS.lock().unwrap_or_else(|e| e.into_inner()) = delay_ms;
        CALLS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// 桩共享状态串行化锁（见 `os_stub` doc）
///
/// 用 tokio 异步锁：测试须跨 `.await` 持有它，`std::sync::MutexGuard`
/// 会触发 clippy 的 await_holding_lock 拒绝
#[cfg(test)]
static OS_STUB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 阻塞发送系统通知（`notify-rust` 平台实现；调用方须在 `spawn_blocking` 内调用）
///
/// 测试构建替换为桩（见 `os_stub`）：真弹桌面通知会打扰用户且依赖桌面会话，
/// 测试只覆盖分派 / 超时 / 失败静默路径。失败返回脱敏短语（notify-rust 的
/// 错误文本不含远端凭据，但保持与其他通道一致的短语风格）。
fn show_os_notification_blocking(title: &str, body: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let delay = *os_stub::DELAY_MS.lock().unwrap_or_else(|e| e.into_inner());
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        os_stub::CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((title.to_string(), body.to_string()));
        return os_stub::RESULT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map_or(Ok(()), Err);
    }
    #[cfg(all(not(test), not(target_os = "android")))]
    {
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .appname(OS_NOTIFICATION_APP_NAME)
            .show()
            .map(|_handle| ())
            .map_err(|e| format!("系统通知发送失败: {e}"))
    }
    // 安卓构建无 notify-rust（依赖门一致）：发送恒失败静默，与无桌面会话同口径
    #[cfg(all(not(test), target_os = "android"))]
    {
        let _ = (title, body);
        Err("当前平台不支持系统通知".to_string())
    }
}

/// 判断 URL 基址是否为本地/私网目标（与 ai::llm 同口径：这类目标强制直连）
fn is_local_base_url(url: &str) -> bool {
    let host = match url::Url::parse(url) {
        Ok(parsed) => parsed.host_str().unwrap_or("").to_string(),
        Err(_) => return false,
    };
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Err(_) => false,
    }
}

/// 出站客户端：本地/私网基址强制直连，公网基址沿用系统代理（见模块 doc）
fn client_for(url: &str, timeout: Duration) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT);
    let builder = if is_local_base_url(url) {
        builder.no_proxy()
    } else {
        builder
    };
    builder
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))
}

/// 把 reqwest 错误归类为不含 URL/凭据的短语（脱敏红线，见模块 doc）
fn describe_reqwest_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "请求超时".to_string()
    } else if error.is_connect() {
        "连接失败（检查网络或目标地址）".to_string()
    } else if error.is_body() || error.is_decode() {
        "响应读取失败".to_string()
    } else {
        "请求失败".to_string()
    }
}

/// URL 脱敏：去掉 query / fragment / userinfo（密钥常放 query 或 userinfo）
///
/// 路径保留（自建 webhook 的路由信息仍有排障价值，且展示对象就是配置者本人）；
/// 解析失败时返回占位串而非原文（原文可能就是凭据本身）。
pub fn sanitize_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            if !parsed.username().is_empty() {
                // set_username/set_password 返回 Result：失败仅影响展示非安全性，
                // 忽略即可（query/fragment 已剥除）
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
            }
            parsed.to_string()
        }
        Err(_) => "[无效 URL]".to_string(),
    }
}

/// 错误文本最终出口：其中出现的任何裸 URL token 都替换为脱敏版本
///
/// 错误短语本身已由 `describe_reqwest_error` 脱敏；本函数兜底拼接了 URL
/// 的场景（如测试端点把「目标 URL + 错误」组合展示）。
pub fn sanitize_error_text(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token.starts_with("http://") || token.starts_with("https://") {
                sanitize_url(token)
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 单次发送（不重试）。错误信息已脱敏。
pub async fn send_once(
    channel: &NotificationChannel,
    message: &MessageContent,
    timeout: Duration,
) -> Result<(), String> {
    match channel {
        NotificationChannel::Webhook {
            url,
            headers,
            body_template,
            ..
        } => {
            let body = template::render_webhook_body(body_template, message);
            let client = client_for(url, timeout)?;
            let mut request = client.post(url);
            // 自定义头（含密钥头）；未指定 content-type 时补 JSON 默认值
            let mut has_content_type = false;
            for header in headers {
                let name = header.name.trim();
                if name.is_empty() {
                    continue;
                }
                if name.eq_ignore_ascii_case("content-type") {
                    has_content_type = true;
                }
                request = request.header(name, &header.value);
            }
            if !has_content_type {
                request = request.header("content-type", "application/json");
            }
            let response = request
                .body(body)
                .send()
                .await
                .map_err(|e| describe_reqwest_error(&e))?;
            finish_response(response).await
        }
        NotificationChannel::Bark {
            server, device_key, ..
        } => {
            let base = server.trim();
            let base = if base.is_empty() {
                BARK_DEFAULT_SERVER
            } else {
                base
            };
            let url = format!("{}/{}", base.trim_end_matches('/'), device_key.trim());
            let client = client_for(&url, timeout)?;
            let response = client
                .post(&url)
                .json(&template::bark_body(message))
                .send()
                .await
                .map_err(|e| describe_reqwest_error(&e))?;
            finish_response(response).await
        }
        NotificationChannel::Serverchan { send_key, .. } => {
            let url = format!("{}/{}.send", SERVERCHAN_BASE, send_key.trim());
            let client = client_for(&url, timeout)?;
            let response = client
                .post(&url)
                .form(&template::serverchan_form(message))
                .send()
                .await
                .map_err(|e| describe_reqwest_error(&e))?;
            finish_response(response).await
        }
        NotificationChannel::Telegram {
            bot_token, chat_id, ..
        } => {
            let url = format!(
                "https://api.telegram.org/bot{}/sendMessage",
                bot_token.trim()
            );
            let client = client_for(&url, timeout)?;
            let response = client
                .post(&url)
                .json(&template::telegram_body(message, chat_id.trim()))
                .send()
                .await
                .map_err(|e| describe_reqwest_error(&e))?;
            finish_response(response).await
        }
        NotificationChannel::Os { .. } => {
            // 系统通知调用是阻塞 IO（winrt/freedesktop 可能百 ms 级），移入专用
            // 阻塞线程池执行，绝不占用异步工作线程；外层 timeout 让共字段
            // timeout_seconds 生效——超时即放弃该次通知（阻塞线程无法强杀，
            // 自然结束后其结果随 JoinHandle 被丢弃）
            let (title, body) = template::os_body(message);
            let handle =
                tokio::task::spawn_blocking(move || show_os_notification_blocking(&title, &body));
            match tokio::time::timeout(timeout, handle).await {
                Ok(Ok(result)) => result,
                // 阻塞任务 panic：join 错误只留短语（不透出 panic 现场细节）
                Ok(Err(join_err)) => Err(format!("系统通知任务异常终止: {join_err}")),
                Err(_) => Err("系统通知超时".to_string()),
            }
        }
    }
}

/// 非 2xx 响应 → 错误（附短响应体片段便于排障；响应体由接收方生成，不含本端凭据）
async fn finish_response(response: reqwest::Response) -> Result<(), String> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let snippet = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect::<String>();
    Err(format!(
        "服务端返回 {status}{}",
        if snippet.is_empty() {
            String::new()
        } else {
            format!(": {snippet}")
        }
    ))
}

/// 重试退避策略（来自 `NotificationSettings`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 重试次数（不含首次；0 = 只发一次）
    pub max_attempts: u32,
    /// 首次重试前等待的基准秒数，之后逐次翻倍（base → 2·base → 4·base …）
    pub base_interval: Duration,
}

impl RetryPolicy {
    /// 由设置节构造（超界值已在 `NotificationSettings::validate` 拦截，此处兜底钳制）
    pub fn from_settings(settings: &crate::config::NotificationSettings) -> Self {
        Self {
            max_attempts: settings.retry_max_attempts,
            base_interval: Duration::from_secs(settings.retry_base_interval_seconds.max(1) as u64),
        }
    }

    /// 测试发送用：不重试，一次真发快速反馈
    pub fn single_shot() -> Self {
        Self {
            max_attempts: 0,
            base_interval: Duration::from_secs(1),
        }
    }
}

/// 重试间隔时钟（注入以测退避节奏；生产为 [`RealClock`]）
#[async_trait::async_trait]
pub trait RetryClock: Send + Sync {
    /// 等待指定时长
    async fn sleep(&self, delay: Duration);
}

/// 生产时钟：真实 tokio 睡眠
pub struct RealClock;

#[async_trait::async_trait]
impl RetryClock for RealClock {
    async fn sleep(&self, delay: Duration) {
        tokio::time::sleep(delay).await;
    }
}

/// 带重试退避的发送：首次 + `max_attempts` 次重试，间隔 base → 2·base → 4·base
///
/// 系统通知（`NotificationChannel::Os`）**不参与重试**：失败多为环境性的
/// （无桌面会话 / 权限拒绝 / SSH 环境），重发同样失败，与 webhook 等远端
/// 通道的瞬时网络错误语义不同（见模块 doc「os 通道特殊口径」）。
///
/// 全部尝试失败时返回**最后一次**的脱敏错误；发送方对失败只 warn 不上抛
/// （失败静默口径见 `super` 模块 doc）。
pub async fn send_with_retry(
    channel: &NotificationChannel,
    message: &MessageContent,
    policy: &RetryPolicy,
    clock: &dyn RetryClock,
) -> Result<(), String> {
    let timeout = Duration::from_secs(channel.timeout_seconds().max(1) as u64);
    // os 通道固定单次：RetryPolicy 的退避节奏对它无意义
    let total = if matches!(channel, NotificationChannel::Os { .. }) {
        1
    } else {
        policy.max_attempts.saturating_add(1)
    };
    let mut last_error = String::new();
    for attempt in 0..total {
        match send_once(channel, message, timeout).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = err;
                if attempt + 1 < total {
                    // 指数退避：第 1 次重试等 base，第 2 次 2·base，第 3 次 4·base …
                    let delay = policy.base_interval.saturating_mul(1u32 << attempt.min(5));
                    clock.sleep(delay).await;
                }
            }
        }
    }
    Err(sanitize_error_text(&last_error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationHeader;
    use std::sync::{Arc, Mutex};

    /// 记录退避节奏、零等待的测试时钟
    #[derive(Default)]
    struct RecordedClock {
        delays_ms: Mutex<Vec<u64>>,
    }

    #[async_trait::async_trait]
    impl RetryClock for RecordedClock {
        async fn sleep(&self, delay: Duration) {
            self.delays_ms
                .lock()
                .unwrap()
                .push(delay.as_millis() as u64);
        }
    }

    fn webhook(url: &str) -> NotificationChannel {
        NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: "测试通道".into(),
            enabled: true,
            url: url.into(),
            headers: vec![NotificationHeader {
                name: "X-Token".into(),
                value: "secret-token-value".into(),
            }],
            body_template: String::new(),
            timeout_seconds: 2,
        }
    }

    fn message() -> MessageContent {
        MessageContent {
            event_type: "login_failed".into(),
            summary: "登录失败".into(),
            detail: "档案 p1".into(),
            profile: "p1".into(),
            time: "2026-01-01 08:00:00".into(),
        }
    }

    /// sanitize_url：query / fragment / userinfo 全剥掉；无效 URL 不回显原文
    #[test]
    fn test_sanitize_url() {
        assert_eq!(
            sanitize_url("https://api.day.app/KEY123?level=active"),
            "https://api.day.app/KEY123"
        );
        assert_eq!(
            sanitize_url("https://user:pass@example.com/hook"),
            "https://example.com/hook"
        );
        assert_eq!(sanitize_url("not a url at all"), "[无效 URL]");
    }

    /// sanitize_error_text：文本里裸露的 URL token 被替换为脱敏版本（query 内密钥消失）
    #[test]
    fn test_sanitize_error_text() {
        let out = sanitize_error_text("目标 https://example.com/hook?key=TOPSECRET 返回 500");
        assert!(!out.contains("TOPSECRET"), "{out}");
        assert!(!out.contains("?key="), "{out}");
        assert!(out.contains("https://example.com/hook"), "{out}");
    }

    /// describe_reqwest_error：错误短语不含 URL（reqwest Display 会带 URL，绝不透出）
    #[test]
    fn test_describe_reqwest_error_hides_url() {
        // no_proxy 与生产 client_for 的本地口径一致（走系统代理会把 127.0.0.1:1
        // 的连接拒绝变成代理的 502 响应，而非 reqwest 连接错误）
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let err = tokio::runtime::Runtime::new().unwrap().block_on(async {
            client
                .post("http://127.0.0.1:1/hook")
                .timeout(Duration::from_millis(500))
                .send()
                .await
                .unwrap_err()
        });
        let described = describe_reqwest_error(&err);
        assert!(!described.contains("127.0.0.1"), "{described}");
        assert!(!described.contains("hook"), "{described}");
        assert!(
            described.contains("连接失败") || described == "请求超时",
            "{described}"
        );
    }

    /// 重试退避节奏：3 次重试 = base → 2·base → 4·base；全程失败返回脱敏错误
    #[tokio::test]
    async fn test_retry_backoff_schedule() {
        let clock = Arc::new(RecordedClock::default());
        let policy = RetryPolicy {
            max_attempts: 3,
            base_interval: Duration::from_millis(500),
        };
        // 打不通的本地端口：每次尝试都连接失败（快）
        let channel = webhook("http://127.0.0.1:1/hook");
        let result = send_with_retry(&channel, &message(), &policy, clock.as_ref()).await;
        assert!(result.is_err(), "坏目标必须失败");
        assert_eq!(
            *clock.delays_ms.lock().unwrap(),
            vec![500, 1000, 2000],
            "指数退避 base → 2·base → 4·base"
        );
        // 最终错误不含 URL 与自定义头凭据
        let err = result.unwrap_err();
        assert!(!err.contains("secret-token-value"), "{err}");
        assert!(!err.contains("127.0.0.1"), "{err}");
    }

    /// max_attempts = 0：只发一次，零退避
    #[tokio::test]
    async fn test_retry_single_shot_no_delays() {
        let clock = Arc::new(RecordedClock::default());
        let channel = webhook("http://127.0.0.1:1/hook");
        let result = send_with_retry(
            &channel,
            &message(),
            &RetryPolicy::single_shot(),
            clock.as_ref(),
        )
        .await;
        assert!(result.is_err());
        assert!(clock.delays_ms.lock().unwrap().is_empty());
    }

    /// 本地/私网基址判定：与 ai::llm 同口径
    #[test]
    fn test_is_local_base_url() {
        assert!(is_local_base_url("http://127.0.0.1:8080/hook"));
        assert!(is_local_base_url("http://192.168.1.5/ntfy"));
        assert!(is_local_base_url("http://localhost:9000"));
        assert!(is_local_base_url("http://[::1]:8080"));
        assert!(!is_local_base_url("https://api.day.app"));
        assert!(!is_local_base_url("https://sctapi.ftqq.com"));
    }

    // ============ os 通道（系统通知；桩隔离真发调用，见 os_stub） ============

    /// os 通道基线构造
    fn os_channel(timeout_seconds: u32) -> NotificationChannel {
        NotificationChannel::Os {
            id: "os-1".into(),
            name: "桌面提醒".into(),
            enabled: true,
            timeout_seconds,
        }
    }

    /// 成功路径：走 spawn_blocking 分派，标题/正文按 os_body 形状送达桩
    #[tokio::test]
    async fn test_os_channel_dispatch_success() {
        let _guard = OS_STUB_LOCK.lock().await;
        os_stub::configure(None, 0);
        let result = send_with_retry(
            &os_channel(5),
            &message(),
            &RetryPolicy::single_shot(),
            &RealClock,
        )
        .await;
        assert!(result.is_ok(), "桩默认成功: {result:?}");
        let calls = os_stub::CALLS.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 1, "恰好一次阻塞调用");
        assert_eq!(calls[0].0, "登录失败", "标题取 summary");
        assert_eq!(calls[0].1, "档案 p1", "正文取 detail（未超长不裁剪）");
    }

    /// 失败静默：桩注入错误（模拟无桌面会话/权限拒绝），返回 Err 由上层 warn
    /// 留痕——不 panic、不重试（见 os 通道特殊口径）
    #[tokio::test]
    async fn test_os_channel_failure_is_silent_error_without_retry() {
        let _guard = OS_STUB_LOCK.lock().await;
        os_stub::configure(Some("无桌面会话".into()), 0);
        // 即使重试策略给足 3 次，os 通道也只发一次（RecordedClock 零延迟记录）
        let clock = Arc::new(RecordedClock::default());
        let policy = RetryPolicy {
            max_attempts: 3,
            base_interval: Duration::from_millis(500),
        };
        let result = send_with_retry(&os_channel(5), &message(), &policy, clock.as_ref()).await;
        assert!(result.is_err(), "桩注入失败必须以 Err 透出");
        assert!(
            result.unwrap_err().contains("无桌面会话"),
            "错误文本透传供 warn 留痕"
        );
        assert!(
            clock.delays_ms.lock().unwrap().is_empty(),
            "os 通道失败不重试：无退避等待"
        );
        assert_eq!(
            os_stub::CALLS.lock().unwrap().len(),
            1,
            "只调用一次阻塞发送"
        );
    }

    /// 超时放弃：阻塞桩注入 2s 延迟、通道超时 1s，外层 timeout 生效返回超时错误
    #[tokio::test]
    async fn test_os_channel_timeout_abandons_send() {
        let _guard = OS_STUB_LOCK.lock().await;
        os_stub::configure(None, 2000);
        let started = std::time::Instant::now();
        let result = send_with_retry(
            &os_channel(1),
            &message(),
            &RetryPolicy::single_shot(),
            &RealClock,
        )
        .await;
        let elapsed = started.elapsed();
        assert!(result.is_err(), "超时必须放弃该次通知");
        assert!(result.unwrap_err().contains("超时"), "错误标注超时语义");
        assert!(
            elapsed < Duration::from_millis(1500),
            "1 秒超时即返回，不等阻塞调用自然结束（实际 {elapsed:?}）"
        );
    }
}
