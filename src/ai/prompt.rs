//! 提示词组装：系统提示词（任务 schema 浓缩指南）+ 用户消息（页面上下文 + 截图）
//!
//! 设计取舍：docs/guides/task-writing-guide.md 全文 565 行直接进 prompt 偏贵，
//! 此处固定一份浓缩版 schema 指南（覆盖步骤类型、必填字段、占位符与输出约束），
//! 与强校验 `validate_task` 的硬性规则一一对应——提示词约束失守时仍有校验兜底。
//! HTML 以登录表单为中心开窗口（头部 CSS/导航可能数十 KB，硬截头部会丢表单）；
//! JS/CSS 不进上下文（体积大、价值低），完整资源由「保存页面文件」按钮下载。

use serde_json::{Value, json};

/// 页面 HTML 截断预算（字符数）。视觉模型上下文有限，超预算按表单中心开窗口。
pub const HTML_MAX_CHARS: usize = 80_000;
/// 窗口不对称比例：锚点前 3/10、后 7/10——提交按钮/协议勾选/内联脚本在表单之后，向后偏重
const WINDOW_BEFORE_TENTHS: usize = 3;

/// 系统提示词：任务 schema 浓缩指南（中文，与 `TaskManager::validate_task` 硬规则对齐）
pub const SYSTEM_PROMPT: &str = r#"你是校园网门户登录自动化专家。根据用户提供的登录页面截图与 HTML 片段，生成一个可被 Campus-Auth 执行器直接运行的浏览器任务 JSON。

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
- 密码若由页面 JS 自动加密，直接填明文占位符到输入框即可，加密由页面自身完成；不要用 eval 自己实现加密。
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

