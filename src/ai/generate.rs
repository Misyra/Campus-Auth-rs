//! 生成编排：调 LLM → 抽取任务 JSON → 强校验 → 错误回喂自纠一轮
//!
//! 编排不直接依赖 tasks 模块（避免 ai → tasks 的反向耦合），校验以闭包注入；
//! LLM 调用同理以异步函数指针注入，单测可用桩函数覆盖全流程。

use serde_json::Value;

use super::error::AiError;
use super::prompt::{self, CaptureContext};

/// 校验失败时最多追加一轮自纠对话（共两次生成）
const MAX_ATTEMPTS: u32 = 2;

/// 生成结果
#[derive(Debug)]
pub struct GenerateOutcome {
    /// 通过校验的任务 JSON（未含 task_id，入库时由前端/导入补齐）
    pub task: Value,
    /// 实际生成轮数（1 = 首轮即通过）
    pub attempts: u32,
    /// 生成过程中的非致命提示（如 JSON 修复说明）
    pub warnings: Vec<String>,
}

/// 从 LLM 回复中抽取任务 JSON
///
/// 鲁棒性策略：优先剥 ```json 围栏；无围栏时取首个 `{` 到最后一个 `}` 的片段
/// （容忍模型在 JSON 前后夹杂简短说明）。
pub fn extract_json(text: &str) -> Result<Value, AiError> {
    let candidate = extract_candidate(text);
    // 第一优先：整体/围栏片段直接解析
    if let Ok(v) = serde_json::from_str::<Value>(candidate) {
        return Ok(v);
    }
    // 兜底：从首个 { 到最后一个 } 再试一次（围栏嵌套/前后缀污染）
    if let (Some(start), Some(end)) = (candidate.find('{'), candidate.rfind('}')) {
        if end > start {
            if let Ok(v) = serde_json::from_str::<Value>(&candidate[start..=end]) {
                return Ok(v);
            }
        }
    }
    Err(AiError::ExtractJsonFailed {
        snippet: text.chars().take(200).collect(),
    })
}

