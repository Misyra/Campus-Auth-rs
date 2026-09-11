//! OpenAI 兼容 chat/completions 客户端（AI 任务生成的出站调用）
//!
//! DeepSeek / 智谱 GLM 等主流服务商均兼容 OpenAI `/chat/completions`
//! 协议；视觉输入统一走 `image_url`（data URL）消息部件。调用为低频一次性
//! 请求，client 按次构建（超时预算独立于其他出站模块）。
//!
//! 流式模式（`stream: true`）用于前端实时进度展示；同时受空闲超时与总预算约束：
//! 收到 chunk 会刷新空闲计时，但单次请求无论是否持续输出都不超过 10 分钟。

use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::LlmSettings;
use super::error::AiError;

/// 空闲超时：连续无字节到达的判定阈值（同时受 10 分钟总预算限制）
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// 总预算上限：单次流式会话的最长墙钟时间（与空闲超时同值，兜底）
pub const TOTAL_BUDGET: Duration = Duration::from_secs(600);

/// 非流式旧语义的总超时（保留供兼容/测试，实际流式路径不再使用）
const CHAT_TIMEOUT: Duration = Duration::from_secs(120);

/// 瞬时故障（超时/连接失败/429/5xx）的额外重试次数（指数退避）。
/// 校验失败的自纠轮由 generate.rs 负责，此处只补传输层抖动
const CHAT_MAX_RETRIES: u32 = 2;
/// 模型文本上限：足够容纳复杂任务，同时阻止异常服务无界占用内存
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// 尚未出现换行的单个 SSE 帧上限
const MAX_SSE_BUFFER_BYTES: usize = 1024 * 1024;

/// 流式回调：每收到一个增量 content 片段即调用
pub type StreamCallback = Box<dyn FnMut(&str) + Send>;

/// 执行一次 chat/completions，返回 assistant 文本（非流式，保留用于测试/回退）
pub async fn chat_completion(
    settings: &LlmSettings,
    api_key: &str,
    messages: Vec<Value>,
) -> Result<String, AiError> {
    chat_completion_with_stream(settings, api_key, messages, None, None).await
}

