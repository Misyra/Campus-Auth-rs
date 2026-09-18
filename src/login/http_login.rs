//! 直连登录执行器：按 Profile 的直连配置构造并发送登录请求。
//!
//! 与浏览器渠道（Python Worker + Playwright）完全独立：整个流程在 Rust 进程内
//! 完成，不要求 Python 环境与浏览器就绪。流水线：
//!
//! 1. （配置了加密脚本时）抓取登录页原文，供脚本从页面取盐值等参数
//! 2. 用户加密脚本：内置 boa 引擎在无网络/文件沙箱中执行 `transform(ctx)`
//! 3. 模板替换：URL/请求头/请求体中的 `{username}` `{password}` 与脚本返回的
//!    任意字段按名替换（值原样替换不转义，特殊字符可用 `url_encode()`）
//! 4. 发送请求（不跟随系统代理，重定向上限 5 跳）
//! 5. 成败判定：失败关键字命中 → 终态失败；成功关键字命中（或留空时 HTTP 2xx）
//!    → 交由会话状态机做登录后网络探测复核
//!
//! 判定结果映射为既有 `Outcome` 复用会话状态机的重试/历史/通知逻辑；
//! 日志与返回消息中的凭证与派生值一律脱敏（见 [`collect_secrets`]）——
//! 含 reqwest 错误消息（其 `Display` 会拼上完整 URL，GET 渠道下即含明文凭据）。
//! 响应体按 `Content-Type` charset 解码、UTF-8 → GBK 兜底，兼容中文门户。

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use boa_engine::{Context, JsValue, NativeFunction, Source};
use futures::StreamExt;
use hmac::Mac;
use reqwest::redirect::Policy;
use serde_json::json;
use sha2::Digest;
use zeroize::Zeroizing;

use crate::bridge::{Outcome, StructuredResult};
use crate::config::{HttpLoginMethod, ProfileSnapshot};

/// 响应体展示/判定的读取上限（字节）：门户响应通常极小，超限部分截断
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// 登录请求重定向跟随上限
const MAX_REDIRECTS: usize = 5;
/// 登录页抓取超时（best effort，失败不影响主流程）
const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// 登录请求总超时
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 用户脚本执行墙钟上限（防死循环拖死会话；引擎内另有指令数兜底）
const SCRIPT_TIMEOUT: Duration = Duration::from_millis(500);
/// 消息中响应片段的最大长度
const SNIPPET_LEN: usize = 240;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_HEADERS_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;
const MAX_PATTERN_BYTES: usize = 8 * 1024;
const MAX_SCRIPT_BYTES: usize = 128 * 1024;
/// 响应头回显上限（字节）：门户响应头通常只有几百字节，超限截断防异常门户撑爆面板
const MAX_RESPONSE_HEADERS_BYTES: usize = 8 * 1024;
/// 未配置 User-Agent 时使用的兜底值。
///
/// reqwest 不设置该头便完全不发 `User-Agent`，部分门户/WAF 会因此返回 403
/// 或另一套页面（而浏览器渠道总有 UA），表现为「抓包看不出问题、直连就是失败」。
/// 用常见浏览器标识兜底，用户可在请求头里显式覆盖。
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 一次直连登录尝试的完整请求参数（由 Profile 快照或测试端点构造）
#[derive(Clone)]
pub(crate) struct HttpLoginRequest {
    /// 请求方法
    pub method: HttpLoginMethod,
    /// 请求 URL（完整地址）
    pub url: String,
    /// 请求头模板（每行 `Key: Value`）
    pub headers: String,
    /// 请求体模板（POST 使用）
    pub body: String,
    /// 成功判定关键字（空 = HTTP 2xx 即成功）
    pub success_pattern: String,
    /// 失败判定关键字（命中即终态失败）
    pub failure_pattern: String,
    /// 加密脚本（空 = 不变换）
    pub crypto_script: String,
    /// 登录用户名
    pub username: String,
    /// 登录密码（Zeroizing 保护）
    pub password: Zeroizing<String>,
    /// 认证地址（供脚本 ctx.auth_url 与登录页抓取）
    pub auth_url: String,
    /// 本机主用接口 IPv4（供脚本 ctx.local_ip；取不到时为空串）
    pub local_ip: String,
    /// 本机主用接口 MAC（供脚本 ctx.local_mac；取不到时为空串）
    pub local_mac: String,
    /// 执行脚本前是否抓取认证页原文（正式登录默认开启，测试端点可关闭）
    pub fetch_page: bool,
    /// 是否忽略 HTTPS 证书错误（调用方已按「方案覆盖 ?? 全局 browser.ignore_https_errors」
    /// 解析为确定值；测试端点同样传解析后的值）。
    ///
    /// 校园网门户大量使用自签名证书，浏览器渠道默认忽略证书错误即可登录；
    /// 直连固定严格校验会造成「同一门户浏览器能登、直连必失败且报错晦涩」。
    pub ignore_https_errors: bool,
}

