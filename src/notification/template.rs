//! 消息模板：事件载荷 → 人话摘要 + 占位符渲染
//!
//! # 脱敏红线
//!
//! [`MessageContent`] 只承载事件白名单载荷里已有的字段（profile_id / 渠道 /
//! 原因短语 / 任务 id / 版本号，见 `events` 模块 doc），**不新增任何敏感来源**；
//! `detail` 由这些字段拼接成人话，凭据在结构上就进不来。webhook 自定义模板
//! 只做占位符替换，不做模板内取值，不存在注入面。

use crate::events::{AppEvent, EventEnvelope};

/// 渲染进消息模板的载荷（全部为脱敏摘要）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageContent {
    /// 事件类型标识（与 `AppEvent::kind()` 同值；测试消息为 `test`）
    pub event_type: String,
    /// 一句话摘要（如「登录失败」「网络已断开」）
    pub summary: String,
    /// 事件载荷的人话拼接（多行；不含凭据）
    pub detail: String,
    /// 关联档案（无关联时为 `-`）
    pub profile: String,
    /// 本地时间（`%Y-%m-%d %H:%M:%S`，用户感知时区）
    pub time: String,
}

impl MessageContent {
    /// 模板校验/预览用的样例载荷（`NotificationSettings::validate` 消费）
    pub fn sample() -> Self {
        Self {
            event_type: "login_failed".to_string(),
            summary: "登录失败".to_string(),
            detail: "档案 default · 渠道 browser · 第 1 次尝试失败：invalid_credential".to_string(),
            profile: "default".to_string(),
            time: "2026-01-01 08:00:00".to_string(),
        }
    }

    /// 测试发送端点的载荷
    pub fn test_message() -> Self {
        Self {
            event_type: "test".to_string(),
            summary: "认证喵测试通知".to_string(),
            detail: "这是一条测试消息，收到即表示该通道配置正确。".to_string(),
            profile: "-".to_string(),
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }
}

/// 事件封装 → 消息载荷（摘要/detail 全部来自事件白名单字段）
pub fn summarize(envelope: &EventEnvelope) -> MessageContent {
    let event = &envelope.event;
    let time = envelope
        .timestamp
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    // 每个分支产出 (summary, detail, profile)；detail 用「 · 」拼接人话短语
    let (summary, detail, profile) = match event {
        AppEvent::LoginSucceeded(s) => (
            "登录成功".to_string(),
            join_parts(&[
                Some(format!("档案 {}", s.profile_id)),
                Some(format!("渠道 {}", s.channel)),
                s.attempt.map(|n| format!("第 {n} 次尝试成功")),
            ]),
            s.profile_id.clone(),
        ),
        AppEvent::LoginFailed(s) => (
            "登录失败".to_string(),
            join_parts(&[
                Some(format!("档案 {}", s.profile_id)),
                Some(format!("渠道 {}", s.channel)),
                s.attempt.map(|n| format!("第 {n} 次尝试")),
                s.reason.as_deref().map(|r| format!("原因：{r}")),
            ]),
            s.profile_id.clone(),
        ),
        AppEvent::NetworkDown(s) => (
            "网络已断开".to_string(),
            join_parts(&[
                s.profile_id.as_deref().map(|p| format!("档案 {p}")),
                s.reason.as_deref().map(|r| format!("原因：{r}")),
            ]),
            s.profile_id.clone().unwrap_or_else(|| "-".to_string()),
        ),
        AppEvent::NetworkUp(s) => (
            "网络已恢复".to_string(),
            join_parts(&[
                s.profile_id.as_deref().map(|p| format!("档案 {p}")),
                s.reason.as_deref().map(|r| format!("说明：{r}")),
            ]),
            s.profile_id.clone().unwrap_or_else(|| "-".to_string()),
        ),
        AppEvent::TaskFinished(s) => (
            "任务执行成功".to_string(),
            join_parts(&[
                Some(format!("任务 {}", s.task_id)),
                s.task_kind.as_deref().map(|k| format!("类型 {k}")),
            ]),
            "-".to_string(),
        ),
        AppEvent::TaskFailed(s) => (
            "任务执行失败".to_string(),
            join_parts(&[
                Some(format!("任务 {}", s.task_id)),
                s.task_kind.as_deref().map(|k| format!("类型 {k}")),
                s.reason.as_deref().map(|r| format!("原因：{r}")),
            ]),
            "-".to_string(),
        ),
        AppEvent::UpdateAvailable(s) => (
            format!("发现新版本 {}", s.version),
            join_parts(&[s
                .current_version
                .as_deref()
                .map(|v| format!("当前版本 {v}"))]),
            "-".to_string(),
        ),
        AppEvent::EnvironmentReady(s) => (
            "Python 环境就绪".to_string(),
            join_parts(&[
                Some(if s.python_ready {
                    "Python 可用".to_string()
                } else {
                    "Python 不可用".to_string()
                }),
                Some(if s.uv_ready {
                    "uv 可用".to_string()
                } else {
                    "uv 不可用".to_string()
                }),
            ]),
            "-".to_string(),
        ),
        AppEvent::ProfileSwitched(s) => (
            "档案已切换".to_string(),
            join_parts(&[
                s.from_profile_id.as_deref().map(|p| format!("原档案 {p}")),
                Some(format!("现档案 {}", s.to_profile_id)),
            ]),
            s.to_profile_id.clone(),
        ),
    };
    MessageContent {
        event_type: event.kind().to_string(),
        summary,
        detail,
        profile,
        time,
    }
}

/// 用「 · 」拼接非空片段；全空时回退占位，避免空 detail
fn join_parts(parts: &[Option<String>]) -> String {
    let joined = parts
        .iter()
        .flatten()
        .map(String::as_str)
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if joined.is_empty() {
        "无详细信息".to_string()
    } else {
        joined
    }
}

/// 占位符替换：`{{event_type}}` `{{summary}}` `{{profile}}` `{{time}}` `{{detail}}`
///
/// 纯文本替换（不做模板内取值/表达式）；未知占位符原样保留。
pub fn render_placeholders(template: &str, message: &MessageContent) -> String {
    template
        .replace("{{event_type}}", &message.event_type)
        .replace("{{summary}}", &message.summary)
        .replace("{{profile}}", &message.profile)
        .replace("{{time}}", &message.time)
        .replace("{{detail}}", &message.detail)
}

/// webhook 默认 JSON 模板（用户模板留空时使用）
const DEFAULT_WEBHOOK_TEMPLATE: &str = r#"{"title": "{{summary}}", "body": "{{detail}}", "event": "{{event_type}}", "profile": "{{profile}}", "time": "{{time}}"}"#;

/// 渲染 webhook 请求体：用户模板非空用用户的，否则用内建默认模板
pub fn render_webhook_body(template: &str, message: &MessageContent) -> String {
    let tpl = if template.trim().is_empty() {
        DEFAULT_WEBHOOK_TEMPLATE
    } else {
        template
    };
    render_placeholders(tpl, message)
}

/// Bark 内建 payload：`{"title","body","group"}`
pub fn bark_body(message: &MessageContent) -> serde_json::Value {
    serde_json::json!({
        "title": message.summary,
        "body": message.detail,
        "group": "Campus-Auth",
    })
}

/// Server酱内建表单字段：`title` + `desp`（markdown 正文）
pub fn serverchan_form(message: &MessageContent) -> Vec<(String, String)> {
    vec![
        ("title".to_string(), message.summary.clone()),
        ("desp".to_string(), message.detail.clone()),
    ]
}

/// Telegram 内建 payload：`sendMessage` 的 `chat_id` + `text`
pub fn telegram_body(message: &MessageContent, chat_id: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "text": format!("{}\n{}", message.summary, message.detail),
    })
}

