//! 提示词组装：系统提示词（任务 schema 浓缩指南）+ 用户消息（页面上下文 + 截图）
//!
//! 设计取舍：docs/guides/task-writing-guide.md 全文 565 行直接进 prompt 偏贵，
//! 此处固定一份浓缩版 schema 指南（覆盖步骤类型、必填字段、占位符与输出约束），
//! 与强校验 `validate_task` 的硬性规则一一对应——提示词约束失守时仍有校验兜底。

use serde_json::{Value, json};

/// 页面上下文截断预算（字符数）。门户页 HTML/JS 可能数 MB，视觉模型上下文有限：
/// HTML 保骨架与内联脚本，外链 JS 按登录相关关键词优先、逐文件截断、总量封顶。
pub const HTML_MAX_CHARS: usize = 80_000;
/// 单个外部脚本文件的最大字符数
pub const SCRIPT_MAX_CHARS: usize = 40_000;
/// 全部外部脚本的累计字符预算
pub const SCRIPTS_TOTAL_CHARS: usize = 120_000;

/// 登录实现线索：文件名/URL 含这些关键词的外链脚本优先纳入上下文
/// （门户登录加密几乎总是 MD5/Base64/AES + 盐值，藏在含此类命名的脚本里）
const LOGIN_SCRIPT_HINTS: [&str; 10] = [
    "login", "auth", "encrypt", "md5", "base64", "aes", "des", "rsa", "crypt", "portal",
];

/// 系统提示词：任务 schema 浓缩指南（中文，与 `TaskManager::validate_task` 硬规则对齐）
pub const SYSTEM_PROMPT: &str = r#"你是校园网门户登录自动化专家。根据用户提供的登录页面截图、HTML 与 JS 源码，生成一个可被 Campus-Auth 执行器直接运行的浏览器任务 JSON。

## 输出要求（最高优先级）
只输出一个 JSON 对象，不要任何解释、markdown 围栏或多余文本。

## 任务 JSON 顶层结构
{
  "name": "任务名（中文，简短）",
  "url": "{{LOGIN_URL}}",
  "timeout": 30000,
  "navigation_wait": 1.0,
  "step_delay": 0.3,
  "variables": {},
  "steps": [ ... ],
  "success_condition": "登录成功判定变量名（可选，见 eval 步骤）"
}
- type 字段缺省即为 browser，无需写。
- url 固定用 "{{LOGIN_URL}}" 占位符（执行时由客户端注入真实登录页地址）。

## 凭据占位符（必须使用，禁止留空或写示例值）
- {{USERNAME}}：学号/账号
- {{PASSWORD}}：密码
- {{ISP}}：运营商（有运营商选择下拉框时用）
- {{LOGIN_URL}}：初始登录页地址

## 可用步骤类型（16 种）与必填字段
- input：填输入框。必填 selector、value。value 里填 {{USERNAME}}/{{PASSWORD}} 等占位符。
- click：点击。必填 selector。
- select：下拉选择。必填 selector、value（option 的 value 或文本）。
- click_select：点击后选择（如运营商下拉）。必填 selector、option_selector。
- wait：等待元素出现或休眠。有 selector 则等元素，否则按 duration(ms) 休眠。
- wait_for_selector：等待元素。必填 selector。
- wait_url：等待 URL 变化。必填 pattern（URL 包含的子串）。
- sleep：休眠。duration 毫秒。
- navigate / goto：跳转。必填 url、value、selector 之一。
- eval / custom_js：执行 JS。必填 script。可将结果存变量：加 "store_as": "变量名"（供 success_condition 判定）。
- ocr：验证码识别（ddddocr）。必填 selector（验证码图片元素）；target_selector 填识别结果的目标输入框（可选）。
- assert_text：断言页面文本。必填 selector、value。
- screenshot：截图（调试用）。
- upload_file：上传文件。必填 selector，且 path 或 value 二选一。