impl HttpLoginRequest {
    /// 由 Profile 快照构造直连请求参数（仅 login_channel = http 时调用）
    ///
    /// `global_ignore_https_errors` 为全局 `browser.ignore_https_errors`：
    /// 方案未显式设置 `http_ignore_https_errors` 时沿用它，保证与浏览器渠道同口径。
    pub fn from_profile(
        profile: &ProfileSnapshot,
        global_ignore_https_errors: bool,
    ) -> Result<Self, String> {
        if profile.http_url.trim().is_empty() {
            return Err("直连请求 URL 为空，请在方案里填写".into());
        }
        Self::validate_url(&profile.http_url)?;
        let request = Self {
            method: profile.http_method,
            url: profile.http_url.trim().to_string(),
            headers: profile.http_headers.clone(),
            body: profile.http_body.clone(),
            success_pattern: profile.http_success_pattern.clone(),
            failure_pattern: profile.http_failure_pattern.clone(),
            crypto_script: profile.http_crypto_script.clone(),
            username: profile.username.trim().to_string(),
            password: Zeroizing::new(profile.password.to_string()),
            auth_url: profile.auth_url.trim().to_string(),
            // 本机地址需异步查询网卡，由调用方（持有 MonitorService）按需填充，
            // 见 [`HttpLoginRequest::with_local_address`]
            local_ip: String::new(),
            local_mac: String::new(),
            fetch_page: true,
            ignore_https_errors: profile
                .http_ignore_https_errors
                .unwrap_or(global_ignore_https_errors),
        };
        request.validate()?;
        Ok(request)
    }

    /// URL 基础校验：仅允许 http/https（网关为校园网内网地址，私网段放行）
    pub fn validate_url(raw: &str) -> Result<(), String> {
        let parsed =
            url::Url::parse(raw.trim()).map_err(|e| format!("直连请求 URL 无法解析: {e}"))?;
        match parsed.scheme() {
            "http" | "https" if parsed.host_str().is_some_and(|host| !host.is_empty()) => Ok(()),
            "http" | "https" => Err("直连请求 URL 缺少主机名".into()),
            other => Err(format!("直连请求 URL 仅支持 http/https，当前为 {other}")),
        }
    }

    /// 校验直连配置的协议与体积边界，避免异常配置造成过量内存/脚本开销。
    pub fn validate(&self) -> Result<(), String> {
        Self::validate_url(&self.url)?;
        Self::validate_templates(
            &self.url,
            &self.headers,
            &self.body,
            &self.success_pattern,
            &self.failure_pattern,
            &self.crypto_script,
        )
    }

    /// 纯模板体积校验（不含 URL 合法性）：保存路径使用。
    ///
    /// 保存与执行必须同一口径——此前只在执行/测试时校验体积，超限配置能静默
    /// 落盘，用户要到真正登录失败才知道（且失败文案指向"过长"而非"保存被拒"，
    /// 无从判断是保存没生效还是配置本来就不对）。
    ///
    /// `url` 单独校验合法性由调用方负责（保存路径允许空串=尚未配置）。
    pub fn validate_templates(
        url: &str,
        headers: &str,
        body: &str,
        success_pattern: &str,
        failure_pattern: &str,
        crypto_script: &str,
    ) -> Result<(), String> {
        let checks = [
            ("直连请求 URL", url.len(), MAX_URL_BYTES),
            ("直连请求头", headers.len(), MAX_HEADERS_BYTES),
            ("直连请求体", body.len(), MAX_REQUEST_BODY_BYTES),
            ("成功关键字", success_pattern.len(), MAX_PATTERN_BYTES),
            ("失败关键字", failure_pattern.len(), MAX_PATTERN_BYTES),
            ("加密脚本", crypto_script.len(), MAX_SCRIPT_BYTES),
        ];
        for (label, actual, limit) in checks {
            if actual > limit {
                return Err(format!("{label}过长（最多 {limit} 字节）"));
            }
        }
        Ok(())
    }

    /// 是否存在用户加密脚本。
    ///
    /// 调用方据此决定是否值得查询本机地址（见 [`Self::with_local_address`]）：
    /// 无脚本时脚本根本不会执行，`local_ip`/`local_mac` 也就无人读取。
    pub fn uses_crypto_script(&self) -> bool {
        !self.crypto_script.trim().is_empty()
    }

    /// 填充本机地址（供脚本 `ctx.local_ip` / `ctx.local_mac`）。
    ///
    /// 仅在配置了加密脚本时才值得调用：网卡查询要 spawn `ipconfig`/`ip` 子进程
    /// （带 30s 缓存），无脚本时登录流程根本不会读这两个字段，白跑一次探测。
    pub fn with_local_address(mut self, addr: &crate::network::LocalAddress) -> Self {
        self.local_ip = addr.ipv4.clone();
        self.local_mac = addr.mac.clone();
        self
    }
}

/// 一次直连尝试的执行报告（登录会话与测试端点共用）
#[derive(Debug)]
pub(crate) struct HttpAttemptReport {
    /// 结果分类
    pub outcome: Outcome,
    /// 人类可读消息（已脱敏）
    pub message: String,
    /// 渲染后的请求 URL（已脱敏）
    pub rendered_url: String,
    /// 渲染后的请求头（已脱敏，`Key: Value` 逐行）
    pub rendered_headers: String,
    /// 渲染后的请求体（已脱敏）
    pub rendered_body: String,
    /// 响应状态码（请求失败时为 None）
    pub status: Option<u16>,
    /// 响应头逐行文本（已脱敏；排查 Content-Type/charset/跳转类问题时必需）
    pub response_headers: String,
    /// 响应体片段（已脱敏）
    pub response_snippet: String,
    /// 脚本执行错误（成功执行时为 None）
    pub script_error: Option<String>,
    /// 耗时（毫秒）
    pub duration_ms: u64,
}

impl HttpAttemptReport {
    /// 映射为会话状态机消费的结构化结果
    pub fn to_structured(&self) -> StructuredResult {
        StructuredResult {
            outcome: self.outcome,
            message: self.message.clone(),
            data: json!({
                "http_status": self.status,
                "response_snippet": self.response_snippet,
            }),
            screenshot_url: None,
            duration_ms: self.duration_ms,
        }
    }
}

