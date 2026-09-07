//! OpenAI 兼容 chat/completions 客户端（AI 任务生成的出站调用）
//!
//! DeepSeek / 智谱 GLM 等主流服务商均兼容 OpenAI `/chat/completions`
//! 协议；视觉输入统一走 `image_url`（data URL）消息部件。调用为低频一次性
//! 请求，client 按次构建（超时预算独立于其他出站模块）。
//!
//! 流式模式（`stream: true`）用于前端实时进度展示；超时语义为"空闲超时"
//! （idle timeout）：只要仍有 chunk 到达就续命，仅当连续 IDLE_TIMEOUT 内
//! 无任何字节时才判定超时，上限 TOTAL_BUDGET（10 分钟）。

use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::LlmSettings;

/// 空闲超时：连续无字节到达的判定阈值（10 分钟，只要还在输出就不超时）
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// 总预算上限：单次流式会话的最长墙钟时间（与空闲超时同值，兜底）
pub const TOTAL_BUDGET: Duration = Duration::from_secs(600);

/// 非流式旧语义的总超时（保留供兼容/测试，实际流式路径不再使用）
const CHAT_TIMEOUT: Duration = Duration::from_secs(120);

/// 瞬时故障（超时/连接失败/429/5xx）的额外重试次数（指数退避）。
/// 校验失败的自纠轮由 generate.rs 负责，此处只补传输层抖动
const CHAT_MAX_RETRIES: u32 = 2;

/// 流式回调：每收到一个增量 content 片段即调用
pub type StreamCallback = Box<dyn FnMut(&str) + Send>;

/// 流式消费错误：区分"换一次连接可能恢复"的传输层故障与重试无意义的内容层故障
#[derive(Debug)]
struct StreamError {
    message: String,
    /// true = 传输层抖动（空闲超时/连接中断/空响应），重试同请求通常可恢复；
    /// false = 内容层故障（输出截断/用户取消/预算耗尽），重试只会重复同样结果
    retryable: bool,
}