## 步骤公共字段
- id：必填，非空，唯一，仅 [a-zA-Z0-9_-]（如 "fill_username"）。
- description：中文一句话说明（可选但建议）。
- required：默认 true；可选步骤才设 false。
- frame：元素在 iframe 内时必填（填写 iframe 的 name，或 "url=特征子串"）。

## 常见门户模式提示
- 表单在 iframe 里时，所有相关步骤都要带 frame 字段。
- 密码框被 JS 加密（找 password、md5、encrypt、salt 相关代码）时，只需填明文占位符到输入框，加密由页面 JS 自动完成；不要用 eval 自己实现加密。
- 有验证码图片时用 ocr 步骤：selector 指向验证码 <img>，target_selector 指向验证码输入框。
- 提交后如需判定成功，可加一个 eval 步骤用 store_as 写入结果变量（如检查页面无错误提示返回 true），顶层 success_condition 填该变量名。
- 优先选择稳定的 selector（id、name、placeholder），避免脆弱的绝对路径。

## 你的目标
分析用户给出的登录页材料，生成输入账号密码（并处理运营商下拉/验证码/勾选协议等元素）后提交登录的最短可靠步骤序列。"#;

/// 捕获产物上下文（已从落盘文件读入内存）
pub struct CaptureContext {
    /// 请求 URL（用户输入）
    pub request_url: String,
    /// 实际落地 URL（重定向后的登录页地址）
    pub final_url: String,
    /// 页面标题
    pub title: String,
    /// 页面 HTML（原始 content）
    pub html: String,
    /// 外链脚本 (url, 内容)，CSS 已过滤
    pub scripts: Vec<(String, String)>,
    /// 截图 PNG 字节
    pub screenshot_png: Vec<u8>,
    /// 资源快照备注（截断说明等）
    pub note: Option<String>,
}

/// 组装生成请求的 messages（system + user[文本上下文 + 截图 image_url]）
///
/// `extra_prompt` 为用户在界面上补充的说明（可为空）。
pub fn build_messages(ctx: &CaptureContext, extra_prompt: Option<&str>) -> Vec<Value> {
    let user_text = build_user_text(ctx, extra_prompt);
    let data_url = format!("data:image/png;base64,{}", b64(&ctx.screenshot_png));
    vec![
        json!({ "role": "system", "content": SYSTEM_PROMPT }),
        json!({
            "role": "user",
            "content": [
                { "type": "text", "text": user_text },
                { "type": "image_url", "image_url": { "url": data_url } },
            ],
        }),
    ]
}

/// 组装回喂自纠消息：把上一轮输出与校验错误追加进对话
pub fn append_retry_messages(messages: &mut Vec<Value>, assistant_text: &str, errors: &[String]) {
    messages.push(json!({ "role": "assistant", "content": assistant_text }));
    messages.push(json!({
        "role": "user",
        "content": format!(
            "上述 JSON 未通过校验，问题如下：\n{}\n请修正后重新输出，仍然只输出完整的任务 JSON，不要解释。",
            errors.join("\n")
        ),
    }));
}

/// 组装用户文本上下文（含截断）
pub fn build_user_text(ctx: &CaptureContext, extra_prompt: Option<&str>) -> String {
    let mut sections: Vec<String> = Vec::new();

    sections.push(format!(
        "## 页面信息\n- 请求 URL: {}\n- 实际落地 URL: {}\n- 页面标题: {}",
        ctx.request_url, ctx.final_url, ctx.title
    ));

    sections.push(format!(
        "## 页面 HTML（可能已截断，共 {} 字符）\n```html\n{}\n```",
        ctx.html.chars().count(),
        truncate_chars(&ctx.html, HTML_MAX_CHARS)
    ));

    let scripts_text = build_scripts_text(&ctx.scripts);
    if !scripts_text.is_empty() {
        sections.push(format!(
            "## 已加载的 JS 脚本（按登录相关性筛选，可能已截断）\n{scripts_text}"
        ));
    }

    if let Some(note) = &ctx.note {
        sections.push(format!("## 捕获备注\n{note}"));
    }

    if let Some(extra) = extra_prompt {
        let extra = extra.trim();
        if !extra.is_empty() {
            sections.push(format!("## 用户补充说明\n{extra}"));
        }
    }

    sections.join("\n\n")
}

