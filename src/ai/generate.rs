//! 生成编排：调 LLM → 抽取任务 JSON → 强校验 → 错误回喂自纠一轮
//!
//! 编排不直接依赖 tasks 模块（避免 ai → tasks 的反向耦合），校验以闭包注入；
//! LLM 调用同理以异步函数指针注入，单测可用桩函数覆盖全流程。

use serde_json::Value;

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
pub fn extract_json(text: &str) -> Result<Value, String> {
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
    Err(format!(
        "无法从模型输出中提取 JSON（输出前 200 字符: {}）",
        text.chars().take(200).collect::<String>()
    ))
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
) -> Result<GenerateOutcome, String>
where
    V: Fn(&Value) -> Fut,
    Fut: std::future::Future<Output = Result<(), Vec<String>>>,
    C: Fn(Vec<Value>) -> CFut,
    CFut: std::future::Future<Output = Result<String, String>>,
{
    let mut messages = prompt::build_messages(ctx, extra_prompt);
    let mut warnings = Vec::new();
    let mut last_errors: Vec<String> = Vec::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let text = chat(messages.clone())
            .await
            .map_err(|e| format!("第 {attempt} 轮生成失败: {e}"))?;
        let task = extract_json(&text)?;
        match validate(&task).await {
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

    Err(format!(
        "连续 {MAX_ATTEMPTS} 轮生成均未通过任务校验，最后错误：\n{}",
        last_errors.join("\n")
    ))
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
            scripts: Vec::new(),
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
                async move { Ok(t.to_string()) }
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
        let bad = json!({ "steps": [] });
        let good = valid_task();
        let call = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call2 = call.clone();
        let outcome = generate_with(
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
                async move { Ok(if n == 0 { b.to_string() } else { g.to_string() }) }
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
        let bad = json!({ "steps": [] });
        let result = generate_with(
            &ctx(),
            None,
            |_v| async { Err(vec!["name 不能为空".to_string()]) },
            move |_messages| {
                let b = bad.clone();
                async move { Ok(b.to_string()) }
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(err.contains("name 不能为空"), "{err}");
        assert!(err.contains("连续 2 轮"), "{err}");
    }

    #[tokio::test]
    async fn test_generate_chat_error_propagates() {
        let result = generate_with(
            &ctx(),
            None,
            |_v| async { Ok(()) },
            |_messages| async { Err("LLM 服务返回 401".to_string()) },
        )
        .await
        .unwrap_err();
        assert!(result.contains("401"));
    }
}
