//! OpenAI 兼容 chat/completions 客户端（AI 任务生成的出站调用）
//!
//! DeepSeek / 智谱 GLM 等主流服务商均兼容 OpenAI `/chat/completions`
//! 协议；视觉输入统一走 `image_url`（data URL）消息部件。调用为低频一次性
//! 请求，client 按次构建（超时预算独立于其他出站模块）。

use std::time::Duration;

use serde_json::{Value, json};

use super::LlmSettings;

/// 单次生成的总超时：视觉模型理解长上下文 + 生成完整任务 JSON，预留宽预算
const CHAT_TIMEOUT: Duration = Duration::from_secs(120);

/// 执行一次 chat/completions，返回 assistant 文本
///
/// `messages` 为已组装好的 OpenAI 消息数组（system/user/assistant，按值接收——
/// 响应 future 需跨越 await，按引用会构成悬垂返回）。非鉴权类失败（网络/超时/
/// 非 2xx）返回带原因的错误串，由调用方决定是否回喂重试。
pub async fn chat_completion(
    settings: &LlmSettings,
    api_key: &str,
    messages: Vec<Value>,
) -> Result<String, String> {
    let url = format!(
        "{}/chat/completions",
        settings.base_url.trim_end_matches('/')
    );
    let payload = json!({
        "model": settings.model,
        "messages": messages,
        // 任务 JSON 生成要确定性输出，低温度抑制自由发挥
        "temperature": 0.1,
        "max_tokens": 8192,
    });

    let client = reqwest::Client::builder()
        .timeout(CHAT_TIMEOUT)
        // 屏蔽系统代理：LLM 端点（国内服务商/本地网关/直连可达的中转）走本机代理
        // 客户端的全局模式反而会失败或绕行；需要代理出网的场景后续按 monitor 的
        // 代理设置模式做成可配置项
        .no_proxy()
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;

    let mut req = client.post(&url).json(&payload);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            format!("LLM 请求超时（>{}s）", CHAT_TIMEOUT.as_secs())
        } else if e.is_connect() {
            format!("无法连接 LLM 服务（{url}）: {e}")
        } else {
            format!("LLM 请求失败: {e}")
        }
    })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // 截断错误体：服务端可能回整页 HTML，只要能定位错误原因即可
        let snippet: String = body.chars().take(500).collect();
        return Err(format!("LLM 服务返回 {status}: {snippet}"));
    }

    parse_chat_response(&body)
}

/// 解析 OpenAI 兼容响应体，提取 `choices[0].message.content`
fn parse_chat_response(body: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("LLM 响应不是合法 JSON: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content.is_empty() {
        // 兼容部分网关把 content 拆成 content 数组的实现：拼接 text 部件
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
}