/// 组装用户文本上下文（HTML 按表单中心开窗口）
pub fn build_user_text(ctx: &CaptureContext, extra_prompt: Option<&str>) -> String {
    let mut sections: Vec<String> = Vec::new();

    sections.push(format!(
        "## 页面信息\n- 请求 URL: {}\n- 实际落地 URL: {}\n- 页面标题: {}",
        ctx.request_url, ctx.final_url, ctx.title
    ));

    let (html_text, html_note) = windowed_html_with_note(&ctx.html, HTML_MAX_CHARS);
    sections.push(format!(
        "## 页面 HTML（{}，原始共 {} 字符）\n```html\n{}\n```",
        html_note,
        ctx.html.chars().count(),
        html_text
    ));

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

/// HTML 截断：预算内原文直出；超预算以登录表单为中心开窗口，找不到锚点回退从头截断
///
/// 返回 (文本, 给模型的裁剪说明)。
pub fn windowed_html_with_note(html: &str, budget: usize) -> (String, String) {
    if html.chars().count() <= budget {
        return (html.to_string(), "完整".to_string());
    }
    let Some(anchor) = find_login_anchor(html) else {
        return (truncate_chars(html, budget), "超长已从头部截断".to_string());
    };
    let chars: Vec<char> = html.chars().collect();
    let total = chars.len();
    let before = budget * WINDOW_BEFORE_TENTHS / 10;
    let mut start = anchor.saturating_sub(before);
    let mut end = (anchor + (budget - before)).min(total);

    // 边界修正：起点落在标签内部时回退到该标签的 '<'；终点落在标签内部时
    // 前进到当前标签的 '>' 之后，保证窗口首尾都是完整的标签边界
    if start > 0 {
        let lt = chars[..start].iter().rposition(|&c| c == '<');
        let gt = chars[..start].iter().rposition(|&c| c == '>');
        match (lt, gt) {
            (Some(l), Some(g)) if l > g => start = l,
            (Some(l), None) => start = l,
            _ => {}
        }
    }
    let lt = chars[start..end].iter().rposition(|&c| c == '<');
    let gt = chars[start..end].iter().rposition(|&c| c == '>');
    if let (Some(l), Some(g)) = (lt, gt) {
        if l > g {
            end = (start + g + 1).min(total);
        }
    }

    let head_omitted = start;
    let tail_omitted = total - end;
    let window: String = chars[start..end].iter().collect();
    let text = format!(
        "<!-- 前方已省略 {head_omitted} 字符（页面头部） -->\n{window}\n<!-- 后方已省略 {tail_omitted} 字符 -->"
    );
    (text, "已以登录表单为中心截取".to_string())
}

/// 在 HTML 中定位登录表单锚点（返回字符偏移），按信号强度逐级降级：
/// 密码输入框 > 用户名/账号类字段 > 提交按钮/登录文案。全部未命中返回 None。
fn find_login_anchor(html: &str) -> Option<usize> {
    // 小写副本上扫描（ASCII 小写不改变字节长度，偏移换算安全；中文不受影响）
    let lower = html.to_lowercase();
    let byte_to_char = |pos: usize| html[..pos].chars().count();

    // 1) 密码框（引号 / 无引号两种写法）
    for needle in ["type=\"password\"", "type=password", "type='password'"] {
        if let Some(pos) = lower.find(needle) {
            return Some(byte_to_char(pos));
        }
    }

    // 2) 用户名/账号类字段：多个词取最早命中
    let mut best: Option<usize> = None;
    for needle in [
        "username",
        "user_name",
        "account",
        "userid",
        "user-id",
        "学号",
        "工号",
        "账号",
        "用户名",
    ] {
        if let Some(pos) = lower.find(needle) {
            let char_pos = byte_to_char(pos);
            best = Some(best.map_or(char_pos, |b: usize| b.min(char_pos)));
        }
    }
    if best.is_some() {
        return best;
    }

    // 3) 提交按钮 / 登录文案
    for needle in ["type=\"submit\"", "type=submit", "登录", "login", "sign in"] {
        if let Some(pos) = lower.find(needle) {
            return Some(byte_to_char(pos));
        }
    }
    None
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

    // ---- 表单中心窗口 ----

    /// 构造"长头部 + 表单 + 长尾部"的 HTML：头部 head 填充、中部 password 表单、尾部脚本
    fn big_html(head_chars: usize, tail_chars: usize) -> String {
        let mut s = String::from("<html><head>");
        s.push_str(&"x".repeat(head_chars));
        s.push_str("</head><body><form><input id=\"username\"><input type=\"password\" name=\"pwd\"><button type=\"submit\">登录</button></form><script>");
        s.push_str(&"y".repeat(tail_chars));
        s.push_str("</script></body></html>");
        s
    }

    #[test]
    fn test_windowed_html_short_untouched() {
        let (text, note) = windowed_html_with_note("<p>hi</p>", 80_000);
        assert_eq!(text, "<p>hi</p>");
        assert_eq!(note, "完整");
    }

    #[test]
    fn test_windowed_html_centers_on_password_field() {
        let html = big_html(100_000, 5_000);
        let (text, note) = windowed_html_with_note(&html, 1_000);
        assert_eq!(note, "已以登录表单为中心截取");
        // 密码框与提交按钮必须保留在窗口内
        assert!(text.contains("type=\"password\""), "表单被截掉: {text}");
        assert!(text.contains("submit"));
        // 尾部 5000 字符脚本只应按预算部分进入窗口，不能整段包含
        assert!(!text.contains(&"y".repeat(2_000)), "尾部脚本不应整段进入窗口");
        // 头部大量内容被省略
        assert!(text.contains("前方已省略"));
    }

    #[test]
    fn test_windowed_html_tail_budget_when_form_early() {
        // 表单在文档很靠前、其后是超长脚本：窗口向脚本侧扩展（后 70% 预算）
        let html = big_html(1_000, 100_000);
        let (text, _) = windowed_html_with_note(&html, 1_000);
        assert!(text.contains("type=\"password\""));
        assert!(text.contains("后方已省略"));
    }

    #[test]
    fn test_windowed_html_falls_back_to_head_truncation_without_anchor() {
        let html = format!("<html><head>{}{}", "z".repeat(90_000), "</head></html>");
        let (text, note) = windowed_html_with_note(&html, 1_000);
        assert_eq!(note, "超长已从头部截断");
        assert!(text.starts_with("<html><head>"));
        assert!(text.contains("已截断"));
    }

    #[test]
    fn test_windowed_html_boundaries_on_tag_edges() {
        // 窗口边界必须落在 '<' / '>' 上：去掉省略注释后 '<' 与 '>' 数量配平
        let html = big_html(50_000, 50_000);
        let (text, _) = windowed_html_with_note(&html, 2_000);
        let body = text
            .lines()
            .filter(|l| !l.starts_with("<!--") && !l.ends_with("-->"))
            .collect::<String>();
        assert_eq!(body.matches('<').count(), body.matches('>').count());
    }

    #[test]
    fn test_windowed_html_multibyte_anchor_safe() {
        // 锚点是中文文案（"账号"），中文与窗口边界混合时不劈字符
        let mut html = String::from("<html><body><div>");
        html.push_str(&"页".repeat(50_000));
        html.push_str("<input name=\"username\" placeholder=\"请输入账号\">");
        html.push_str(&"页".repeat(50_000));
        html.push_str("</div></body></html>");
        let (text, note) = windowed_html_with_note(&html, 800);
        assert_eq!(note, "已以登录表单为中心截取");
        assert!(text.contains("username"));
        // 未劈开 UTF-8（劈开会产生替换符 U+FFFD）
        assert!(!text.contains('\u{FFFD}'));
    }

    #[test]
    fn test_build_messages_contains_image_and_no_scripts() {
        let messages = build_messages(&ctx(), Some("运营商选择电信"));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        let content = messages[1]["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("http://portal.example.com/login"));
        assert!(text.contains("运营商选择电信"));
        assert!(text.contains("<html>"));
        assert!(!text.contains("### "), "JS 段已移出上下文");
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