/// 执行一次直连登录尝试（不发网络验证，验证由会话状态机负责）
pub(crate) async fn run_once(req: &HttpLoginRequest) -> HttpAttemptReport {
    let start = Instant::now();

    // 1. 用户脚本值变换：产出可被占位符引用的字段表
    let mut vars = BTreeMap::new();
    vars.insert("username".to_string(), req.username.clone());
    vars.insert("password".to_string(), req.password.to_string());
    vars.insert("auth_url".to_string(), req.auth_url.clone());
    // 本机地址同样注册为占位符：脚本可以不用 ctx 而直接在 URL/body 里写
    // {local_ip}，也能在 transform 中引用；取不到时为空串（脚本须容忍）
    vars.insert("local_ip".to_string(), req.local_ip.clone());
    vars.insert("local_mac".to_string(), req.local_mac.clone());

    let mut script_error = None;
    if req.uses_crypto_script() {
        // 登录页原文 best effort 抓取：失败置空串，脚本须容忍缺失
        let page = if req.fetch_page {
            fetch_login_page(&req.auth_url, req.ignore_https_errors).await
        } else {
            String::new()
        };
        match execute_crypto_script(
            &req.crypto_script,
            &req.username,
            &req.password,
            &req.auth_url,
            (&req.local_ip, &req.local_mac),
            page,
        )
        .await
        {
            Ok(values) => {
                for (k, v) in values {
                    if k == "username" || k == "password" || vars.contains_key(&k) {
                        // 覆盖内置字段是合法用法（如盐值拼接后的密码）
                        tracing::debug!("脚本字段覆盖内置占位符: {k}");
                    }
                    vars.insert(k, v);
                }
            }
            Err(e) => {
                script_error = Some(e);
            }
        }
    }

    // 脚本失败直接终态：凭证变换错误时发出去的请求必错，重试无意义
    if let Some(e) = &script_error {
        return HttpAttemptReport {
            outcome: Outcome::UnknownError,
            message: format!("加密脚本执行失败: {e}"),
            rendered_url: String::new(),
            rendered_headers: String::new(),
            rendered_body: String::new(),
            status: None,
            response_headers: String::new(),
            response_snippet: String::new(),
            script_error: Some(e.clone()),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // 2. 模板渲染
    let rendered_url = substitute(&req.url, &vars);
    let rendered_headers = substitute(&req.headers, &vars);
    let rendered_body = substitute(&req.body, &vars);

    // 3. 发送请求
    let send = send_request(req, &rendered_url, &rendered_headers, &rendered_body).await;
    let (status, body, response_headers) = match send {
        Ok(triple) => triple,
        Err(e) => {
            let secrets = collect_secrets(&vars);
            // reqwest 的 Error::Display 会把完整 URL 拼进消息（"for url (...)"），
            // GET 渠道下 URL 含明文凭据，必须先脱敏再进 message——它会流入日志、
            // 登录历史与前端；rendered_url 的自有脱敏无法覆盖这条错误路径。
            return HttpAttemptReport {
                outcome: Outcome::NetworkError,
                message: redact_text(&format!("直连请求失败: {e}"), &secrets),
                rendered_url: redact_url(&rendered_url, &secrets),
                rendered_headers: redact_text(&rendered_headers, &secrets),
                rendered_body: redact_text(&rendered_body, &secrets),
                status: None,
                response_headers: String::new(),
                response_snippet: String::new(),
                script_error,
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    // 4. 成败判定
    let secrets = collect_secrets(&vars);
    let body_text = body;
    let failure_hit =
        !req.failure_pattern.trim().is_empty() && body_text.contains(req.failure_pattern.trim());
    let success_pattern = req.success_pattern.trim();
    let success = if failure_hit {
        false
    } else if success_pattern.is_empty() {
        status.is_success()
    } else {
        body_text.contains(success_pattern)
    };

    let snippet = truncate_snippet(&body_text);
    let (outcome, message) = if failure_hit {
        (
            Outcome::InvalidCredential,
            format!(
                "门户返回失败标识（HTTP {status}）: {}",
                redact_text(&snippet, &secrets)
            ),
        )
    } else if success {
        (Outcome::Success, format!("直连请求成功（HTTP {status}）"))
    } else {
        (
            Outcome::AssertionFailed,
            format!(
                "未命中成功标识（HTTP {status}）: {}",
                redact_text(&snippet, &secrets)
            ),
        )
    };

    HttpAttemptReport {
        outcome,
        message,
        rendered_url: redact_url(&rendered_url, &secrets),
        rendered_headers: redact_text(&rendered_headers, &secrets),
        rendered_body: redact_text(&rendered_body, &secrets),
        status: Some(status.as_u16()),
        // 响应头同样按凭据字典脱敏：门户回显参数、回跳地址里可能带回提交过的凭据
        response_headers: redact_text(&response_headers, &secrets),
        response_snippet: redact_text(&snippet, &secrets),
        script_error,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// 构建直连请求客户端（登录请求与登录页抓取共用同一策略）。
///
/// 统一收口三件事，避免两处各自构造时策略漂移：
/// - 证书策略：按 `ignore_https_errors`（自签门户必需，与浏览器渠道同口径）
/// - 代理：显式 `no_proxy`（校园网网关是本机直连可达的内网地址，走代理必失败）
/// - User-Agent：未显式配置时补浏览器 UA（reqwest 默认完全不发该头）
fn build_client(timeout: Duration, ignore_https_errors: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(Policy::limited(MAX_REDIRECTS))
        .no_proxy()
        .danger_accept_invalid_certs(ignore_https_errors)
        .user_agent(DEFAULT_USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(|e| format!("客户端构建失败: {e}"))
}

/// 抓取登录页原文（best effort）：脚本 ctx.page 数据源，失败返回空串
async fn fetch_login_page(auth_url: &str, ignore_https_errors: bool) -> String {
    if auth_url.is_empty() {
        return String::new();
    }
    let client = match build_client(PAGE_FETCH_TIMEOUT, ignore_https_errors) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("登录页抓取客户端构建失败: {e}");
            return String::new();
        }
    };
    match client.get(auth_url).send().await {
        Ok(resp) => match read_limited_body(resp).await {
            Ok(body) => body.0,
            Err(e) => {
                tracing::warn!("登录页读取失败: {e}");
                String::new()
            }
        },
        Err(e) => {
            tracing::warn!("登录页抓取失败（脚本将收到空 page）: {e}");
            String::new()
        }
    }
}

/// 发送登录请求，返回 (状态码, 响应体原文, 响应头逐行文本)
async fn send_request(
    req: &HttpLoginRequest,
    url: &str,
    headers: &str,
    body: &str,
) -> Result<(reqwest::StatusCode, String, String), String> {
    let client = build_client(REQUEST_TIMEOUT, req.ignore_https_errors)?;

    let mut request = match req.method {
        HttpLoginMethod::Get => client.get(url),
        HttpLoginMethod::Post => {
            let mut r = client.post(url);
            if !body.is_empty() {
                r = r.body(body.to_string());
            }
            r
        }
    };

    // 请求头模板逐行解析；POST 带体且未显式指定 Content-Type 时补默认表单类型
    let parsed = parse_headers(headers);
    for (k, v) in &parsed {
        request = request.header(k, v);
    }
    if req.method == HttpLoginMethod::Post && !body.is_empty() {
        let has_content_type = parsed
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
        if !has_content_type {
            request = request.header("Content-Type", "application/x-www-form-urlencoded");
        }
    }

    let resp = request.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    // 响应头先快照再消费响应体（流式读取会拿走所有权）
    let headers_text = format_response_headers(resp.headers());
    let (body, _charset) = read_limited_body(resp).await?;
    Ok((status, body, headers_text))
}

/// 响应头 → 逐行 `Key: Value` 文本（截断到上限），供测试结果面板排查排查
fn format_response_headers(headers: &reqwest::header::HeaderMap) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        // 头值按可见字符展示；非 UTF-8（罕见）以 lossy 兜底，绝不因为一个头解析失败而丢整份回显
        let value = String::from_utf8_lossy(value.as_bytes());
        out.push_str(name.as_str());
        out.push_str(": ");
        out.push_str(value.trim());
        out.push('\n');
        if out.len() >= MAX_RESPONSE_HEADERS_BYTES {
            out.push_str("…（已截断）");
            break;
        }
    }
    out
}

/// 流式读取响应体并在 64 KiB 处停止，避免异常门户以超大响应撑高进程内存。
///
/// 解码先于流式读取取响应头 charset（消费 `resp` 后头字段不可再访问），
/// 一并返回实际使用的 charset 标签（未声明时为 None），供响应头回显核对编码。
async fn read_limited_body(resp: reqwest::Response) -> Result<(String, Option<String>), String> {
    let charset = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_charset);
    let mut stream = resp.bytes_stream();
    let mut bytes = Vec::with_capacity(MAX_RESPONSE_BYTES.min(8 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() == MAX_RESPONSE_BYTES {
            break;
        }
    }
    Ok((decode_body(&bytes, charset.as_deref()), charset))
}

/// 从 `Content-Type` 头解析 charset 参数（大小写不敏感；无该参数返回 None）
fn parse_charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['"', '\'']).to_string())
            .filter(|v| !v.is_empty())
    })
}