impl StreamError {
    fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// 执行一次 chat/completions，返回 assistant 文本（非流式，保留用于测试/回退）
pub async fn chat_completion(
    settings: &LlmSettings,
    api_key: &str,
    messages: Vec<Value>,
) -> Result<String, String> {
    chat_completion_with_stream(settings, api_key, messages, None, None).await
}

/// 流式 chat/completions：`on_delta` 非空时以 `stream: true` 请求，并在解析
/// 到每个增量 `delta.content` 时回调；空闲超时语义：只要有字节到达就重置计时。
///
/// `cancel` 非空时响应取消令牌：流消费过程中令牌触发即中止（用于客户端断连/
/// 用户取消时停止无谓的 token 消耗）。
pub async fn chat_completion_with_stream(
    settings: &LlmSettings,
    api_key: &str,
    messages: Vec<Value>,
    mut on_delta: Option<StreamCallback>,
    cancel: Option<&CancellationToken>,
) -> Result<String, String> {
    let url = format!(
        "{}/chat/completions",
        settings.base_url.trim_end_matches('/')
    );
    let streaming = on_delta.is_some();
    // max_tokens 仅在显式配置时携带：硬编码上限会截断长任务 JSON，也可能超出
    // 部分模型的限额；未配置时交由服务商默认（见 LlmSettings::max_tokens）
    let mut payload = json!({
        "model": settings.model,
        "messages": messages,
        "temperature": 0.1,
    });
    if let Some(mt) = settings.max_tokens {
        payload["max_tokens"] = json!(mt);
    }
    if streaming {
        payload["stream"] = json!(true);
        payload["stream_options"] = json!({ "include_usage": false });
    }

    let timeout_budget = if streaming {
        TOTAL_BUDGET
    } else {
        CHAT_TIMEOUT
    };
    let client = reqwest::Client::builder()
        .timeout(timeout_budget)
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;

    let mut last_err = String::new();
    for attempt in 0..=CHAT_MAX_RETRIES {
        if let Some(t) = cancel
            && t.is_cancelled()
        {
            return Err("生成已取消".into());
        }
        let mut req = client.post(&url).json(&payload);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp: Result<reqwest::Response, (reqwest::Error, bool)> = tokio::select! {
            biased;
            _ = async {
                match cancel {
                    Some(t) => t.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                return Err("生成已取消".into());
            }
            r = req.send() => r.map_err(|e| (e, cancel.is_some_and(|t| t.is_cancelled()))),
        };
        let resp = match resp {
            Ok(r) => r,
            Err((e, was_cancelled)) => {
                // 竞态：取消与 send 错误同到达时已响应取消兜底消息，
                // 否则按原始错误类型可重试判定
                if was_cancelled {
                    return Err("生成已取消".into());
                }
                let retryable = e.is_timeout() || e.is_connect();
                last_err = if e.is_timeout() {
                    format!("LLM 请求超时（>{}s）", timeout_budget.as_secs())
                } else if e.is_connect() {
                    format!("无法连接 LLM 服务（{url}）: {e}")
                } else {
                    format!("LLM 请求失败: {e}")
                };
                if retryable && attempt < CHAT_MAX_RETRIES {
                    tracing::warn!(attempt = attempt + 1, "{last_err}，退避后重试");
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                    continue;
                }
                return Err(last_err);
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(500).collect();
            last_err = format!("LLM 服务返回 {status}: {snippet}");
            let retryable = status == 429 || status.is_server_error();
            if retryable && attempt < CHAT_MAX_RETRIES {
                tracing::warn!(attempt = attempt + 1, "{last_err}，退避后重试");
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                continue;
            }
            return Err(last_err);
        }

        if streaming {
            match stream_chat_response(resp, &mut on_delta, cancel).await {
                Ok(text) => return Ok(text),
                Err(e) if e.retryable && attempt < CHAT_MAX_RETRIES => {
                    last_err = e.message;
                    tracing::warn!(attempt = attempt + 1, "{last_err}，退避后重试");
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                    continue;
                }
                Err(e) => return Err(e.message),
            }
        } else {
            let body = resp.text().await.unwrap_or_default();
            return parse_chat_response(&body);
        }
    }
    Err(last_err)
}

/// 以 idle 超时消费 SSE 流，聚合完整文本并通过回调实时推送增量
async fn stream_chat_response(
    resp: reqwest::Response,
    on_delta: &mut Option<StreamCallback>,
    cancel: Option<&CancellationToken>,
) -> Result<String, StreamError> {
    let mut full = String::new();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut finished = false;
    let mut last_finish_reason: Option<String> = None;

    let deadline = tokio::time::Instant::now() + TOTAL_BUDGET;
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(StreamError::fatal("LLM 流式响应总时长超出 10 分钟上限"));
        }
        let remaining_idle = idle_deadline.saturating_duration_since(tokio::time::Instant::now());
        let remaining_total = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining_idle.min(remaining_total);
        let chunk = tokio::select! {
            _ = async {
                match cancel {
                    Some(t) => t.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                return Err(StreamError::fatal("生成已取消"));
            }
            waited = tokio::time::timeout(wait, stream.next()) => match waited {
                Ok(Some(Ok(bytes))) => bytes,
                Ok(Some(Err(e))) => {
                    return Err(StreamError::retryable(format!("LLM 流式传输失败: {e}")));
                }
                Ok(None) => break,
                Err(_) => {
                    if tokio::time::Instant::now() >= idle_deadline {
                        return Err(StreamError::retryable(format!(
                            "LLM 流式空闲超时（>{}s 无输出），请检查网络或稍后重试",
                            IDLE_TIMEOUT.as_secs()
                        )));
                    }
                    return Err(StreamError::fatal("LLM 流式响应总时长超出 10 分钟上限"));
                }
            },
        };
        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
        let text = String::from_utf8_lossy(&chunk);
        buf.push_str(&text);
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let data = if let Some(d) = line.strip_prefix("data:") {
                d.trim()
            } else {
                continue;
            };
            if data == "[DONE]" {
                finished = true;
                break;
            }
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if let Some(fr) = v
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                {
                    if fr == "length" {
                        return Err(StreamError::fatal(
                            "LLM 输出被 max_tokens 截断（finish_reason=length），任务 JSON 不完整；请简化任务描述或调大 llm.json 的 max_tokens 后重试",
                        ));
                    }
                    if !fr.is_empty() && fr != "null" {
                        last_finish_reason = Some(fr.to_string());
                    }
                }
                if let Some(delta) = v
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                {
                    if !delta.is_empty() {
                        full.push_str(delta);
                        if let Some(cb) = on_delta.as_mut() {
                            cb(delta);
                        }
                    }
                    continue;
                }
                if let Some(content) = v
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                {
                    if !content.is_empty() {
                        full.push_str(content);
                        if let Some(cb) = on_delta.as_mut() {
                            cb(content);
                        }
                    }
                }
            }
        }
        if finished {
            break;
        }
    }
    if full.is_empty() && !buf.trim().is_empty() {
        if let Ok(v) = serde_json::from_str::<Value>(buf.trim()) {
            if let Some(c) = v
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
            {
                full.push_str(c);
                if let Some(cb) = on_delta.as_mut() {
                    cb(c);
                }
            }
        }
    }
    if full.is_empty() {
        return Err(StreamError::retryable(
            "LLM 流式响应为空（未收到任何增量内容）",
        ));
    }
    if last_finish_reason.as_deref() == Some("length") {
        return Err(StreamError::fatal(
            "LLM 输出被 max_tokens 截断（finish_reason=length），任务 JSON 不完整；请简化任务描述或调大 llm.json 的 max_tokens 后重试",
        ));
    }
    Ok(full)
}

/// 解析 OpenAI 兼容响应体，提取 `choices[0].message.content`
fn parse_chat_response(body: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("LLM 响应不是合法 JSON: {e}"))?;
    if v.pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        return Err(
            "LLM 输出被 max_tokens 截断（finish_reason=length），任务 JSON 不完整；请简化任务描述后重试".into(),
        );
    }
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content.is_empty() {
        let joined = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if joined.is_empty() {
            return Err(format!(
                "LLM 响应缺少 choices[0].message.content: {}",
                body.chars().take(300).collect::<String>()
            ));
        }
        return Ok(joined);
    }
    Ok(content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chat_response_standard_shape() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"{\"name\":\"x\"}"}}]}"#;
        assert_eq!(parse_chat_response(body).unwrap(), "{\"name\":\"x\"}");
    }

    #[test]
    fn test_parse_chat_response_content_parts_fallback() {
        let body = r#"{"choices":[{"message":{"content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}}]}"#;
        assert_eq!(parse_chat_response(body).unwrap(), "hello world");
    }

    #[test]
    fn test_parse_chat_response_missing_content_errors() {
        assert!(parse_chat_response(r#"{"choices":[]}"#).is_err());
        assert!(parse_chat_response("not json").is_err());
    }

    #[test]
    fn test_parse_chat_response_truncated_reports_reason() {
        let body = r#"{"choices":[{"message":{"content":"{\"name\":"},"finish_reason":"length"}]}"#;
        let err = parse_chat_response(body).unwrap_err();
        assert!(err.contains("截断"), "actual: {err}");
    }
}