/// 流式 chat/completions：`on_delta` 非空时以 `stream: true` 请求，并在解析
/// 到每个增量 `delta.content` 时回调；收到字节会重置空闲计时，总预算不会重置。
///
/// `cancel` 非空时响应取消令牌：流消费过程中令牌触发即中止（用于客户端断连/
/// 用户取消时停止无谓的 token 消耗）。
pub async fn chat_completion_with_stream(
    settings: &LlmSettings,
    api_key: &str,
    messages: Vec<Value>,
    mut on_delta: Option<StreamCallback>,
    cancel: Option<&CancellationToken>,
) -> Result<String, AiError> {
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
        .map_err(|e| AiError::ClientBuild { source: e })?;

    let mut last_err = AiError::Cancelled;
    for attempt in 0..=CHAT_MAX_RETRIES {
        if let Some(t) = cancel
            && t.is_cancelled()
        {
            return Err(AiError::Cancelled);
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
                return Err(AiError::Cancelled);
            }
            r = req.send() => r.map_err(|e| (e, cancel.is_some_and(|t| t.is_cancelled()))),
        };
        let resp = match resp {
            Ok(r) => r,
            Err((e, was_cancelled)) => {
                // 竞态：取消与 send 错误同到达时已响应取消兜底消息，
                // 否则按原始错误类型可重试判定
                if was_cancelled {
                    return Err(AiError::Cancelled);
                }
                last_err = if e.is_timeout() {
                    AiError::RequestTimeout {
                        seconds: timeout_budget.as_secs(),
                    }
                } else if e.is_connect() {
                    AiError::ConnectFailed {
                        url: url.clone(),
                        source: e,
                    }
                } else {
                    AiError::RequestFailed { source: e }
                };
                if last_err.is_retryable() && attempt < CHAT_MAX_RETRIES {
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
            last_err = AiError::LlmServiceError { status, snippet };
            if last_err.is_retryable() && attempt < CHAT_MAX_RETRIES {
                tracing::warn!(attempt = attempt + 1, "{last_err}，退避后重试");
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                continue;
            }
            return Err(last_err);
        }

        if streaming {
            match stream_chat_response(resp, &mut on_delta, cancel).await {
                Ok(text) => return Ok(text),
                Err((e, false)) if e.is_retryable() && attempt < CHAT_MAX_RETRIES => {
                    tracing::warn!(attempt = attempt + 1, "{e}，退避后重试");
                    last_err = e;
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                    continue;
                }
                // 已向前端推送过部分文本时不透明重试，避免两次响应拼接成无效 JSON。
                Err((e, _)) => return Err(e),
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
) -> Result<String, (AiError, bool)> {
    let mut full = String::new();
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut finished = false;
    let mut last_finish_reason: Option<String> = None;
    let mut emitted = false;

    let deadline = tokio::time::Instant::now() + TOTAL_BUDGET;
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;

    // 外层循环：单次连接的生命周期——逐块拉取 SSE 数据并驱动"空闲/总时长"双预算，
    // 直到流自然结束、收到 [DONE]，或任一预算耗尽/取消触发返回
    loop {
        // 总预算（10 分钟）硬上限：空闲预算会被数据不断刷新，此处兜底防"慢滴流"无限占用
        if tokio::time::Instant::now() >= deadline {
            return Err((AiError::TotalBudgetExceeded, emitted));
        }
        let remaining_idle = idle_deadline.saturating_duration_since(tokio::time::Instant::now());
        let remaining_total = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining_idle.min(remaining_total);
        // 中层 select：取消赛道与数据赛道先到者胜；等待上限取空闲剩余与总预算剩余的
        // 较小值，超时后再按命中哪条预算定性（空闲可重试、总预算致命）
        let chunk = tokio::select! {
            // 取消赛道：无令牌时以 pending() 永久挂起，两分支统一为同型 future
            _ = async {
                match cancel {
                    Some(t) => t.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                return Err((AiError::Cancelled, emitted));
            }
            waited = tokio::time::timeout(wait, stream.next()) => match waited {
                Ok(Some(Ok(bytes))) => bytes,
                Ok(Some(Err(e))) => {
                    return Err((AiError::StreamInterrupted(e), emitted));
                }
                // 流自然结束（服务端关闭连接）：跳出外层循环，进入收尾解析
                Ok(None) => break,
                Err(_) => {
                    // 超时赛道：区分空闲超时（连接可能还活着，可重试）与总预算耗尽（致命）
                    if tokio::time::Instant::now() >= idle_deadline {
                        return Err((AiError::IdleTimeout {
                            seconds: IDLE_TIMEOUT.as_secs(),
                        }, emitted));
                    }
                    return Err((AiError::TotalBudgetExceeded, emitted));
                }
            },
        };
        // 收到数据即刷新空闲预算：只有"持续无输出"才判定为空闲超时
        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_SSE_BUFFER_BYTES {
            return Err((AiError::ResponseTooLarge, emitted));
        }
        // 内层 while：将累积缓冲按换行切段逐行解析——TCP chunk 不保证与 SSE 行
        // 边界对齐，必须先入 buf 再按 '\n' 切分，残行留待下个 chunk 补齐
        while let Some(pos) = buf.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            match consume_sse_line(
                &line[..line.len().saturating_sub(1)],
                &mut full,
                on_delta,
                &mut last_finish_reason,
            ) {
                Ok(done) => {
                    emitted = !full.is_empty();
                    if full.len() > MAX_RESPONSE_BYTES {
                        return Err((AiError::ResponseTooLarge, emitted));
                    }
                    if done {
                        finished = true;
                        break;
                    }
                }
                Err(error) => return Err((error, emitted)),
            }
        }
        if finished {
            break;
        }
    }
    // 尾部兜底：流结束但缓冲仍有残留（无换行结尾的单行 JSON——部分网关不发
    // [DONE] 也不带尾换行）时，按整包响应再解析一次，避免丢掉最后一段内容
    if !buf.iter().all(u8::is_ascii_whitespace) {
        match consume_sse_line(&buf, &mut full, on_delta, &mut last_finish_reason) {
            Ok(_) => emitted = !full.is_empty(),
            Err(error) => return Err((error, emitted)),
        }
    }
    // 全程未产出任何文本：按可重试失败处理（让上层换连接重试），而非静默返回空串
    if full.is_empty() {
        return Err((AiError::EmptyStream, emitted));
    }
    // 收尾双保险：即使逐行路径因宽松解析漏过 length 事件，也不允许截断产物被当成功返回
    if last_finish_reason.as_deref() == Some("length") {
        return Err((AiError::Truncated, emitted));
    }
    Ok(full)
}

/// 解析一行 OpenAI 兼容 SSE；返回是否收到 `[DONE]`。
fn consume_sse_line(
    bytes: &[u8],
    full: &mut String,
    on_delta: &mut Option<StreamCallback>,
    last_finish_reason: &mut Option<String>,
) -> Result<bool, AiError> {
    let line = std::str::from_utf8(bytes)
        .map_err(|_| AiError::InvalidStreamUtf8)?
        .trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }
    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
        return Ok(false);
    };
    if data == "[DONE]" {
        return Ok(true);
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Ok(false);
    };
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        if reason == "length" {
            return Err(AiError::Truncated);
        }
        if !reason.is_empty() && reason != "null" {
            *last_finish_reason = Some(reason.to_string());
        }
    }
    let content = value
        .pointer("/choices/0/delta/content")
        .or_else(|| value.pointer("/choices/0/message/content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !content.is_empty() {
        full.push_str(content);
        if let Some(callback) = on_delta.as_mut() {
            callback(content);
        }
    }
    Ok(false)
}

/// 解析 OpenAI 兼容响应体，提取 `choices[0].message.content`
fn parse_chat_response(body: &str) -> Result<String, AiError> {
    let v: Value = serde_json::from_str(body)?;
    if v.pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        return Err(AiError::Truncated);
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
            return Err(AiError::MissingContent {
                snippet: body.chars().take(300).collect(),
            });
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
        assert!(matches!(err, AiError::Truncated), "actual: {err}");
    }

    #[test]
    fn test_consume_sse_line_preserves_multibyte_and_tail_event() {
        let mut full = String::new();
        let mut callback = None;
        let mut reason = None;
        let line = r#"data: {"choices":[{"delta":{"content":"校园网"},"finish_reason":"stop"}]}"#;
        assert!(!consume_sse_line(line.as_bytes(), &mut full, &mut callback, &mut reason).unwrap());
        assert_eq!(full, "校园网");
        assert_eq!(reason.as_deref(), Some("stop"));
        assert!(consume_sse_line(b"data: [DONE]", &mut full, &mut callback, &mut reason).unwrap());
    }

    #[test]
    fn test_consume_sse_line_rejects_invalid_utf8() {
        let mut full = String::new();
        let mut callback = None;
        let mut reason = None;
        let err = consume_sse_line(&[0xff], &mut full, &mut callback, &mut reason).unwrap_err();
        assert!(matches!(err, AiError::InvalidStreamUtf8));
    }
}