/// AI 生成专用安全校验：限制为新建浏览器任务，阻止覆盖已有任务或读取本机文件。
pub fn validate_generated_task(task: &Value) -> Result<(), Vec<String>> {
    let Some(obj) = task.as_object() else {
        return Err(vec!["顶层必须为 JSON 对象".to_string()]);
    };
    let mut errors = Vec::new();
    if obj.contains_key("task_id") || obj.contains_key("id") {
        errors.push("不得包含 task_id 或顶层 id；保存时会自动创建新任务 ID".to_string());
    }
    if obj
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|value| value != "browser")
    {
        errors.push("AI 只能生成 browser 类型任务".to_string());
    }
    if obj.get("url").and_then(Value::as_str) != Some("{{LOGIN_URL}}") {
        errors.push("顶层 url 必须固定为 {{LOGIN_URL}}".to_string());
    }
    if let Some(steps) = obj.get("steps").and_then(Value::as_array) {
        for (index, step) in steps.iter().enumerate() {
            if step.get("type").and_then(Value::as_str) == Some("upload_file") {
                errors.push(format!("步骤 {} 不允许使用 upload_file", index + 1));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 抽取候选串：剥 markdown 围栏后 trim
fn extract_candidate(text: &str) -> &str {
    let trimmed = text.trim();
    // ```json ... ``` / ``` ... ```
    if let Some(rest) = trimmed.strip_prefix("```") {
        // 跳过语言标记行
        let after_lang = rest.split_once('\n').map(|(_, tail)| tail).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            return after_lang[..end].trim();
        }
    }
    trimmed
}

/// 执行生成编排
///
/// `validate` 为任务 JSON 强校验闭包（错误列表非空即校验失败；异步以适配
/// `TaskApi::validate_task_json`，注意 future 不能持有入参引用，注入实现请先 clone）；
/// `chat` 为 LLM 调用实现（生产传 [`llm::chat_completion`] 适配闭包，测试可注入桩）。
pub async fn generate_with<V, Fut, C, CFut>(
    ctx: &CaptureContext,
    extra_prompt: Option<&str>,
    validate: V,
    chat: C,
) -> Result<GenerateOutcome, AiError>
where
    V: Fn(&Value) -> Fut,
    Fut: std::future::Future<Output = Result<(), Vec<String>>>,
    C: Fn(Vec<Value>) -> CFut,
    CFut: std::future::Future<Output = Result<String, AiError>>,
{
    let mut messages = prompt::build_messages(ctx, extra_prompt);
    let mut warnings = Vec::new();
    let mut last_errors: Vec<String> = Vec::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let text = chat(messages.clone())
            .await
            .map_err(|e| AiError::ChatAttemptFailed {
                attempt,
                source: Box::new(e),
            })?;
        let parsed = extract_json(&text);
        let validation = match parsed.as_ref() {
            Ok(task) => match validate_generated_task(task) {
                Ok(()) => validate(task).await,
                Err(errors) => Err(errors),
            },
            Err(error) => Err(vec![error.to_string()]),
        };
        let task = parsed.unwrap_or(Value::Null);
        match validation {
            Ok(()) => {
                return Ok(GenerateOutcome {
                    task,
                    attempts: attempt,
                    warnings,
                });
            }
            Err(errors) => {
                last_errors = errors;
                if attempt < MAX_ATTEMPTS {
                    warnings.push("首轮输出未通过校验，已自动回喂错误并重试".to_string());
                    prompt::append_retry_messages(&mut messages, &text, &last_errors);
                }
            }
        }
    }

    Err(AiError::ValidationExhausted {
        max_attempts: MAX_ATTEMPTS,
        errors: last_errors,
    })
}

/// 流式生成编排：与 `generate_with` 同步义，但 LLM 调用以流式增量回调推送
///
/// `on_progress` 在以下时机被调用（按序）：
/// - LLM 每个增量文本片段到达时（`delta: <text>`）
/// - 校验/自纠阶段的状态变更
///
/// 回调在生成任务内同步执行，不应阻塞。
pub async fn generate_with_stream<V, Fut, C, CFut>(
    ctx: &CaptureContext,
    extra_prompt: Option<&str>,
    validate: V,
    chat_stream: C,
    on_progress: std::sync::Arc<std::sync::Mutex<Vec<StreamEvent>>>,
) -> Result<GenerateOutcome, AiError>
where
    V: Fn(&Value) -> Fut,
    Fut: std::future::Future<Output = Result<(), Vec<String>>>,
    C: Fn(Vec<Value>, Box<dyn FnMut(&str) + Send + 'static>) -> CFut,
    CFut: std::future::Future<Output = Result<String, AiError>>,
{
    let mut messages = prompt::build_messages(ctx, extra_prompt);
    let mut warnings = Vec::new();
    let mut last_errors: Vec<String> = Vec::new();
    let push = |ev: StreamEvent| {
        on_progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ev)
    };

    for attempt in 1..=MAX_ATTEMPTS {
        push(StreamEvent::AttemptStart {
            attempt,
            max: MAX_ATTEMPTS,
        });
        let on_progress_clone = on_progress.clone();
        let text = chat_stream(
            messages.clone(),
            Box::new(move |delta: &str| {
                on_progress_clone
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(StreamEvent::Delta {
                        attempt,
                        text: delta.to_string(),
                    });
            }),
        )
        .await
        .map_err(|e| AiError::ChatAttemptFailed {
            attempt,
            source: Box::new(e),
        })?;
        push(StreamEvent::AttemptDeltaDone {
            attempt,
            text_len: text.len(),
        });
        let parsed = extract_json(&text);
        let validation = match parsed.as_ref() {
            Ok(task) => match validate_generated_task(task) {
                Ok(()) => validate(task).await,
                Err(errors) => Err(errors),
            },
            Err(error) => Err(vec![error.to_string()]),
        };
        let task = parsed.unwrap_or(Value::Null);
        match validation {
            Ok(()) => {
                push(StreamEvent::Validated { attempt });
                return Ok(GenerateOutcome {
                    task,
                    attempts: attempt,
                    warnings,
                });
            }
            Err(errors) => {
                last_errors = errors.clone();
                push(StreamEvent::ValidationFailed {
                    attempt,
                    errors: errors.clone(),
                });
                if attempt < MAX_ATTEMPTS {
                    warnings.push("首轮输出未通过校验，已自动回喂错误并重试".to_string());
                    prompt::append_retry_messages(&mut messages, &text, &last_errors);
                    push(StreamEvent::Retrying {
                        attempt,
                        next: attempt + 1,
                    });
                }
            }
        }
    }

    Err(AiError::ValidationExhausted {
        max_attempts: MAX_ATTEMPTS,
        errors: last_errors,
    })
}

/// 流式进度事件（序列化为 SSE `data:` 帧）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    AttemptStart {
        attempt: u32,
        max: u32,
    },
    Delta {
        attempt: u32,
        text: String,
    },
    AttemptDeltaDone {
        attempt: u32,
        text_len: usize,
    },
    Validated {
        attempt: u32,
    },
    ValidationFailed {
        attempt: u32,
        errors: Vec<String>,
    },
    Retrying {
        attempt: u32,
        next: u32,
    },
    Done {
        attempts: u32,
        warnings: Vec<String>,
        task: Value,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- extract_json ----

    #[test]
    fn test_extract_json_plain() {
        let v = extract_json(r#"{"name": "登录"}"#).unwrap();
        assert_eq!(v["name"], "登录");
    }

    #[test]
    fn test_extract_json_fenced() {
        let text = "```json\n{\"name\": \"登录\", \"steps\": []}\n```";
        let v = extract_json(text).unwrap();
        assert_eq!(v["name"], "登录");
    }

    #[test]
    fn test_extract_json_with_prose_prefix_suffix() {
        let text = "好的，以下是任务：\n{\"name\": \"x\"}\n希望有帮助";
        let v = extract_json(text).unwrap();
        assert_eq!(v["name"], "x");
    }

    #[test]
    fn test_extract_json_nested_braces() {
        // rfind('}') 取最外层收尾，嵌套对象不破坏抽取
        let text = "前缀 {\"a\": {\"b\": 1}, \"steps\": []} 后缀";
        let v = extract_json(text).unwrap();
        assert_eq!(v["a"]["b"], 1);
    }

    #[test]
    fn test_extract_json_invalid_errors() {
        assert!(extract_json("完全没有 JSON").is_err());
    }

    // ---- 编排 ----

    /// 最小合法任务（通过 mock 校验器的规则）
    fn valid_task() -> Value {
        json!({
            "name": "测试登录",
            "url": "{{LOGIN_URL}}",
            "steps": [{ "id": "fill_user", "type": "input", "selector": "#u", "value": "{{USERNAME}}" }]
        })
    }

    /// 捕获上下文桩
    fn ctx() -> CaptureContext {
        CaptureContext {
            request_url: "http://p".into(),
            final_url: "http://p/login".into(),
            title: "t".into(),
            html: "<html></html>".into(),
            structure: None,
            screenshot_png: vec![0],
            note: None,
        }
    }

    #[tokio::test]
    async fn test_generate_first_attempt_success() {
        let task = valid_task();
        let outcome = generate_with(
            &ctx(),
            None,
            |_v| async { Ok(()) },
            move |_messages| {
                let t = task.clone();
                async move { Ok::<String, AiError>(t.to_string()) }
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.attempts, 1);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.task["name"], "测试登录");
    }

    #[tokio::test]
    async fn test_generate_retry_after_validation_failure() {
        // 第一轮输出缺 name（校验失败），第二轮输出合法任务
        let bad = json!({ "url": "{{LOGIN_URL}}", "steps": [] });
        let good = valid_task();
        let call = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call2 = call.clone();
        let outcome =
            generate_with(
                &ctx(),
                None,
                |v: &Value| {
                    let empty_name = v
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::is_empty)
                        .unwrap_or(true);
                    async move {
                        if empty_name {
                            Err(vec!["name 不能为空".to_string()])
                        } else {
                            Ok(())
                        }
                    }
                },
                move |_messages| {
                    let n = call2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let b = bad.clone();
                    let g = good.clone();
                    async move {
                        Ok::<String, AiError>(if n == 0 { b.to_string() } else { g.to_string() })
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.attempts, 2);
        assert!(!outcome.warnings.is_empty());
        assert_eq!(outcome.task["name"], "测试登录");
    }

    #[tokio::test]
    async fn test_generate_exhausts_attempts_with_error_list() {
        let bad = json!({ "url": "{{LOGIN_URL}}", "steps": [] });
        let result = generate_with(
            &ctx(),
            None,
            |_v| async { Err(vec!["name 不能为空".to_string()]) },
            move |_messages| {
                let b = bad.clone();
                async move { Ok::<String, AiError>(b.to_string()) }
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                AiError::ValidationExhausted {
                    max_attempts: 2,
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("name 不能为空"), "{err}");
    }

    #[tokio::test]
    async fn test_generate_chat_error_propagates() {
        let result = generate_with(
            &ctx(),
            None,
            |_v| async { Ok(()) },
            |_messages| async {
                Err::<String, AiError>(AiError::LlmServiceError {
                    status: reqwest::StatusCode::UNAUTHORIZED,
                    snippet: "unauthorized".into(),
                })
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(result, AiError::ChatAttemptFailed { attempt: 1, .. }),
            "应包装轮次上下文，实际 {result:?}"
        );
        assert!(result.to_string().contains("401"));
    }

    #[test]
    fn test_generated_task_rejects_identity_script_kind_and_file_upload() {
        let errors = validate_generated_task(&json!({
            "task_id": "default",
            "type": "script",
            "url": "https://example.com",
            "steps": [{"type": "upload_file"}]
        }))
        .unwrap_err();
        assert!(errors.iter().any(|error| error.contains("task_id")));
        assert!(errors.iter().any(|error| error.contains("browser")));
        assert!(errors.iter().any(|error| error.contains("LOGIN_URL")));
        assert!(errors.iter().any(|error| error.contains("upload_file")));
    }
}