/// 解码响应体：优先按响应头声明 charset，其次 UTF-8，最后 GBK 兜底。
///
/// 国内校园网门户大量以 GBK（代码页 936）返回中文，且常省略 charset 声明；
/// 仅用 `from_utf8_lossy` 会把「登录成功」解成乱码，导致实际登录成功却被判为
/// 「未命中成功标识」。兜底口径与 `network::detect::decode_console_output`
/// 一致（同一 GBK 假设），保证中文关键字判定可用。
fn decode_body(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(encoding) =
        charset.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
    {
        return encoding.decode_without_bom_handling(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::GBK
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    }
}

/// 解析请求头模板：每行 `Key: Value`，空行与无冒号行忽略
fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (k, v) = line.split_once(':')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// 模板占位符替换：`{name}` 按 vars 查表；未知占位符原样保留便于发现拼写问题
fn substitute(template: &str, vars: &BTreeMap<String, String>) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = template[i + 1..].find('}') {
                let name = &template[i + 1..i + 1 + end];
                if is_placeholder_name(name) {
                    if let Some(v) = vars.get(name) {
                        out.push_str(v);
                        i += end + 2;
                        continue;
                    }
                }
            }
        }
        // 逐字符推进（多字节字符按字节步进安全：非 '{' 直接拷贝）
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&template[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// 占位符名合法字符：字母数字下划线连字符点
fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// 首字节对应 UTF-8 字符长度（非 ASCII 首字节按前导 1 位数推算，异常回退 1）
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// 截断响应片段
fn truncate_snippet(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = compact.chars().take(SNIPPET_LEN).collect();
    if compact.chars().count() > SNIPPET_LEN {
        s.push('…');
    }
    s
}

/// 汇总脱敏字典：凭证与全部脚本产出（含 URL 编码形态）
fn collect_secrets(vars: &BTreeMap<String, String>) -> Vec<String> {
    let mut secrets = Vec::new();
    for (key, v) in vars {
        // auth_url 是公开配置值，仅作为模板便利字段，不属于凭据或脚本产出。
        // local_ip / local_mac 同理：调用方自己的机器地址，非用户秘密；且它们
        // 常出现在「IP 不匹配」这类门户诊断文案里，脱敏会把排查线索一并遮掉。
        if matches!(key.as_str(), "auth_url" | "local_ip" | "local_mac") {
            continue;
        }
        if v.is_empty() {
            continue;
        }
        secrets.push(v.clone());
        let encoded = url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
        if encoded != *v {
            secrets.push(encoded);
        }
    }
    // 先替换长值，避免短值是长值前缀时只遮住前半段；去重减少重复扫描。
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.dedup();
    secrets
}

/// 文本脱敏：命中凭证（原样或 URL 编码形态）替换为 ***
fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets {
        out = out.replace(s, "***");
    }
    out
}