/// 系统通知正文长度上限（字符数；桌面通知正文过长会被折叠或截断）
const OS_BODY_MAX_CHARS: usize = 200;

/// 系统通知内容：`(标题, 正文)`
///
/// 标题取一句话摘要 `summary`，正文取人话拼接 `detail` 并裁剪到
/// `OS_BODY_MAX_CHARS` 字符——按字符边界截断（不切半多字节字符），
/// 截断时补省略号提示。
pub fn os_body(message: &MessageContent) -> (String, String) {
    let mut chars = message.detail.chars();
    let body: String = chars.by_ref().take(OS_BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        (message.summary.clone(), format!("{body}…"))
    } else {
        (message.summary.clone(), body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{LoginSummary, NetworkSummary, TaskSummary, UpdateSummary};

    fn envelope(event: AppEvent) -> EventEnvelope {
        EventEnvelope {
            id: "e-1".into(),
            timestamp: chrono::Utc::now(),
            event,
        }
    }

    /// 占位符替换：五个占位符各就各位；未知占位符原样保留
    #[test]
    fn test_render_placeholders() {
        let message = MessageContent {
            event_type: "login_failed".into(),
            summary: "登录失败".into(),
            detail: "档案 p1 · 原因：x".into(),
            profile: "p1".into(),
            time: "2026-01-01 08:00:00".into(),
        };
        let out = render_placeholders(
            "[{{time}}] {{event_type}}/{{profile}}: {{summary}} — {{detail}} {{nope}}",
            &message,
        );
        assert_eq!(
            out,
            "[2026-01-01 08:00:00] login_failed/p1: 登录失败 — 档案 p1 · 原因：x {{nope}}"
        );
    }

    /// 缺字段（None 载荷）不产生空段拼接：无 reason / attempt 时短语省略
    #[test]
    fn test_summarize_login_failed_minimal() {
        let message = summarize(&envelope(AppEvent::LoginFailed(LoginSummary {
            profile_id: "p1".into(),
            channel: "http".into(),
            reason: None,
            attempt: None,
        })));
        assert_eq!(message.event_type, "login_failed");
        assert_eq!(message.summary, "登录失败");
        assert_eq!(message.detail, "档案 p1 · 渠道 http");
        assert_eq!(message.profile, "p1");
    }

    /// 三预设 payload 形状钉住（字段名是接收方契约，改名即坏）
    #[test]
    fn test_preset_payload_shapes() {
        let message = MessageContent {
            event_type: "network_down".into(),
            summary: "网络已断开".into(),
            detail: "原因：tcp_timeout".into(),
            profile: "p1".into(),
            time: "2026-01-01 08:00:00".into(),
        };
        let bark = bark_body(&message);
        assert_eq!(bark["title"], "网络已断开");
        assert_eq!(bark["body"], "原因：tcp_timeout");
        assert_eq!(bark["group"], "Campus-Auth");
        let form = serverchan_form(&message);
        assert_eq!(form[0], ("title".into(), "网络已断开".into()));
        assert_eq!(form[1], ("desp".into(), "原因：tcp_timeout".into()));
        let tg = telegram_body(&message, "12345");
        assert_eq!(tg["chat_id"], "12345");
        assert_eq!(tg["text"], "网络已断开\n原因：tcp_timeout");
    }

    /// webhook 默认模板渲染后是合法 JSON 且占位符全部落位
    #[test]
    fn test_default_webhook_body_valid_json() {
        let message = summarize(&envelope(AppEvent::TaskFailed(TaskSummary {
            task_id: "t-1".into(),
            task_kind: Some("script".into()),
            reason: Some("exit_code_1".into()),
        })));
        let body = render_webhook_body("", &message);
        let value: serde_json::Value =
            serde_json::from_str(&body).expect("默认模板必须渲染出合法 JSON");
        assert_eq!(value["title"], "任务执行失败");
        assert_eq!(value["event"], "task_failed");
        assert_eq!(value["profile"], "-");
    }

    /// 自定义模板：占位符位于字符串值内，渲染后仍是合法 JSON
    #[test]
    fn test_custom_webhook_body() {
        let message = summarize(&envelope(AppEvent::UpdateAvailable(UpdateSummary {
            version: "5.2.0".into(),
            current_version: Some("5.1.3".into()),
        })));
        let body = render_webhook_body(r#"{"text": "新版本 {{summary}}（{{detail}}）"}"#, &message);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["text"].as_str().unwrap().contains("5.2.0"));
    }

    /// detail 只拼接白名单字段（脱敏红线：无凭据来源可进）
    #[test]
    fn test_summarize_network_down_includes_reason_only() {
        let message = summarize(&envelope(AppEvent::NetworkDown(NetworkSummary {
            profile_id: Some("p1".into()),
            reason: Some("tcp_timeout".into()),
        })));
        assert_eq!(message.summary, "网络已断开");
        assert_eq!(message.detail, "档案 p1 · 原因：tcp_timeout");
    }

    /// 系统通知内容：标题取摘要，正文短于上限原样透出，超长按字符边界裁剪补省略号
    #[test]
    fn test_os_body_truncates_on_char_boundary() {
        let message = MessageContent {
            event_type: "network_down".into(),
            summary: "网络已断开".into(),
            detail: "档案 p1 · 原因：tcp_timeout".into(),
            profile: "p1".into(),
            time: "2026-01-01 08:00:00".into(),
        };
        let (title, body) = os_body(&message);
        assert_eq!(title, "网络已断开");
        assert_eq!(body, "档案 p1 · 原因：tcp_timeout", "未超长不裁剪");

        // 超长：正好 200 字符 + 1 个多字节字符，验证按字符而非字节截断
        let long = "喵".repeat(201);
        let message = MessageContent {
            detail: long,
            ..message
        };
        let (title, body) = os_body(&message);
        assert_eq!(title, "网络已断开");
        assert_eq!(body.chars().count(), 201, "200 字符正文 + 1 个省略号");
        assert!(body.ends_with('…'), "截断时补省略号");
        assert!(
            body.starts_with("喵".repeat(200).as_str()),
            "按字符边界截断"
        );
    }

    /// 样例载荷与测试消息：供校验与端点使用，形状完整
    #[test]
    fn test_sample_and_test_message() {
        assert_eq!(MessageContent::sample().event_type, "login_failed");
        assert_eq!(MessageContent::test_message().event_type, "test");
    }
}
