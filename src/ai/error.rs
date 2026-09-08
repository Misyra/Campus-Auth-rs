//! AI 生成链路强类型错误：替代各处的 `Result<_, String>`
//!
//! 约束：所有变体的 [`std::fmt::Display`] 文案与收敛前的字符串逐字一致——
//! 前端靠 SSE 错误文本分类（如 `includes("空闲超时")`），路由测试断言文案
//! 子串，改文案会静默破坏跨层契约。唯一例外是 `Truncated`：流式与非流式
//! 两处截断文案原先略有出入（后者缺 max_tokens 操作建议），统一为较长的
//! 流式版，两处语义相同且测试仅断言 `contains("截断")`。

/// AI 生成链路错误（LLM 调用 → 生成编排 → URL 校验）
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// 用户取消 / 客户端断连（流消费中令牌触发）
    #[error("生成已取消")]
    Cancelled,
    /// reqwest 请求级超时（可重试）
    #[error("LLM 请求超时（>{seconds}s）")]
    RequestTimeout { seconds: u64 },
    /// TCP 建连失败（可重试）
    #[error("无法连接 LLM 服务（{url}）: {source}")]
    ConnectFailed {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// 非超时/建连的其他 reqwest 错误（不可重试）
    #[error("LLM 请求失败: {source}")]
    RequestFailed {
        #[source]
        source: reqwest::Error,
    },
    /// HTTP 客户端构建失败（服务端配置问题，不可重试）
    #[error("HTTP 客户端构建失败: {source}")]
    ClientBuild {
        #[source]
        source: reqwest::Error,
    },
    /// LLM 远端返回非 2xx（429/5xx 可重试）
    #[error("LLM 服务返回 {status}: {snippet}")]
    LlmServiceError {
        status: reqwest::StatusCode,
        snippet: String,
    },
    /// 流式传输中连接中断（可重试）
    #[error("LLM 流式传输失败: {0}")]
    StreamInterrupted(#[from] reqwest::Error),
    /// 流式空闲超时（可重试）
    #[error("LLM 流式空闲超时（>{seconds}s 无输出），请检查网络或稍后重试")]
    IdleTimeout { seconds: u64 },
    /// 流式总时长预算耗尽（致命，不可重试）
    #[error("LLM 流式响应总时长超出 10 分钟上限")]
    TotalBudgetExceeded,
    /// 输出被 max_tokens 截断（致命：产物必然残缺）
    #[error(
        "LLM 输出被 max_tokens 截断（finish_reason=length），任务 JSON 不完整；请简化任务描述或调大 llm.json 的 max_tokens 后重试"
    )]
    Truncated,
    /// 流全程未产出文本（可重试：换连接通常可恢复）
    #[error("LLM 流式响应为空（未收到任何增量内容）")]
    EmptyStream,
    /// 非流式响应不是合法 JSON
    #[error("LLM 响应不是合法 JSON: {source}")]
    InvalidJson {
        #[from]
        source: serde_json::Error,
    },
    /// 非流式响应缺 content（数组部件兜底亦空）
    #[error("LLM 响应缺少 choices[0].message.content: {snippet}")]
    MissingContent { snippet: String },
    /// 模型输出中抽不出任务 JSON（围栏剥离 + 首尾花括号兜底均失败）
    #[error("无法从模型输出中提取 JSON（输出前 200 字符: {snippet}）")]
    ExtractJsonFailed { snippet: String },
    /// 自纠轮耗尽仍未通过任务校验
    #[error("连续 {max_attempts} 轮生成均未通过任务校验，最后错误：\n{}", errors.join("\n"))]
    ValidationExhausted {
        max_attempts: u32,
        errors: Vec<String>,
    },
    /// 生成编排包装的单轮 LLM 调用失败（保留轮次上下文）
    #[error("第 {attempt} 轮生成失败: {source}")]
    ChatAttemptFailed {
        attempt: u32,
        #[source]
        source: Box<AiError>,
    },
    /// Base URL 为空
    #[error("Base URL 不能为空")]
    BaseUrlEmpty,
    /// Base URL 解析失败
    #[error("Base URL 格式无效: {0}")]
    BaseUrlInvalid(#[from] url::ParseError),
    /// Base URL 非 http/https
    #[error("Base URL 仅支持 http/https")]
    BaseUrlUnsupportedScheme,
    /// Base URL 携带 userinfo（凭据会落配置明文区）
    #[error("Base URL 不允许携带用户名/密码（请把 key 填在 API Key 输入框）")]
    BaseUrlUserInfoForbidden,
    /// Base URL 缺 host
    #[error("Base URL 缺少主机名")]
    BaseUrlMissingHost,
}

impl AiError {
    /// 是否值得换一次连接重试（替代原私有 `StreamError.retryable` 标记——
    /// 该标记曾在 `chat_completion_with_stream` 的 `Err(e) => return Err(e.message)`
    /// 处被丢弃，导致重试判定退化为字符串；编码进变体后随类型传播）
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::RequestTimeout { .. }
            | AiError::ConnectFailed { .. }
            | AiError::StreamInterrupted(_)
            | AiError::IdleTimeout { .. }
            | AiError::EmptyStream => true,
            AiError::LlmServiceError { status, .. } => {
                *status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
            }
            AiError::ChatAttemptFailed { source, .. } => source.is_retryable(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display 文案与收敛前逐字一致（跨层契约：前端文本分类 + 路由测试断言）
    #[test]
    fn test_display_matches_legacy_strings() {
        assert_eq!(AiError::Cancelled.to_string(), "生成已取消");
        assert_eq!(
            AiError::RequestTimeout { seconds: 120 }.to_string(),
            "LLM 请求超时（>120s）"
        );
        assert_eq!(
            AiError::IdleTimeout { seconds: 600 }.to_string(),
            "LLM 流式空闲超时（>600s 无输出），请检查网络或稍后重试"
        );
        assert!(
            AiError::Truncated.to_string().contains("截断"),
            "截断文案必须含可断言子串"
        );
        assert_eq!(
            AiError::ValidationExhausted {
                max_attempts: 2,
                errors: vec!["name 不能为空".into()],
            }
            .to_string(),
            "连续 2 轮生成均未通过任务校验，最后错误：\nname 不能为空"
        );
        assert_eq!(
            AiError::ChatAttemptFailed {
                attempt: 1,
                source: Box::new(AiError::Cancelled),
            }
            .to_string(),
            "第 1 轮生成失败: 生成已取消"
        );
    }

    /// 可重试判定与原 `StreamError.retryable` 语义一致
    #[test]
    fn test_is_retryable() {
        assert!(AiError::RequestTimeout { seconds: 1 }.is_retryable());
        assert!(AiError::IdleTimeout { seconds: 1 }.is_retryable());
        assert!(AiError::EmptyStream.is_retryable());
        assert!(
            AiError::LlmServiceError {
                status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                snippet: String::new(),
            }
            .is_retryable()
        );
        assert!(
            !AiError::LlmServiceError {
                status: reqwest::StatusCode::BAD_REQUEST,
                snippet: String::new(),
            }
            .is_retryable()
        );
        assert!(!AiError::Truncated.is_retryable());
        assert!(!AiError::Cancelled.is_retryable());
        // 包装层委托内部判定
        assert!(
            AiError::ChatAttemptFailed {
                attempt: 1,
                source: Box::new(AiError::IdleTimeout { seconds: 1 }),
            }
            .is_retryable()
        );
    }
}