/// 拼接外链脚本内容：登录相关命名的脚本优先，单文件与总量双重截断
fn build_scripts_text(scripts: &[(String, String)]) -> String {
    let mut ordered: Vec<&(String, String)> = scripts.iter().collect();
    ordered.sort_by_key(|(url, _)| {
        let lower = url.to_lowercase();
        let hits = LOGIN_SCRIPT_HINTS
            .iter()
            .filter(|h| lower.contains(*h))
            .count();
        // 命中线索多的排前；同分按原顺序（stable sort）
        std::cmp::Reverse(hits)
    });

    let mut out = String::new();
    let mut budget = SCRIPTS_TOTAL_CHARS;
    for (url, content) in ordered {
        if budget == 0 {
            out.push_str(&format!(
                "\n（其余 {} 个脚本超出总量预算已省略）\n",
                scripts.len()
            ));
            break;
        }
        let text = truncate_chars(content, SCRIPT_MAX_CHARS.min(budget));
        budget = budget.saturating_sub(text.chars().count());
        out.push_str(&format!("\n### {url}\n```javascript\n{text}\n```\n"));
    }
    out.trim().to_string()
}

/// 按字符截断（非字节），保证不劈开 UTF-8 多字节字符
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n…（已截断）")
}

/// 标准 base64
fn b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CaptureContext {
        CaptureContext {
            request_url: "http://portal.example.com".into(),
            final_url: "http://portal.example.com/login".into(),
            title: "校园网登录".into(),
            html: "<html><body><form></form></body></html>".into(),
            scripts: Vec::new(),
            screenshot_png: vec![1, 2, 3],
            note: None,
        }
    }

    #[test]
    fn test_truncate_chars_multibyte_safe() {
        let s = "校园网".repeat(100);
        let cut = truncate_chars(&s, 10);
        assert!(cut.starts_with("校园网校园网校园网校"));
        assert!(cut.contains("已截断"));
        // 不超预算
        assert!(cut.chars().count() < 20);
    }

    #[test]
    fn test_scripts_sorted_by_login_hints_and_capped() {
        let scripts = vec![
            (
                "https://cdn.example.com/jquery.js".to_string(),
                "a".repeat(50),
            ),
            (
                "https://portal.example.com/js/login-md5.js".to_string(),
                "b".repeat(50),
            ),
        ];
        let text = build_scripts_text(&scripts);
        // login-md5 命中 2 个关键词，应排在 jquery 前
        let pos_login = text.find("login-md5").unwrap();
        let pos_jquery = text.find("jquery").unwrap();
        assert!(pos_login < pos_jquery);
    }

    #[test]
    fn test_scripts_budget_zero_stops() {
        let scripts: Vec<(String, String)> = (0..10)
            .map(|i| {
                (
                    format!("https://x/{i}/login.js"),
                    "c".repeat(SCRIPT_MAX_CHARS + 1),
                )
            })
            .collect();
        let text = build_scripts_text(&scripts);
        // 总预算 120k，单文件 40k → 最多 3 个文件
        assert_eq!(text.matches("### https://x/").count(), 3);
    }

    #[test]
    fn test_build_messages_contains_image_and_placeholders_doc() {
        let messages = build_messages(&ctx(), Some("运营商选择电信"));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        let content = messages[1]["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("http://portal.example.com/login"));
        assert!(text.contains("运营商选择电信"));
        assert!(text.contains("<html>"));
        let img = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(img.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn test_append_retry_messages_appends_pair() {
        let mut messages = build_messages(&ctx(), None);
        let before = messages.len();
        append_retry_messages(&mut messages, "{\"broken\":1}", &["name 不能为空".into()]);
        assert_eq!(messages.len(), before + 2);
        assert_eq!(messages[messages.len() - 2]["role"], "assistant");
        let last = messages[messages.len() - 1]["content"].as_str().unwrap();
        assert!(last.contains("name 不能为空"));
    }
}