/// URL 脱敏（复用文本脱敏；URL 已渲染，直接按字典替换）
fn redact_url(url: &str, secrets: &[String]) -> String {
    redact_text(url, secrets)
}

/// 在无 IO 沙箱内执行用户加密脚本（阻塞调用，内部走 spawn_blocking + 墙钟超时）
///
/// 契约：脚本需定义 `function transform(ctx)`，返回对象；其字符串/数字/布尔
/// 字段成为可被模板引用的占位符值。ctx 含
/// `username/password/auth_url/page/local_ip/local_mac`。
///
/// `local` 为本机主用接口的 (IPv4, MAC)，取不到时均为空串——部分门户（eportal /
/// Dr.COM）的字段密钥由来源 IP 推导，没有它就只能从页面里找补。
async fn execute_crypto_script(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    auth_url: &str,
    local: (&str, &str),
    page: String,
) -> Result<BTreeMap<String, String>, String> {
    let script = script.to_string();
    let username = username.to_string();
    let password = Zeroizing::new(password.to_string());
    let auth_url = auth_url.to_string();
    let local_ip = local.0.to_string();
    let local_mac = local.1.to_string();

    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = run_script_in_sandbox(
            &script, &username, &password, &auth_url, &local_ip, &local_mac, page,
        );
        // 接收端已超时丢弃时发送失败，静默即可
        let _ = tx.send(result);
    });

    match tokio::time::timeout(SCRIPT_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("脚本执行线程异常退出".into()),
        Err(_) => Err(format!(
            "脚本执行超时（上限 {}ms）",
            SCRIPT_TIMEOUT.as_millis()
        )),
    }
}

/// boa 沙箱执行：注册内置函数 → eval 脚本 → 调用 transform(ctx) → 序列化返回值
#[allow(clippy::too_many_arguments)]
fn run_script_in_sandbox(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    auth_url: &str,
    local_ip: &str,
    local_mac: &str,
    page: String,
) -> Result<BTreeMap<String, String>, String> {
    let mut context = Context::default();

    // 沙箱限制：递归深度与循环次数双兜底，超出即抛错终止
    let limits = context.runtime_limits_mut();
    limits.set_recursion_limit(64);
    limits.set_loop_iteration_limit(100_000);

    register_builtins(&mut context)?;

    context
        .eval(Source::from_bytes(script.as_bytes()))
        .map_err(|e| format!("脚本语法/执行错误: {e}"))?;

    let ctx_value = JsValue::from_json(
        &json!({
            "username": username,
            "password": password.as_str(),
            "auth_url": auth_url,
            "page": page,
            "local_ip": local_ip,
            "local_mac": local_mac,
        }),
        &mut context,
    )
    .map_err(|e| format!("脚本输入构造失败: {e}"))?;

    let transform_obj = context
        .global_object()
        .get(boa_engine::JsString::from("transform"), &mut context)
        .map_err(|e| format!("读取 transform 失败: {e}"))?
        .as_object()
        .ok_or_else(|| "脚本未定义 function transform(ctx)".to_string())?;

    let result = transform_obj
        .call(&JsValue::undefined(), &[ctx_value], &mut context)
        .map_err(|e| format!("transform 调用失败: {e}"))?;

    // 0.22 的 to_json 返回 Option：undefined/null 视为无效返回
    let json = result
        .to_json(&mut context)
        .map_err(|e| format!("transform 返回值序列化失败: {e}"))?
        .ok_or_else(|| "transform 必须返回对象".to_string())?;
    let Some(map) = json.as_object() else {
        return Err("transform 必须返回对象".into());
    };

    let mut out = BTreeMap::new();
    for (k, v) in map {
        let value = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        out.insert(k.clone(), value);
    }
    Ok(out)
}

/// 注册脚本内置函数：散列/HMAC/编码/时间戳（均为纯计算，无 IO）
fn register_builtins(context: &mut Context) -> Result<(), String> {
    macro_rules! register {
        ($name:expr, $len:expr, $fn:expr) => {
            context
                .register_global_builtin_callable($name, $len, $fn)
                .map_err(|e| format!("内置函数 {} 注册失败: {e}", $name.to_std_string_escaped()))?;
        };
    }

    // 单参文本 → 摘要/编码类
    register!(
        boa_engine::JsString::from("md5"),
        1,
        text_digest_fn(|s| { format!("{:x}", md5::Md5::digest(s.as_bytes())) })
    );
    register!(
        boa_engine::JsString::from("sha1"),
        1,
        text_digest_fn(|s| { format!("{:x}", sha1::Sha1::digest(s.as_bytes())) })
    );
    register!(
        boa_engine::JsString::from("sha256"),
        1,
        text_digest_fn(|s| { format!("{:x}", sha2::Sha256::digest(s.as_bytes())) })
    );
    register!(
        boa_engine::JsString::from("hex_encode"),
        1,
        text_digest_fn(|s| { hex::encode(s) })
    );
    register!(
        boa_engine::JsString::from("base64_encode"),
        1,
        text_digest_fn(|s| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
        })
    );
    register!(
        boa_engine::JsString::from("base64_decode"),
        1,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            use base64::Engine as _;
            let value = js_arg(args, 0, ctx)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value.as_bytes())
                .map_err(|e| {
                    boa_engine::JsError::from_opaque(
                        boa_engine::JsString::from(format!("Base64 解码失败: {e}")).into(),
                    )
                })?;
            let decoded = String::from_utf8(bytes).map_err(|e| {
                boa_engine::JsError::from_opaque(
                    boa_engine::JsString::from(format!("Base64 结果不是 UTF-8: {e}")).into(),
                )
            })?;
            Ok(boa_engine::JsString::from(decoded).into())
        })
    );
    register!(
        boa_engine::JsString::from("url_encode"),
        1,
        text_digest_fn(|s| { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() })
    );

    // 双参：hmac_sha256(key, data) → hex
    register!(
        boa_engine::JsString::from("hmac_sha256"),
        2,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            let key = js_arg(args, 0, ctx)?;
            let data = js_arg(args, 1, ctx)?;
            type HmacSha256 = hmac::Hmac<sha2::Sha256>;
            let mut mac = HmacSha256::new_from_slice(key.as_bytes()).map_err(|e| {
                boa_engine::JsError::from_opaque(boa_engine::JsString::from(e.to_string()).into())
            })?;
            mac.update(data.as_bytes());
            Ok(boa_engine::JsString::from(format!("{:x}", mac.finalize().into_bytes())).into())
        })
    );

    // 零参：当前毫秒时间戳
    register!(
        boa_engine::JsString::from("now_ms"),
        0,
        NativeFunction::from_copy_closure(|_this, _args, _ctx| {
            Ok(JsValue::from(chrono::Utc::now().timestamp_millis() as f64))
        })
    );
    Ok(())
}

/// 构造"单参文本进、字符串出"的内置函数
fn text_digest_fn<F>(f: F) -> NativeFunction
where
    F: Fn(&str) -> String + Copy + 'static,
{
    NativeFunction::from_copy_closure(move |_this, args, ctx| {
        let s = js_arg(args, 0, ctx)?;
        Ok(boa_engine::JsString::from(f(&s)).into())
    })
}

/// 取第 index 个参数并转为 String（缺失/非字符串按空串处理）
fn js_arg(
    args: &[JsValue],
    index: usize,
    ctx: &mut Context,
) -> Result<String, boa_engine::JsError> {
    match args.get(index) {
        Some(v) if !v.is_null_or_undefined() => {
            let s = v.to_string(ctx)?;
            Ok(s.to_std_string_escaped())
        }
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn request(url: String) -> HttpLoginRequest {
        HttpLoginRequest {
            method: HttpLoginMethod::Get,
            url,
            headers: "X-User: {username}".into(),
            body: String::new(),
            success_pattern: "登录成功".into(),
            failure_pattern: "密码错误".into(),
            crypto_script: String::new(),
            username: "abc".into(),
            password: Zeroizing::new("abcdef".into()),
            auth_url: "http://portal.example/login".into(),
            local_ip: String::new(),
            local_mac: String::new(),
            fetch_page: false,
            // 与浏览器渠道默认口径一致（校园网门户多为自签名证书）
            ignore_https_errors: true,
        }
    }

    /// 起一个返回固定响应、并把收到的请求行/请求头回传给调用方的极简 HTTP 服务。
    ///
    /// 用于断言"实际发出的请求长什么样"（UA 兜底、自定义请求头覆盖等）——
    /// 只看响应侧无法证明请求头是否正确。
    async fn spawn_capturing_response(
        status: u16,
        body: &str,
        extra_headers: &str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        let extra_headers = extra_headers.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: text/plain; charset=utf-8\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = tx.send(received);
        });
        (format!("http://{addr}/login"), rx)
    }

    /// 未配置 User-Agent 时必须补浏览器 UA：reqwest 默认完全不发该头，
    /// 部分门户/WAF 据此返回 403 或另一套页面，表现为"抓包看不出问题但直连失败"。
    #[tokio::test]
    async fn request_sends_default_user_agent_when_not_configured() {
        let (url, received) = spawn_capturing_response(200, "登录成功", "").await;
        let report = run_once(&request(url)).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        let raw = received.await.unwrap();
        assert!(
            raw.to_ascii_lowercase().contains("user-agent: mozilla"),
            "未发出 User-Agent 兜底头:\n{raw}"
        );
    }

    /// 用户显式配置 User-Agent 时不得被兜底值覆盖
    #[tokio::test]
    async fn explicit_user_agent_header_wins_over_default() {
        let (url, received) = spawn_capturing_response(200, "登录成功", "").await;
        let mut req = request(url);
        req.headers = "User-Agent: CampusAuth-Test".into();
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        let raw = received.await.unwrap().to_ascii_lowercase();
        assert!(raw.contains("user-agent: campusauth-test"), "{raw}");
        assert!(
            !raw.contains("mozilla/5.0"),
            "兜底 UA 不应与显式配置同时发出:\n{raw}"
        );
    }

    /// 响应头必须回显（排查 Content-Type/charset/跳转问题时唯一线索），且同样脱敏
    #[tokio::test]
    async fn report_includes_redacted_response_headers() {
        let (url, _rx) = spawn_capturing_response(
            200,
            "登录成功 abcdef",
            "X-Portal-Echo: abcdef\r\nSet-Cookie: sid=abcdef\r\n",
        )
        .await;
        let report = run_once(&request(url)).await;
        // http crate 把响应头名归一为小写，断言按小写比对
        assert!(
            report.response_headers.contains("set-cookie:"),
            "响应头未回显: {:?}",
            report.response_headers
        );
        assert!(
            report.response_headers.contains("content-type:"),
            "响应头未含 Content-Type: {:?}",
            report.response_headers
        );
        assert!(
            !report.response_headers.contains("abcdef"),
            "响应头未脱敏，泄露凭据: {:?}",
            report.response_headers
        );
    }

    /// 保存路径的体积校验必须与执行路径同一口径（超限配置不得静默落盘）
    #[test]
    fn validate_templates_rejects_oversized_fields() {
        let ok = HttpLoginRequest::validate_templates("http://p/login", "", "", "", "", "");
        assert!(ok.is_ok());

        let long_script = "a".repeat(MAX_SCRIPT_BYTES + 1);
        let err = HttpLoginRequest::validate_templates("", "", "", "", "", &long_script)
            .expect_err("超限脚本必须被拒");
        assert!(err.contains("加密脚本过长"), "{err}");

        let long_body = "b".repeat(MAX_REQUEST_BODY_BYTES + 1);
        let err = HttpLoginRequest::validate_templates("", "", &long_body, "", "", "")
            .expect_err("超限请求体必须被拒");
        assert!(err.contains("直连请求体过长"), "{err}");
    }

    /// 证书策略缺省解析：方案未设置时跟随全局（与浏览器渠道同口径）
    #[test]
    fn from_profile_falls_back_to_global_cert_policy() {
        let mut profile = crate::config::ProfileSnapshot {
            id: "p".into(),
            name: String::new(),
            username: "u".into(),
            password: Zeroizing::new("pw".into()),
            auth_url: String::new(),
            trigger_url: String::new(),
            isp: String::new(),
            gateway_ip: String::new(),
            wifi_ssid: String::new(),
            active_task: String::new(),
            login_channel: crate::config::LoginChannel::Http,
            http_method: HttpLoginMethod::Get,
            http_url: "http://10.0.0.1/login".into(),
            http_headers: String::new(),
            http_body: String::new(),
            http_success_pattern: String::new(),
            http_failure_pattern: String::new(),
            http_crypto_script: String::new(),
            http_ignore_https_errors: None,
        };

        let followed = HttpLoginRequest::from_profile(&profile, true).unwrap();
        assert!(followed.ignore_https_errors, "未设置时应跟随全局 true");

        let followed_strict = HttpLoginRequest::from_profile(&profile, false).unwrap();
        assert!(
            !followed_strict.ignore_https_errors,
            "未设置时应跟随全局 false"
        );

        // 显式覆盖优先于全局
        profile.http_ignore_https_errors = Some(false);
        let overridden = HttpLoginRequest::from_profile(&profile, true).unwrap();
        assert!(!overridden.ignore_https_errors, "方案显式设置必须覆盖全局");
    }

    async fn spawn_response(status: u16, body: &str) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let reason = if status == 200 { "OK" } else { "ERROR" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/login?u={{username}}&p={{password}}")
    }

    #[test]
    fn substitute_preserves_unknown_and_unicode() {
        let vars = BTreeMap::from([("username".into(), "张三".into())]);
        assert_eq!(
            substitute("你好 {username} / {missing}", &vars),
            "你好 张三 / {missing}"
        );
    }

    #[test]
    fn parse_headers_ignores_blank_and_malformed_lines() {
        assert_eq!(
            parse_headers("X-Test: one\ninvalid\n\nContent-Type: text/plain"),
            vec![
                ("X-Test".into(), "one".into()),
                ("Content-Type".into(), "text/plain".into()),
            ]
        );
    }

    #[test]
    fn script_builtins_return_template_fields() {
        let password = Zeroizing::new("hello world".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { digest: md5(ctx.password), encoded: url_encode(ctx.password), flag: true, count: 2 }; }",
            "user",
            &password,
            "http://portal.example/",
            "",
            "",
            String::new(),
        )
        .unwrap();
        assert_eq!(values["digest"], "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert_eq!(values["encoded"], "hello+world");
        assert_eq!(values["flag"], "true");
        assert_eq!(values["count"], "2");
    }

    #[test]
    fn script_reports_invalid_base64() {
        let password = Zeroizing::new(String::new());
        let error = run_script_in_sandbox(
            "function transform() { return { decoded: base64_decode('%%%') }; }",
            "",
            &password,
            "",
            "",
            "",
            String::new(),
        )
        .unwrap_err();
        assert!(error.contains("Base64 解码失败"), "{error}");
    }

    /// ctx 暴露本机地址：eportal / Dr.COM 类门户的字段密钥由来源 IP 推导，
    /// 没有 ctx.local_ip 就无法在直连渠道复现（见 changelog）。
    #[test]
    fn script_ctx_exposes_local_address() {
        let password = Zeroizing::new("pw".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { ip: ctx.local_ip, mac: ctx.local_mac }; }",
            "user",
            &password,
            "http://portal.example/",
            "10.20.30.40",
            "00:1a:2b:3c:4d:5e",
            String::new(),
        )
        .unwrap();
        assert_eq!(values["ip"], "10.20.30.40");
        assert_eq!(values["mac"], "00:1a:2b:3c:4d:5e");
    }

    /// 取不到本机地址时 ctx 字段为空串（而非 undefined/报错），脚本据此可
    /// 判断并回退到从页面提取——脚本不应因为拿不到 IP 就崩掉。
    #[test]
    fn script_ctx_local_address_empty_when_unknown() {
        let password = Zeroizing::new("pw".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { ip: ctx.local_ip, hasIp: ctx.local_ip.length > 0 }; }",
            "user",
            &password,
            "",
            "",
            "",
            String::new(),
        )
        .unwrap();
        assert_eq!(values["ip"], "");
        assert_eq!(values["hasIp"], "false");
    }

    #[test]
    fn validate_url_requires_http_host() {
        assert!(HttpLoginRequest::validate_url("https://portal.example/login").is_ok());
        assert!(HttpLoginRequest::validate_url("file:///tmp/login").is_err());
        assert!(HttpLoginRequest::validate_url("https://").is_err());
    }

    /// 本机地址不参与脱敏：它常出现在「IP 不匹配」这类门户诊断文案里，
    /// 遮掉就等于把排查线索一并抹除（且它并非用户秘密）。
    #[test]
    fn collect_secrets_excludes_local_address_and_auth_url() {
        let mut vars = BTreeMap::new();
        vars.insert("username".to_string(), "alice".to_string());
        vars.insert("password".to_string(), "s3cret".to_string());
        vars.insert("auth_url".to_string(), "http://10.0.0.1/".to_string());
        vars.insert("local_ip".to_string(), "192.168.123.210".to_string());
        vars.insert("local_mac".to_string(), "00:1a:2b:3c:4d:5e".to_string());
        let secrets = collect_secrets(&vars);
        assert!(secrets.contains(&"alice".to_string()), "账号应脱敏");
        assert!(secrets.contains(&"s3cret".to_string()), "密码应脱敏");
        assert!(
            !secrets.contains(&"192.168.123.210".to_string()),
            "本机 IP 不应脱敏（否则 IP 不匹配类诊断无法排查）：{secrets:?}"
        );
        assert!(
            !secrets.contains(&"00:1a:2b:3c:4d:5e".to_string()),
            "本机 MAC 不应脱敏"
        );
        assert!(
            !secrets.contains(&"http://10.0.0.1/".to_string()),
            "auth_url 不应脱敏"
        );
    }

    /// 以指定 charset 编码响应体返回（GBK 等非 UTF-8 门户场景）
    async fn spawn_encoded_response(status: u16, body: &str, content_type: &str) -> String {
        let (bytes, _, _) = encoding_rs::GBK.encode(body);
        let bytes = bytes.into_owned();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let ctype = content_type.to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let mut response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .into_bytes();
            response.extend_from_slice(&bytes);
            stream.write_all(&response).await.unwrap();
        });
        format!("http://{addr}/login?u={{username}}&p={{password}}")
    }

    /// 显式声明 GBK 的中文门户：中文成功关键字必须能命中
    #[tokio::test]
    async fn gbk_body_with_charset_matches_chinese_success_pattern() {
        let url = spawn_encoded_response(200, "登录成功", "text/html; charset=gbk").await;
        let report = run_once(&request(url)).await;
        assert_eq!(
            report.outcome,
            Outcome::Success,
            "GBK 响应体被误解码: {}",
            report.message
        );
    }

    /// 未声明 charset 的 GBK 响应同样不能破坏中文关键字判定
    #[tokio::test]
    async fn gbk_body_without_charset_matches_chinese_success_pattern() {
        let url = spawn_encoded_response(200, "登录成功", "text/html").await;
        let report = run_once(&request(url)).await;
        assert_eq!(
            report.outcome,
            Outcome::Success,
            "GBK 响应体被误解码: {}",
            report.message
        );
    }

    /// 网络错误消息不得携带凭据：reqwest 的 Error::Display 会拼上完整 URL
    #[tokio::test]
    async fn network_error_message_does_not_leak_credentials() {
        // 指向未监听端口触发请求错误
        let report = run_once(&request(
            "http://127.0.0.1:1/login?u={username}&p={password}".to_string(),
        ))
        .await;
        assert_eq!(report.outcome, Outcome::NetworkError);
        assert!(
            !report.message.contains("abcdef"),
            "网络错误消息泄露密码: {}",
            report.message
        );
        assert!(
            !report.message.contains("abc"),
            "网络错误消息泄露账号: {}",
            report.message
        );
    }

    #[tokio::test]
    async fn failure_pattern_wins_over_success_pattern() {
        let url = spawn_response(200, "登录成功，但密码错误").await;
        let report = run_once(&request(url)).await;
        assert_eq!(report.outcome, Outcome::InvalidCredential);
    }

    #[tokio::test]
    async fn report_redacts_longer_secret_before_its_prefix() {
        let url = spawn_response(200, "登录成功 abc abcdef").await;
        let report = run_once(&request(url)).await;
        assert_eq!(report.outcome, Outcome::Success);
        assert!(!report.rendered_url.contains("abc"));
        assert!(!report.rendered_url.contains("def"));
        assert!(!report.response_snippet.contains("abc"));
        assert!(!report.response_snippet.contains("def"));
        assert!(report.rendered_url.contains("***"));
    }
}
