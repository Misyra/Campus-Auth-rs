//! 直连登录执行器：按方案的直连任务构造并发送登录请求。
//!
//! 与浏览器渠道（Python Worker + Playwright）完全独立：请求由 Rust 主进程发送，
//! 纯计算脚本在同一可执行文件的受限子进程中运行，不要求 Python 环境与浏览器就绪。流水线：
//!
//! 1. （配置了加密脚本时）抓取登录页原文，供脚本从页面取盐值等参数
//! 2. 用户加密脚本：内置 boa 引擎在无网络/文件沙箱中执行 `transform(ctx)`
//! 3. 模板替换：URL/请求头/请求体中的 `{username}` `{password}` 与脚本返回的
//!    任意字段按名替换（值原样替换不转义，特殊字符可用 `url_encode()`）
//! 4. 发送请求（不跟随系统代理；跨源跳转重建无凭据请求，上限 5 跳）
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
use crate::tasks::{
    HttpActionRequest, HttpExtractRule, HttpFlowStep, HttpFlowStepKind, HttpPreRequest,
    HttpRequestMethod, HttpStepOnError, HttpTaskConfig,
};

/// 响应体展示/判定的读取上限（字节）：门户响应通常极小，超限部分截断
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// 登录请求重定向跟随上限
const MAX_REDIRECTS: usize = 5;
/// 登录页抓取超时（best effort，失败不影响主流程）
const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// 前置请求（取 CSRF token 之类）超时
const PRE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 退出登录动作请求超时：与前置请求同级——都是辅助动作，门户不回也不该拖死登录
const LOGOUT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 退出登录请求后的额外等待上限（秒）：与模型层 `HttpActionRequest::MAX_WAIT_SECS`
/// 同口径的执行侧兜底（任务 JSON 可能绕过保存校验直接构造）
const LOGOUT_MAX_WAIT_SECS: f64 = 30.0;
/// 登录请求总超时
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 新式多步骤流程的一次尝试总时限。
const FLOW_TIMEOUT: Duration = Duration::from_secs(120);
/// 用户计算脚本的子进程总时限；超时后回收进程，避免孤儿计算线程累积。
#[cfg(not(test))]
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(2);
/// 子进程协议上限，覆盖脚本、页面文本及前面步骤的变量。
const MAX_TRANSFORM_IPC_BYTES: usize = 512 * 1024;
/// 消息中响应片段的最大长度
const SNIPPET_LEN: usize = 240;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_HEADERS_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;
const MAX_PATTERN_BYTES: usize = 8 * 1024;
const MAX_SCRIPT_BYTES: usize = 128 * 1024;
/// 单个响应变量和本次登录所有变量的内存预算，避免异常门户放大后续脚本输入。
const MAX_EXTRACT_VALUE_BYTES: usize = 8 * 1024;
const MAX_FLOW_VARIABLE_BYTES: usize = 128 * 1024;
/// 响应头回显上限（字节）：门户响应头通常只有几百字节，超限截断防异常门户撑爆面板
const MAX_RESPONSE_HEADERS_BYTES: usize = 8 * 1024;
/// 未配置 User-Agent 时使用的兜底值。
///
/// reqwest 不设置该头便完全不发 `User-Agent`，部分门户/WAF 会因此返回 403
/// 或另一套页面（而浏览器渠道总有 UA），表现为「抓包看不出问题、直连就是失败」。
/// 用常见浏览器标识兜底，用户可在请求头里显式覆盖。
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 一次直连登录尝试的完整请求参数（由方案绑定的直连任务构造）
#[derive(Clone)]
pub(crate) struct HttpLoginRequest {
    /// 新式有序流程；非空时取代旧固定请求链。
    pub steps: Vec<HttpFlowStep>,
    /// 结果判断所引用的请求步骤 ID；空串表示最后一个请求步骤。
    pub result_step_id: String,
    /// 请求方法
    pub method: HttpRequestMethod,
    /// 请求 URL（完整地址）
    pub url: String,
    /// 请求头模板（每行 `Key: Value`）
    pub headers: String,
    /// 请求体模板（POST 使用）
    pub body: String,
    /// 成功判定关键字（空 = HTTP 2xx 即成功）
    pub success_pattern: String,
    /// 失败判定关键字（命中后由 failure_action 决定停止或重试）
    pub failure_pattern: String,
    /// 失败关键字命中后的动作（凭据错误立即停止 / 临时故障重试）。
    pub failure_action: crate::tasks::HttpFailureAction,
    /// 成败判定方式：`Response` 响应关键字（默认）/ `Network` 网络检测
    ///
    /// `Network` 模式下响应体与状态码都不参与成功判定——请求发出且未命中
    /// `failure_pattern` 即为候选成功，最终成败由会话层登录后网络验证判定
    /// （直连不构造 worker_config，该验证对直连恒生效）。`failure_pattern`
    /// 仍用于快速失败：门户明确报错时没必要等探测结果。
    pub success_check: crate::tasks::HttpSuccessCheck,
    /// 加密脚本（空 = 不变换）
    pub crypto_script: String,
    /// 前置请求（`None` = 不需要）：先取回一个值（如 CSRF token）再渲染登录请求。
    /// 两次请求共用一个 `Client`，故 token 绑定 TCP 连接的门户也能成功。
    pub pre_request: Option<HttpPreRequest>,
    /// 退出登录请求（`None` = 不需要）：登录前先发一次下线动作，治「IP 已在线拒绝
    /// 重复登录」类门户。只求触达不求结果——下线请求失败不判终态，登录照常进行。
    pub logout_request: Option<HttpActionRequest>,
    /// 登录用户名
    pub username: String,
    /// 登录密码（Zeroizing 保护）
    pub password: Zeroizing<String>,
    /// 方案的运营商字段（原样透传：预设为「移动/联通/电信」，自定义时为用户输入的
    /// 关键字，未选择时为空串）。供脚本 `ctx.isp` 与模板 `{isp}` 占位符使用——
    /// 门户侧的运营商表示法（如 Dr.COM 的 `@cmcc` 后缀）由任务脚本自行映射。
    pub isp: String,
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
    /// 由直连任务构造请求参数（方案只提供凭据与回退认证地址）。
    ///
    /// 请求本体（方法 / 地址 / 请求头 / 请求体 / 判定关键字 / 凭据变换脚本 / 证书策略）
    /// 全部来自任务 `tasks/http/<id>.json`：同一门户的多个账号因此共用一份配置，
    /// 改门户地址只需改任务，不必逐个方案改。
    ///
    /// `auth_url` 由调用方解析好传入——任务的认证地址非空时优先，留空才回退方案的
    /// 同名字段（该字段两渠道共用，老配置不填也照旧可用），回退链在调用侧实现
    /// （见 `LoginOrchestrator::resolve_http_task` 与 `/api/http-tasks/test`），
    /// 以免这里再持有一份方案快照。
    ///
    /// `global_ignore_https_errors` 为全局 `browser.ignore_https_errors`：任务未显式
    /// 设置 `ignore_https_errors` 时沿用它，保证与浏览器渠道同口径。
    pub fn from_task(
        task: &HttpTaskConfig,
        username: &str,
        password: &str,
        isp: &str,
        auth_url: &str,
        fetch_page: bool,
        global_ignore_https_errors: bool,
    ) -> Result<Self, String> {
        task.validate_flow()?;
        if task.steps.is_empty() && task.url.trim().is_empty() {
            return Err("直连任务缺少请求地址，请在「任务 · 直连任务」里填写".into());
        }
        if task.steps.is_empty() {
            Self::validate_url(&task.url)?;
        }
        let request = Self {
            steps: task.steps.clone(),
            result_step_id: task.result_step_id.clone(),
            method: task.method,
            url: task.url.trim().to_string(),
            headers: task.headers.clone(),
            body: task.body.clone(),
            success_pattern: task.success_pattern.clone(),
            failure_pattern: task.failure_pattern.clone(),
            failure_action: task.failure_action,
            success_check: task.success_check,
            crypto_script: task.crypto_script.clone(),
            pre_request: task.pre_request.clone(),
            logout_request: task.logout_request.clone(),
            username: username.trim().to_string(),
            password: Zeroizing::new(password.to_string()),
            isp: isp.trim().to_string(),
            auth_url: auth_url.trim().to_string(),
            // 本机地址需异步查询网卡，由调用方（持有 MonitorService）按需填充，
            // 见 [`HttpLoginRequest::with_local_address`]
            local_ip: String::new(),
            local_mac: String::new(),
            fetch_page,
            ignore_https_errors: task
                .ignore_https_errors
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
        if self.steps.is_empty() {
            Self::validate_url(&self.url)?;
        } else {
            let task = HttpTaskConfig {
                schema_version: 2,
                steps: self.steps.clone(),
                result_step_id: self.result_step_id.clone(),
                ..HttpTaskConfig::default()
            };
            task.validate_flow()?;
        }
        Self::validate_templates(
            &self.url,
            &self.headers,
            &self.body,
            &self.success_pattern,
            &self.failure_pattern,
            &self.crypto_script,
        )?;
        if let Some(pre) = &self.pre_request {
            Self::validate_pre_request(pre)?;
        }
        if let Some(logout) = &self.logout_request {
            Self::validate_logout_request(logout)?;
        }
        Ok(())
    }

    /// 退出登录请求校验：形状委托给 [`HttpActionRequest::validate`]（保存与执行
    /// 同源），此处只补执行侧才关心的体积边界。
    pub fn validate_logout_request(logout: &HttpActionRequest) -> Result<(), String> {
        logout.validate()?;
        let checks = [
            ("退出登录请求 URL", logout.url.len(), MAX_URL_BYTES),
            ("退出登录请求头", logout.headers.len(), MAX_HEADERS_BYTES),
            ("退出登录请求体", logout.body.len(), MAX_REQUEST_BODY_BYTES),
        ];
        for (label, actual, limit) in checks {
            if actual > limit {
                return Err(format!("{label}过长（最多 {limit} 字节）"));
            }
        }
        Ok(())
    }

    /// 前置请求校验：结构形状与各模板体积上限。
    ///
    /// 形状（地址必填且 http(s)、取值方式可解析）委托给
    /// [`HttpPreRequest::validate`]——保存路径（只拿到 JSON 值）与执行路径必须同源，
    /// 此处只补执行侧才关心的体积边界。
    pub fn validate_pre_request(pre: &HttpPreRequest) -> Result<(), String> {
        pre.validate()?;
        let checks = [
            ("前置请求 URL", pre.url.len(), MAX_URL_BYTES),
            ("前置请求头", pre.headers.len(), MAX_HEADERS_BYTES),
            ("前置请求体", pre.body.len(), MAX_REQUEST_BODY_BYTES),
            ("前置请求取值方式", pre.extract.len(), MAX_PATTERN_BYTES),
            ("前置请求占位符名", pre.name.len(), MAX_PATTERN_BYTES),
        ];
        for (label, actual, limit) in checks {
            if actual > limit {
                return Err(format!("{label}过长（最多 {limit} 字节）"));
            }
        }
        Ok(())
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
        if self.steps.is_empty() {
            !self.crypto_script.trim().is_empty()
        } else {
            self.steps
                .iter()
                .any(|step| step.kind == HttpFlowStepKind::Transform)
        }
    }

    /// 是否需要本机地址（决定是否值得做一次网卡探测）。
    ///
    /// 除了脚本要读 `ctx.local_ip` / `ctx.local_mac`，**模板里直接写 `{local_ip}` 也算**：
    /// 锐捷 ePortal 这类门户把本机 IP 当必填参数提交，少查一次就是静默发出空 IP
    /// （请求照发、门户照拒，用户看不出是哪个环节空了）。此前只看"有没有脚本"，
    /// 于是"模板用了 {local_ip} 但没配脚本"的任务必然失败且无从判断。
    pub fn needs_local_address(&self) -> bool {
        fn mentions(text: &str) -> bool {
            text.contains("{local_ip}") || text.contains("{local_mac}")
        }
        self.uses_crypto_script()
            || self
                .steps
                .iter()
                .any(|step| mentions(&step.url) || mentions(&step.headers) || mentions(&step.body))
            || mentions(&self.url)
            || mentions(&self.headers)
            || mentions(&self.body)
            || self.pre_request.as_ref().is_some_and(|pre| {
                mentions(&pre.url) || mentions(&pre.headers) || mentions(&pre.body)
            })
            || self.logout_request.as_ref().is_some_and(|logout| {
                mentions(&logout.url) || mentions(&logout.headers) || mentions(&logout.body)
            })
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

/// 多步骤流程中单个步骤的脱敏诊断。
#[derive(Debug, serde::Serialize)]
pub(crate) struct HttpStepReport {
    /// 稳定步骤 ID。
    pub id: String,
    /// 用户设置的步骤名称。
    pub name: String,
    /// 步骤类型。
    pub kind: HttpFlowStepKind,
    /// `success`、`ignored` 或 `failed`。
    pub outcome: &'static str,
    /// 该步骤的简短说明。
    pub message: String,
    /// 本步骤生成的变量名；仅回传名称，不回传敏感值。
    pub produced_vars: Vec<String>,
    /// 脱敏后的请求地址。
    pub rendered_url: String,
    /// 脱敏后的请求头。
    pub rendered_headers: String,
    /// 脱敏后的请求体。
    pub rendered_body: String,
    /// HTTP 状态码。
    pub status: Option<u16>,
    /// 脱敏后的响应头。
    pub response_headers: String,
    /// 脱敏后的响应片段。
    pub response_snippet: String,
    /// 耗时毫秒数。
    pub duration_ms: u64,
}

impl HttpStepReport {
    /// 在步骤变量全部收集后统一脱敏，覆盖后来才提取出的令牌。
    fn redact(&mut self, secrets: &[String]) {
        self.message = redact_text(&self.message, secrets);
        for name in &mut self.produced_vars {
            *name = redact_text(name, secrets);
        }
        self.rendered_url = redact_url(&self.rendered_url, secrets);
        self.rendered_headers = redact_text(&self.rendered_headers, secrets);
        self.rendered_body = redact_text(&self.rendered_body, secrets);
        self.response_headers = redact_text(&self.response_headers, secrets);
        self.response_snippet = redact_text(&self.response_snippet, secrets);
    }
}

/// 一次直连尝试的执行报告（登录会话与测试端点共用）
#[derive(Debug)]
pub(crate) struct HttpAttemptReport {
    /// 多步骤流程的逐步诊断；旧任务为空。
    pub step_reports: Vec<HttpStepReport>,
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

/// 两种任务格式共用的内置占位符，脚本产出和步骤提取值随后覆盖到同一张表。
fn initial_vars(req: &HttpLoginRequest) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("username".to_string(), req.username.clone()),
        ("password".to_string(), req.password.to_string()),
        // 运营商值原样透传，门户特有的后缀由任务脚本映射。
        ("isp".to_string(), req.isp.clone()),
        ("auth_url".to_string(), req.auth_url.clone()),
        // 本机地址取不到时保留空串，让模板和脚本自行处理。
        ("local_ip".to_string(), req.local_ip.clone()),
        ("local_mac".to_string(), req.local_mac.clone()),
    ])
}

/// 最终响应的判定；消息与诊断格式仍由各自的任务路径生成。
enum HttpResponseVerdict {
    Failure(Outcome),
    Success,
    MissingSuccess,
}

fn classify_response(
    req: &HttpLoginRequest,
    status: reqwest::StatusCode,
    body: &str,
) -> HttpResponseVerdict {
    let failure_pattern = req.failure_pattern.trim();
    if !failure_pattern.is_empty() && body.contains(failure_pattern) {
        let outcome = match req.failure_action {
            crate::tasks::HttpFailureAction::Credential => Outcome::InvalidCredential,
            crate::tasks::HttpFailureAction::Retry => Outcome::AssertionFailed,
            crate::tasks::HttpFailureAction::Manual => Outcome::ManualRequired,
        };
        return HttpResponseVerdict::Failure(outcome);
    }

    // 网络模式只把响应当作候选成功；最终由会话层网络验证决定。
    if req.success_check == crate::tasks::HttpSuccessCheck::Network {
        return HttpResponseVerdict::Success;
    }
    let success_pattern = req.success_pattern.trim();
    if (success_pattern.is_empty() && status.is_success())
        || (!success_pattern.is_empty() && body.contains(success_pattern))
    {
        HttpResponseVerdict::Success
    } else {
        HttpResponseVerdict::MissingSuccess
    }
}

/// 执行一次直连登录尝试（不发网络验证，验证由会话状态机负责）
pub(crate) async fn run_once(req: &HttpLoginRequest) -> HttpAttemptReport {
    if !req.steps.is_empty() {
        return run_flow_once(req).await;
    }
    let start = Instant::now();

    // 0. 本次尝试共用的 HTTP 客户端（退出登录请求 / 登录页抓取 / 前置请求 / 登录请求
    //    四处都用它）：连接池挂在 Client 上，CSRF 令牌绑定 TCP 连接的门户要求取 token 与
    //    发登录落在同一条 keep-alive 连接，故整次尝试只能建一个。见 [`build_client`]。
    let client = match build_client(req.ignore_https_errors) {
        Ok(client) => client,
        Err(e) => {
            return abort_report(
                Outcome::UnknownError,
                format!("HTTP 客户端构建失败: {e}"),
                None,
                None,
                None,
                start.elapsed().as_millis() as u64,
            );
        }
    };

    // 占位符表：先放内置项（下线请求只能用这些，见下方第 1 步），脚本产出的字段在第 2 步
    // 并入同一张表。
    let mut vars = initial_vars(req);
    let mut secrets = collect_secrets(&vars);

    // 1. 退出登录动作（可选）：治「IP 已在线，拒绝重复登录」类门户——先踢掉旧会话再登录。
    //    必须在**登录页抓取与凭据变换脚本之前**：这类门户连取令牌的接口都可能被旧会话
    //    挡住（返回 already-online 类错误），`fetch_login_page` 抓到的会是"已在线"页而不是
    //    登录表单，脚本据此产出的字段全是错的——正是本功能要治的那类门户。先清场再取令牌。
    //
    //    代价：此处只能用**内置占位符**（username / password / isp / auth_url /
    //    local_ip / local_mac），拿不到脚本产出的字段（脚本还没跑）。下线地址通常只需
    //    要账号，而"抓到错的登录页"是必然坏、脚本占位符只是可能用到，故取前者。
    //
    //    与前置请求的本质差异在**结果语义**：取值失败 = 必然登不上（终态），下线没生效
    //    = 登录仍可能成功（不判死）。因此这里只记日志、不产生报告分支。
    //
    // 步骤编号（与 `docs/guides/http-login-guide.md` 的 3.x 节一致）：
    // 1 下线 → 2 底层变量表 + 凭据变换脚本 → 3 前置请求 → 4 模板渲染 → 5 发送 → 6 成败判定。
    // 变量表（`vars`）在这里先建出来，脚本产出合并进它是在第 2 步（紧跟本段之后）。
    if let Some(logout) = &req.logout_request {
        let logout_url = substitute(&logout.url, &vars);
        let logout_headers = substitute(&logout.headers, &vars);
        let logout_body = substitute(&logout.body, &vars);
        match send_http(
            &client,
            logout.method,
            &logout_url,
            &logout_headers,
            &logout_body,
            LOGOUT_REQUEST_TIMEOUT,
        )
        .await
        {
            Ok((status, _body, _headers)) => {
                tracing::debug!("退出登录请求已发送（HTTP {status}）");
            }
            Err(e) => {
                // best effort：下线失败只说明旧会话可能还在，登录本身仍值得一试。
                // 错误消息可能拼 URL（GET 下线地址或含凭据），日志走脱敏。
                tracing::warn!(
                    "退出登录请求失败（忽略，继续登录）: {}",
                    redact_text(&e, &secrets)
                );
            }
        }
        // 下线通常是异步生效的：等待窗口钳到 30s，防止误配置拖爆登录节奏
        let wait = logout.wait_secs.clamp(0.0, LOGOUT_MAX_WAIT_SECS);
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
    }

    // 2. 用户脚本值变换：跑凭据脚本，产出可被占位符引用的字段表（并入上面的 `vars`）
    let mut script_error = None;
    if req.uses_crypto_script() {
        // 登录页原文 best effort 抓取：失败置空串，脚本须容忍缺失
        let page = if req.fetch_page {
            fetch_login_page(&client, &req.auth_url).await
        } else {
            String::new()
        };
        match execute_crypto_script(
            &req.crypto_script,
            &req.username,
            &req.password,
            &req.isp,
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
                    add_secret(&mut secrets, &v);
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
        normalize_secrets(&mut secrets);
        return abort_report(
            Outcome::UnknownError,
            redact_text(&format!("加密脚本执行失败: {e}"), &secrets),
            None,
            None,
            Some(redact_text(e, &secrets)),
            start.elapsed().as_millis() as u64,
        );
    }

    // 3. 前置请求（可选）：先取回一个值（如 CSRF token）注册成占位符，供登录请求的
    //     URL / 请求头 / 请求体引用。与登录请求同一个 client，两条请求因此落在同一条
    //     连接上；取不到值就没必要再发登录请求（必然被门户拒），直接终态并把这次
    //     前置请求的请求与响应带进报告，便于在测试面板里看清是哪一步不对。
    if let Some(pre) = &req.pre_request {
        let pre_url = substitute(&pre.url, &vars);
        let pre_headers = substitute(&pre.headers, &vars);
        let pre_body = substitute(&pre.body, &vars);
        normalize_secrets(&mut secrets);
        let rendered = (
            redact_url(&pre_url, &secrets),
            redact_text(&pre_headers, &secrets),
            redact_text(&pre_body, &secrets),
        );

        match send_http(
            &client,
            pre.method,
            &pre_url,
            &pre_headers,
            &pre_body,
            PRE_REQUEST_TIMEOUT,
        )
        .await
        {
            Ok((status, body, response_headers)) => {
                let body_snippet = truncate_snippet(&body);
                let extracted = match pre.extract_path() {
                    Ok(path) => extract_pre_value(path, &body),
                    Err(e) => Err(e),
                };
                match extracted {
                    Ok(value) => {
                        let placeholder = pre.placeholder_name();
                        tracing::debug!(
                            "前置请求（HTTP {status}）取到占位符 `{placeholder}`（{} 字节）",
                            value.len()
                        );
                        // 取到的值并入本次尝试的脱敏字典，后续报告不会回显令牌。
                        add_secret(&mut secrets, &value);
                        vars.insert(placeholder, value);
                    }
                    Err(e) => {
                        return abort_report(
                            Outcome::UnknownError,
                            redact_text(&format!("前置请求未取到占位符: {e}"), &secrets),
                            Some((&rendered.0, &rendered.1, &rendered.2)),
                            Some((
                                status.as_u16(),
                                &redact_text(&response_headers, &secrets),
                                &redact_text(&body_snippet, &secrets),
                            )),
                            None,
                            start.elapsed().as_millis() as u64,
                        );
                    }
                }
            }
            Err(e) => {
                // reqwest 的错误消息会拼上完整 URL，前置请求地址同样可能带凭据参数
                return abort_report(
                    Outcome::NetworkError,
                    redact_text(&format!("前置请求失败: {e}"), &secrets),
                    Some((&rendered.0, &rendered.1, &rendered.2)),
                    None,
                    None,
                    start.elapsed().as_millis() as u64,
                );
            }
        }
    }

    // 4. 模板渲染
    let rendered_url = substitute(&req.url, &vars);
    let rendered_headers = substitute(&req.headers, &vars);
    let rendered_body = substitute(&req.body, &vars);

    // 5. 发送请求
    let send = send_http(
        &client,
        req.method,
        &rendered_url,
        &rendered_headers,
        &rendered_body,
        REQUEST_TIMEOUT,
    )
    .await;
    let (status, body, response_headers) = match send {
        Ok(triple) => triple,
        Err(e) => {
            normalize_secrets(&mut secrets);
            // reqwest 的 Error::Display 会把完整 URL 拼进消息（"for url (...)"），
            // GET 渠道下 URL 含明文凭据，必须先脱敏再进 message——它会流入日志、
            // 登录历史与前端；rendered_url 的自有脱敏无法覆盖这条错误路径。
            return abort_report(
                Outcome::NetworkError,
                redact_text(&format!("直连请求失败: {e}"), &secrets),
                Some((
                    &redact_url(&rendered_url, &secrets),
                    &redact_text(&rendered_headers, &secrets),
                    &redact_text(&rendered_body, &secrets),
                )),
                None,
                script_error,
                start.elapsed().as_millis() as u64,
            );
        }
    };

    // 6. 成败判定
    normalize_secrets(&mut secrets);
    let snippet = truncate_snippet(&body);
    let (outcome, message) = match classify_response(req, status, &body) {
        HttpResponseVerdict::Failure(outcome) => (
            outcome,
            format!(
                "门户返回失败标识（HTTP {status}）: {}",
                redact_text(&snippet, &secrets)
            ),
        ),
        HttpResponseVerdict::Success => {
            let message = if req.success_check == crate::tasks::HttpSuccessCheck::Network {
                format!("已发送登录请求（HTTP {status}），成功与否由登录后网络检测判定")
            } else {
                format!("直连请求成功（HTTP {status}）")
            };
            (Outcome::Success, message)
        }
        HttpResponseVerdict::MissingSuccess => (
            Outcome::AssertionFailed,
            format!(
                "未命中成功标识（HTTP {status}）: {}",
                redact_text(&snippet, &secrets)
            ),
        ),
    };

    HttpAttemptReport {
        step_reports: Vec::new(),
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

/// 执行可编辑的有序 HTTP 登录流程；旧任务仍走上面的固定链。
async fn run_flow_once(req: &HttpLoginRequest) -> HttpAttemptReport {
    let start = Instant::now();
    let client = match build_client(req.ignore_https_errors) {
        Ok(client) => client,
        Err(error) => {
            return abort_report(
                Outcome::UnknownError,
                format!("HTTP 客户端构建失败: {error}"),
                None,
                None,
                None,
                start.elapsed().as_millis() as u64,
            );
        }
    };
    let mut vars = initial_vars(req);
    // 步骤可覆盖内置变量；后面统一脱敏时仍须记住前面请求用过的旧值。
    let mut secrets = collect_secrets(&vars);
    let result_id = if req.result_step_id.is_empty() {
        req.steps
            .iter()
            .rev()
            .find(|step| step.kind == HttpFlowStepKind::Request)
            .map_or("", |step| step.id.as_str())
    } else {
        req.result_step_id.as_str()
    };
    let mut reports = Vec::with_capacity(req.steps.len());
    let mut page: Option<String> = None;
    // 状态码、响应体/头、实际发出的请求：结果判断可引用中间请求，不能只保存最后一条。
    let mut result_response: Option<(reqwest::StatusCode, String, String, String, String, String)> =
        None;

    for step in &req.steps {
        if start.elapsed() >= FLOW_TIMEOUT {
            return flow_abort(
                Outcome::NetworkError,
                "HTTP 登录流程超时".into(),
                None,
                None,
                None,
                start,
                reports,
                &secrets,
            );
        }
        let step_start = Instant::now();
        if step.kind == HttpFlowStepKind::Transform {
            let page_text = if req.fetch_page {
                if page.is_none() {
                    page = Some(fetch_login_page(&client, &req.auth_url).await);
                }
                page.clone().unwrap_or_default()
            } else {
                String::new()
            };
            match execute_crypto_script_with_vars(
                &step.script,
                &req.username,
                &req.password,
                &req.isp,
                &req.auth_url,
                (&req.local_ip, &req.local_mac),
                page_text,
                vars.clone(),
            )
            .await
            {
                Ok(values) => {
                    let count = values.len();
                    let produced_vars = values.keys().cloned().collect();
                    if values
                        .values()
                        .any(|value| value.len() > MAX_EXTRACT_VALUE_BYTES)
                        || vars.values().map(String::len).sum::<usize>()
                            + values.values().map(String::len).sum::<usize>()
                            > MAX_FLOW_VARIABLE_BYTES
                    {
                        reports.push(flow_step_report(
                            step,
                            "failed",
                            "计算结果中的变量超过大小限制".into(),
                            ("", "", ""),
                            None,
                            step_start,
                        ));
                        return flow_abort(
                            Outcome::UnknownError,
                            "HTTP 流程变量超过大小限制".into(),
                            None,
                            None,
                            None,
                            start,
                            reports,
                            &secrets,
                        );
                    }
                    for value in values.values() {
                        add_secret(&mut secrets, value);
                    }
                    vars.extend(values);
                    reports.push(flow_step_report(
                        step,
                        "success",
                        format!("计算完成，生成 {count} 个字段"),
                        ("", "", ""),
                        None,
                        step_start,
                    ));
                    if let Some(report) = reports.last_mut() {
                        report.produced_vars = produced_vars;
                    }
                }
                Err(error) => {
                    reports.push(flow_step_report(
                        step,
                        "failed",
                        format!("计算失败: {error}"),
                        ("", "", ""),
                        None,
                        step_start,
                    ));
                    return flow_abort(
                        Outcome::UnknownError,
                        format!("步骤「{}」计算失败: {error}", step.name),
                        None,
                        None,
                        Some(error),
                        start,
                        reports,
                        &secrets,
                    );
                }
            }
            continue;
        }

        let url = substitute(&step.url, &vars);
        let headers = substitute(&step.headers, &vars);
        let body = substitute(&step.body, &vars);
        let rendered = (url.as_str(), headers.as_str(), body.as_str());
        let remaining = FLOW_TIMEOUT.saturating_sub(start.elapsed());
        let timeout = REQUEST_TIMEOUT.min(remaining);
        let response = send_http_detailed(
            &client,
            step.method,
            &url,
            &headers,
            &body,
            timeout,
            step.stop_on_redirect,
        )
        .await;
        let HttpResponse {
            status,
            body: response_body,
            headers: response_header_map,
            headers_text: response_headers,
            final_url,
        } = match response {
            Ok(response) => response,
            Err(error) => {
                let ignored = step.on_error == HttpStepOnError::Continue;
                reports.push(flow_step_report(
                    step,
                    if ignored { "ignored" } else { "failed" },
                    format!("请求失败: {error}"),
                    rendered,
                    None,
                    step_start,
                ));
                if ignored {
                    if !wait_flow_step(step.wait_secs, start).await {
                        return flow_abort(
                            Outcome::NetworkError,
                            "HTTP 登录流程超时".into(),
                            None,
                            None,
                            None,
                            start,
                            reports,
                            &secrets,
                        );
                    }
                    continue;
                }
                return flow_abort(
                    Outcome::NetworkError,
                    format!("步骤「{}」请求失败: {error}", step.name),
                    Some(rendered),
                    None,
                    None,
                    start,
                    reports,
                    &secrets,
                );
            }
        };
        let response_tuple = (
            status.as_u16(),
            response_headers.as_str(),
            response_body.as_str(),
        );
        let is_result = step.id == result_id;
        let mut issue = None;
        let mut produced_vars = Vec::new();
        if !step.extracts.is_empty() {
            // 一步中的多条取值要整体提交；忽略后半段失败时不能留下已覆盖的半份变量。
            let mut pending_vars = Vec::<(String, String)>::with_capacity(step.extracts.len());
            let mut variable_bytes = vars.values().map(String::len).sum::<usize>();
            for rule in &step.extracts {
                match extract_flow_value(rule, &response_body, &response_header_map, &final_url) {
                    Ok(value) => {
                        let previous_len = pending_vars
                            .iter()
                            .rev()
                            .find(|(name, _)| name == &rule.name)
                            .map(|(_, value)| value.len())
                            .or_else(|| vars.get(&rule.name).map(String::len))
                            .unwrap_or(0);
                        if value.len() > MAX_EXTRACT_VALUE_BYTES
                            || variable_bytes - previous_len + value.len() > MAX_FLOW_VARIABLE_BYTES
                        {
                            issue = Some((
                                format!("提取 `{}` 失败: 变量超过大小限制", rule.name),
                                Outcome::UnknownError,
                            ));
                            break;
                        }
                        variable_bytes = variable_bytes - previous_len + value.len();
                        // 即便整步最终失败，已读到的值仍可能出现在该步响应与错误报告里。
                        add_secret(&mut secrets, &value);
                        pending_vars.push((rule.name.clone(), value));
                    }
                    Err(error) => {
                        issue = Some((
                            format!("提取 `{}` 失败: {error}", rule.name),
                            Outcome::UnknownError,
                        ));
                        break;
                    }
                }
            }
            if issue.is_none() {
                for (name, value) in pending_vars {
                    produced_vars.push(name.clone());
                    vars.insert(name, value);
                }
            }
        } else if !step.extract.trim().is_empty() {
            let pre = HttpPreRequest {
                extract: step.extract.clone(),
                name: step.extract_as.clone(),
                ..HttpPreRequest::default()
            };
            match pre
                .extract_path()
                .and_then(|path| extract_pre_value(path, &response_body))
            {
                Ok(value) => {
                    if value.len() > MAX_EXTRACT_VALUE_BYTES
                        || vars.values().map(String::len).sum::<usize>() + value.len()
                            > MAX_FLOW_VARIABLE_BYTES
                    {
                        issue = Some((
                            "提取变量失败: 变量超过大小限制".into(),
                            Outcome::UnknownError,
                        ));
                    } else {
                        add_secret(&mut secrets, &value);
                        let name = pre.placeholder_name();
                        vars.insert(name.clone(), value);
                        produced_vars.push(name);
                    }
                }
                Err(error) => {
                    // 旧式前置请求取不到字段会直接终止；打开旧任务后编辑步骤不能
                    // 把同一错误悄悄改成重复请求。取到字段时也沿用旧式“响应可用”口径。
                    issue = Some((format!("提取变量失败: {error}"), Outcome::UnknownError));
                }
            }
        }
        if issue.is_none()
            && !status.is_success()
            && !is_result
            && step.extract.trim().is_empty()
            && step.extracts.is_empty()
        {
            issue = Some((format!("HTTP {status}"), Outcome::AssertionFailed));
        }
        let ignored = issue.is_some() && step.on_error == HttpStepOnError::Continue;
        let step_message = issue
            .as_ref()
            .map_or_else(|| format!("HTTP {status}"), |(error, _)| error.clone());
        reports.push(flow_step_report(
            step,
            if ignored {
                "ignored"
            } else if issue.is_some() {
                "failed"
            } else {
                "success"
            },
            step_message,
            rendered,
            Some(response_tuple),
            step_start,
        ));
        if let Some(report) = reports.last_mut() {
            report.produced_vars = produced_vars;
        }
        if let Some((error, outcome)) = issue {
            if !ignored {
                return flow_abort(
                    outcome,
                    format!("步骤「{}」失败: {error}", step.name),
                    Some(rendered),
                    Some(response_tuple),
                    None,
                    start,
                    reports,
                    &secrets,
                );
            }
        }
        if is_result {
            result_response = Some((status, response_body, response_headers, url, headers, body));
        }
        if !wait_flow_step(step.wait_secs, start).await {
            return flow_abort(
                Outcome::NetworkError,
                "HTTP 登录流程超时".into(),
                None,
                None,
                None,
                start,
                reports,
                &secrets,
            );
        }
    }

    let Some((status, body, response_headers, url, headers, request_body)) = result_response else {
        return flow_abort(
            Outcome::UnknownError,
            "结果判断引用的请求没有得到响应".into(),
            None,
            None,
            None,
            start,
            reports,
            &secrets,
        );
    };
    let (outcome, message) = match classify_response(req, status, &body) {
        HttpResponseVerdict::Failure(outcome) => (
            outcome,
            format!("步骤「{result_id}」命中失败标识（HTTP {status}）"),
        ),
        HttpResponseVerdict::Success => {
            let message = if req.success_check == crate::tasks::HttpSuccessCheck::Network {
                format!("已完成 HTTP 登录流程（HTTP {status}），由登录后网络检测判断结果")
            } else {
                format!("HTTP 登录流程成功（HTTP {status}）")
            };
            (Outcome::Success, message)
        }
        HttpResponseVerdict::MissingSuccess => (
            Outcome::AssertionFailed,
            format!("步骤「{result_id}」未命中成功标识（HTTP {status}）"),
        ),
    };
    normalize_secrets(&mut secrets);
    for report in &mut reports {
        report.redact(&secrets);
    }
    HttpAttemptReport {
        step_reports: reports,
        outcome,
        message: redact_text(&message, &secrets),
        rendered_url: redact_url(&url, &secrets),
        rendered_headers: redact_text(&headers, &secrets),
        rendered_body: redact_text(&request_body, &secrets),
        status: Some(status.as_u16()),
        response_headers: redact_text(&response_headers, &secrets),
        response_snippet: redact_text(&truncate_snippet(&body), &secrets),
        script_error: None,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// 请求失败被忽略时也要执行等待，保持旧式下线动作的时序。
async fn wait_flow_step(wait_secs: f64, start: Instant) -> bool {
    if wait_secs <= 0.0 {
        return true;
    }
    let wait = Duration::from_secs_f64(wait_secs);
    if wait > FLOW_TIMEOUT.saturating_sub(start.elapsed()) {
        return false;
    }
    tokio::time::sleep(wait).await;
    true
}

/// 生成流程步骤诊断；变量全部收集后才统一脱敏。
fn flow_step_report(
    step: &HttpFlowStep,
    outcome: &'static str,
    message: String,
    rendered: (&str, &str, &str),
    response: Option<(u16, &str, &str)>,
    start: Instant,
) -> HttpStepReport {
    let (status, response_headers, response_body) = response.unwrap_or((0, "", ""));
    HttpStepReport {
        id: step.id.clone(),
        name: step.name.clone(),
        kind: step.kind,
        outcome,
        message,
        produced_vars: Vec::new(),
        rendered_url: rendered.0.to_string(),
        rendered_headers: rendered.1.to_string(),
        rendered_body: rendered.2.to_string(),
        status: (status != 0).then_some(status),
        response_headers: response_headers.to_string(),
        response_snippet: truncate_snippet(response_body),
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// 统一构造流程中止报告，并在返回前脱敏每一个已执行步骤。
#[allow(clippy::too_many_arguments)]
fn flow_abort(
    outcome: Outcome,
    message: String,
    rendered: Option<(&str, &str, &str)>,
    response: Option<(u16, &str, &str)>,
    script_error: Option<String>,
    start: Instant,
    mut reports: Vec<HttpStepReport>,
    secrets: &[String],
) -> HttpAttemptReport {
    let mut secrets = secrets.to_vec();
    normalize_secrets(&mut secrets);
    for report in &mut reports {
        report.redact(&secrets);
    }
    let mut result = abort_report(
        outcome,
        redact_text(&message, &secrets),
        rendered,
        response,
        script_error.map(|error| redact_text(&error, &secrets)),
        start.elapsed().as_millis() as u64,
    );
    result.rendered_url = redact_url(&result.rendered_url, &secrets);
    result.rendered_headers = redact_text(&result.rendered_headers, &secrets);
    result.rendered_body = redact_text(&result.rendered_body, &secrets);
    result.response_headers = redact_text(&result.response_headers, &secrets);
    result.response_snippet = redact_text(&truncate_snippet(&result.response_snippet), &secrets);
    result.step_reports = reports;
    result
}

/// 构建直连请求客户端（登录请求与登录页抓取共用同一策略）。
///
/// 统一收口三件事，避免两处各自构造时策略漂移：
/// - 证书策略：按 `ignore_https_errors`（自签门户必需，与浏览器渠道同口径）
/// - 代理：显式 `no_proxy`（校园网网关是本机直连可达的内网地址，走代理必失败）
/// - User-Agent：未显式配置时补浏览器 UA（reqwest 默认完全不发该头）
///
/// **整次直连尝试只建一个**，由调用方传给登录页抓取 / 前置请求 / 登录请求三处：
/// reqwest 的连接池挂在 `Client` 上，而部分门户把 CSRF 令牌绑在 TCP 连接上（取 token
/// 与发登录必须同一条 keep-alive 连接，换连接服务器回 `CSRF token mismatch`）。
/// 此前每个请求各建一个 Client，等于每次登录都新开一条连接，这类门户必然失败。
///
/// 超时因此不设在 Client 上（一个 Client 只能有一个全局超时），改由每个请求的
/// `RequestBuilder::timeout` 单独给：抓登录页 5s / 前置请求 10s / 登录 20s。
fn build_client(ignore_https_errors: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        // 每次登录尝试只建一个 Client，Cookie 仅在该次流程和所属域名内复用。
        .cookie_store(true)
        .no_proxy()
        .danger_accept_invalid_certs(ignore_https_errors)
        .user_agent(DEFAULT_USER_AGENT)
        .build()
        .map_err(|e| format!("客户端构建失败: {e}"))
}

/// 凭据模板仅在同源路径间沿用；协议、主机或有效端口变化须清理请求。
fn redirect_has_same_origin(origin: &url::Url, target: &url::Url) -> bool {
    target.scheme() == origin.scheme()
        && target.host_str() == origin.host_str()
        && target.port_or_known_default() == origin.port_or_known_default()
}

/// HTTPS 响应中的跳转不能把目标 URL 中的一次性令牌暴露给明文链路。
fn redirect_is_https_downgrade(origin: &url::Url, target: &url::Url) -> bool {
    origin.scheme() == "https" && target.scheme() == "http"
}

/// 抓取登录页原文（best effort）：脚本 ctx.page 数据源，失败返回空串
async fn fetch_login_page(client: &reqwest::Client, auth_url: &str) -> String {
    if auth_url.is_empty() {
        return String::new();
    }
    match send_with_redirects(
        client,
        HttpRequestMethod::Get,
        auth_url,
        "",
        "",
        PAGE_FETCH_TIMEOUT,
        false,
    )
    .await
    {
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

/// 发送一次请求，返回 (状态码, 响应体原文, 响应头逐行文本)。
///
/// 登录请求与前置请求共用（差别只有方法/地址/体积与超时），`client` 由调用方传入而
/// 非在此新建——两次请求必须落在同一条连接上，见 [`build_client`]。
async fn send_http(
    client: &reqwest::Client,
    method: HttpRequestMethod,
    url: &str,
    headers: &str,
    body: &str,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, String, String), String> {
    let response = send_http_detailed(client, method, url, headers, body, timeout, false).await?;
    Ok((response.status, response.body, response.headers_text))
}

/// 请求的可提取信息；原始响应头只留在执行期，不直接序列化进测试报告。
struct HttpResponse {
    status: reqwest::StatusCode,
    body: String,
    headers: reqwest::header::HeaderMap,
    headers_text: String,
    final_url: url::Url,
}

/// 为多变量取值保留最终 URL 与响应头；结果展示仍使用脱敏文本。
#[allow(clippy::too_many_arguments)]
async fn send_http_detailed(
    client: &reqwest::Client,
    method: HttpRequestMethod,
    url: &str,
    headers: &str,
    body: &str,
    timeout: Duration,
    stop_on_redirect: bool,
) -> Result<HttpResponse, String> {
    let resp = send_with_redirects(
        client,
        method,
        url,
        headers,
        body,
        timeout,
        stop_on_redirect,
    )
    .await?;
    let status = resp.status();
    let final_url = resp.url().clone();
    let response_headers = resp.headers().clone();
    let headers_text = format_response_headers(&response_headers);
    let (body, _charset) = read_limited_body(resp).await?;
    Ok(HttpResponse {
        status,
        body,
        headers: response_headers,
        headers_text,
        final_url,
    })
}

/// 逐跳发送请求；跨源后永久清除模板头和请求体，防止后续跳回原站时恢复凭据。
async fn send_with_redirects(
    client: &reqwest::Client,
    method: HttpRequestMethod,
    url: &str,
    headers: &str,
    body: &str,
    timeout: Duration,
    stop_on_redirect: bool,
) -> Result<reqwest::Response, String> {
    let mut current = url::Url::parse(url).map_err(|e| e.to_string())?;
    let mut method = method;
    let mut headers = headers;
    let mut body = body;
    let started = Instant::now();
    for hop in 0..=MAX_REDIRECTS {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "直连请求超时".to_string())?;
        let mut request = match method {
            HttpRequestMethod::Get => client.get(current.clone()),
            HttpRequestMethod::Post => {
                let mut request = client.post(current.clone());
                if !body.is_empty() {
                    request = request.body(body.to_string());
                }
                request
            }
        };
        let parsed = parse_headers(headers);
        for (name, value) in &parsed {
            request = request.header(name, value);
        }
        if method == HttpRequestMethod::Post
            && !body.is_empty()
            && !parsed
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            request = request.header("Content-Type", "application/x-www-form-urlencoded");
        }
        let response = request
            .timeout(remaining)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        if stop_on_redirect {
            return Ok(response);
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(response);
        };
        if hop == MAX_REDIRECTS {
            return Err("直连请求重定向次数过多".to_string());
        }
        let next = current.join(location).map_err(|e| e.to_string())?;
        if !matches!(next.scheme(), "http" | "https") || next.host_str().is_none() {
            return Err("直连请求重定向地址不是 HTTP(S) URL".to_string());
        }
        if redirect_is_https_downgrade(&current, &next) {
            return Err("直连请求拒绝 HTTPS 降级跳转".to_string());
        }
        if !redirect_has_same_origin(&current, &next) {
            headers = "";
            body = "";
        }
        if status == reqwest::StatusCode::SEE_OTHER
            || (matches!(status.as_u16(), 301 | 302) && method == HttpRequestMethod::Post)
        {
            method = HttpRequestMethod::Get;
            body = "";
        }
        current = next;
    }
    Err("直连请求重定向次数过多".to_string())
}

/// 从前置请求的响应体里取出值（字符串取原值，其余类型取 JSON 文本）。
///
/// `path` 是已解析好的点号路径（见 [`HttpPreRequest::extract_path`]）。容忍 JSONP
/// 包裹与前后脏字符：这类接口由门户前端 AJAX 调用，常见
/// `dr1003({"csrf_token":"..."})` 或带 BOM/空行的返回。
fn extract_pre_value(path: &str, body: &str) -> Result<String, String> {
    let raw = body.trim();
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(first) => {
            // JSONP 包裹（`dr1003({...})`）或前后脏字符：退一步取最外层花括号再试
            let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) else {
                return Err(format!("响应不是 JSON（取值 `json:{path}`）: {first}"));
            };
            if end <= start {
                return Err(format!("响应不是 JSON（取值 `json:{path}`）: {first}"));
            }
            serde_json::from_str(&raw[start..=end])
                .map_err(|e| format!("响应不是 JSON（取值 `json:{path}`）: {e}"))?
        }
    };

    let mut current = &value;
    for segment in path.split('.') {
        current = current
            .get(segment)
            .ok_or_else(|| format!("响应里没有字段 `{path}`"))?;
    }
    Ok(match current {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// 从单次响应的正文、响应头或地址提取字段；多字段规则与旧 JSON 规则共享终止语义。
fn extract_flow_value(
    rule: &HttpExtractRule,
    body: &str,
    headers: &reqwest::header::HeaderMap,
    final_url: &url::Url,
) -> Result<String, String> {
    let (kind, arg) = rule
        .source
        .trim()
        .split_once(':')
        .ok_or_else(|| "取值来源格式错误".to_string())?;
    let arg = arg.trim();
    match kind {
        "json" => extract_pre_value(arg, body),
        "header" => headers
            .get(arg)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| format!("响应头没有 `{arg}`")),
        "url" => query_value(final_url, arg),
        "redirect" => {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "响应没有 Location 头；请在本步骤启用停止自动跳转".to_string())?;
            let target = final_url
                .join(location)
                .map_err(|_| "Location 不是有效地址".to_string())?;
            query_value(&target, arg)
        }
        "html" => html_input_value(body, arg),
        "regex" => regex::Regex::new(arg)
            .map_err(|e| format!("取值正则无效: {e}"))?
            .captures(body)
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str().to_string())
            .ok_or_else(|| "响应未命中正则的第一个捕获组".to_string()),
        _ => Err("不支持的取值来源".into()),
    }
}

/// 查找响应或跳转 URL 中的第一个同名查询参数。
fn query_value(url: &url::Url, name: &str) -> Result<String, String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| format!("地址中没有参数 `{name}`"))
}

/// 读取常见门户表单的 input[name] 值；标签与属性顺序、单双引号均可变化。
fn html_input_value(body: &str, name: &str) -> Result<String, String> {
    use std::sync::OnceLock;
    static INPUT_TAG: OnceLock<regex::Regex> = OnceLock::new();
    static ATTRIBUTE: OnceLock<regex::Regex> = OnceLock::new();
    let tags = INPUT_TAG.get_or_init(|| {
        regex::Regex::new(r#"(?is)<input\b(?:[^>"']|"[^"]*"|'[^']*')*>"#)
            .expect("静态 input 标签表达式有效")
    });
    let attributes = ATTRIBUTE.get_or_init(|| {
        regex::Regex::new(r#"(?is)([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#)
            .expect("静态 HTML 属性表达式有效")
    });
    for tag in tags.find_iter(body) {
        let mut field_name = None;
        let mut field_value = None;
        for capture in attributes.captures_iter(tag.as_str()) {
            let key = capture.get(1).map_or("", |value| value.as_str());
            let value = (2..=4)
                .find_map(|index| capture.get(index))
                .map_or("", |value| value.as_str());
            if key.eq_ignore_ascii_case("name") {
                field_name = Some(value);
            } else if key.eq_ignore_ascii_case("value") {
                field_value = Some(value);
            }
        }
        if field_name == Some(name) {
            return field_value
                .map(decode_html_entities)
                .ok_or_else(|| format!("HTML 输入框 `{name}` 没有 value"));
        }
    }
    Err(format!("HTML 中没有名为 `{name}` 的输入框"))
}

/// 解码表单值里常见的 HTML 字符实体，避免把 `&amp;` 原样发回门户。
fn decode_html_entities(value: &str) -> String {
    use std::sync::OnceLock;
    static ENTITY: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = ENTITY.get_or_init(|| {
        regex::Regex::new(r"&(?:amp|lt|gt|quot|apos|#(?:[0-9]+|[xX][0-9a-fA-F]+));")
            .expect("静态字符实体表达式有效")
    });
    pattern
        .replace_all(value, |captures: &regex::Captures<'_>| match &captures[0] {
            "&amp;" => "&".to_string(),
            "&lt;" => "<".to_string(),
            "&gt;" => ">".to_string(),
            "&quot;" => "\"".to_string(),
            "&apos;" => "'".to_string(),
            raw => {
                let number = raw.trim_start_matches("&#").trim_end_matches(';');
                let parsed = number
                    .strip_prefix(['x', 'X'])
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| number.parse::<u32>().ok());
                parsed
                    .and_then(char::from_u32)
                    .map_or_else(|| raw.to_string(), |ch| ch.to_string())
            }
        })
        .into_owned()
}

/// 组装"还没走到登录请求就终结"的报告（客户端构建失败 / 脚本失败 / 前置请求失败）。
///
/// `rendered` 传**实际卡住的那一步**的渲染结果（前置请求就传前置请求的），测试面板要
/// 能看到失败那一步真实发出的东西，而不是一片空白。
fn abort_report(
    outcome: Outcome,
    message: String,
    rendered: Option<(&str, &str, &str)>,
    response: Option<(u16, &str, &str)>,
    script_error: Option<String>,
    duration_ms: u64,
) -> HttpAttemptReport {
    let (url, headers, body) = rendered.unwrap_or(("", "", ""));
    let (status, response_headers, response_snippet) = response.unwrap_or((0, "", ""));
    HttpAttemptReport {
        step_reports: Vec::new(),
        outcome,
        message,
        script_error,
        duration_ms,
        rendered_url: url.to_string(),
        rendered_headers: headers.to_string(),
        rendered_body: body.to_string(),
        status: (status != 0).then_some(status),
        response_headers: response_headers.to_string(),
        response_snippet: response_snippet.to_string(),
    }
}

/// 响应头 → 逐行 `Key: Value` 文本（截断到上限），供测试结果面板排查排查
fn format_response_headers(headers: &reqwest::header::HeaderMap) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        if name == reqwest::header::SET_COOKIE {
            out.push_str("set-cookie: ***\n");
            continue;
        }
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
        add_secret(&mut secrets, v);
    }
    normalize_secrets(&mut secrets);
    secrets
}

/// 同时保留原文与 URL 编码形态；变量覆盖前的值也必须持续参与脱敏。
fn add_secret(secrets: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        return;
    }
    secrets.push(value.to_string());
    let encoded = url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    if encoded != value {
        secrets.push(encoded);
    }
}

/// 长值优先替换，按字典序让同值相邻后完整去重。
fn normalize_secrets(secrets: &mut Vec<String>) {
    secrets.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    secrets.dedup();
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

/// 在无 IO 沙箱内执行用户加密脚本；正式运行时由临时子进程承载并受墙钟时限约束。
///
/// 契约：脚本需定义 `function transform(ctx)`，返回对象；其字符串/数字/布尔
/// 字段成为可被模板引用的占位符值。ctx 含
/// `username/password/isp/auth_url/page/local_ip/local_mac`。
///
/// `local` 为本机主用接口的 (IPv4, MAC)，取不到时均为空串——部分门户（eportal /
/// Dr.COM）的字段密钥由来源 IP 推导，没有它就只能从页面里找补。
async fn execute_crypto_script(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    isp: &str,
    auth_url: &str,
    local: (&str, &str),
    page: String,
) -> Result<BTreeMap<String, String>, String> {
    execute_crypto_script_with_vars(
        script,
        username,
        password,
        isp,
        auth_url,
        local,
        page,
        BTreeMap::new(),
    )
    .await
}

/// 纯计算子进程的输入；密码只经匿名 stdin 管道下发，不进入命令行与环境变量。
#[derive(serde::Serialize, serde::Deserialize)]
struct TransformWorkerInput {
    script: String,
    username: String,
    password: String,
    isp: String,
    auth_url: String,
    local_ip: String,
    local_mac: String,
    page: String,
    vars: BTreeMap<String, String>,
}

/// 同一可执行文件的隐藏计算模式；只读 stdin、只写 JSON 结果到 stdout。
pub fn run_transform_worker() -> std::io::Result<()> {
    use std::io::{Read, Write};

    let mut bytes = Zeroizing::new(Vec::new());
    std::io::stdin()
        .take((MAX_TRANSFORM_IPC_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_TRANSFORM_IPC_BYTES {
        return Err(std::io::Error::other("计算脚本输入过大"));
    }
    let input: TransformWorkerInput =
        serde_json::from_slice(&bytes).map_err(|_| std::io::Error::other("计算脚本输入无效"))?;
    let password = Zeroizing::new(input.password);
    let mut result = run_script_in_sandbox_with_vars(
        &input.script,
        &input.username,
        &password,
        &input.isp,
        &input.auth_url,
        &input.local_ip,
        &input.local_mac,
        input.page,
        input.vars,
    );
    let mut output = serde_json::to_vec(&result)?;
    if output.len() > MAX_TRANSFORM_IPC_BYTES {
        result = Err("计算结果过大".into());
        output = serde_json::to_vec(&result)?;
    }
    std::io::stdout().write_all(&output)
}

/// 执行流程中的计算步骤，并把前面步骤提取的变量提供给 `ctx.vars`。
#[allow(clippy::too_many_arguments)]
async fn execute_crypto_script_with_vars(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    isp: &str,
    auth_url: &str,
    local: (&str, &str),
    page: String,
    vars: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let script = script.to_string();
    let username = username.to_string();
    let password = Zeroizing::new(password.to_string());
    let isp = isp.to_string();
    let auth_url = auth_url.to_string();
    let local_ip = local.0.to_string();
    let local_mac = local.1.to_string();

    #[cfg(test)]
    {
        // 单元测试运行在 cargo 的测试可执行文件内，直接测同一纯函数；真实
        // 子进程路径由独立集成测试覆盖。
        run_script_in_sandbox_with_vars(
            &script, &username, &password, &isp, &auth_url, &local_ip, &local_mac, page, vars,
        )
    }

    #[cfg(not(test))]
    {
        use tokio::io::AsyncWriteExt;

        let input = TransformWorkerInput {
            script,
            username,
            password: password.to_string(),
            isp,
            auth_url,
            local_ip,
            local_mac,
            page,
            vars,
        };
        let payload = Zeroizing::new(
            serde_json::to_vec(&input).map_err(|_| "计算脚本输入编码失败".to_string())?,
        );
        if payload.len() > MAX_TRANSFORM_IPC_BYTES {
            return Err("计算脚本输入过大".into());
        }
        let exe = std::env::current_exe().map_err(|_| "无法定位计算子进程".to_string())?;
        let mut command = tokio::process::Command::new(exe);
        command
            .arg("--http-transform-worker")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env_clear()
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);

        let mut child = command
            .spawn()
            .map_err(|_| "计算子进程启动失败".to_string())?;
        let execution = async {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "计算子进程输入管道不可用".to_string())?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|_| "计算子进程输入失败".to_string())?;
            drop(stdin);
            let output = child
                .wait_with_output()
                .await
                .map_err(|_| "计算子进程等待失败".to_string())?;
            if !output.status.success() {
                return Err("计算子进程异常退出".to_string());
            }
            serde_json::from_slice::<Result<BTreeMap<String, String>, String>>(&output.stdout)
                .map_err(|_| "计算子进程结果无效".to_string())?
        };
        tokio::time::timeout(SCRIPT_TIMEOUT, execution)
            .await
            .map_err(|_| format!("脚本执行超时（上限 {}ms）", SCRIPT_TIMEOUT.as_millis()))?
    }
}

/// boa 沙箱执行：注册内置函数 → eval 脚本 → 调用 transform(ctx) → 序列化返回值
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn run_script_in_sandbox(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    isp: &str,
    auth_url: &str,
    local_ip: &str,
    local_mac: &str,
    page: String,
) -> Result<BTreeMap<String, String>, String> {
    run_script_in_sandbox_with_vars(
        script,
        username,
        password,
        isp,
        auth_url,
        local_ip,
        local_mac,
        page,
        BTreeMap::new(),
    )
}

/// 沙箱计算的完整入口；新增变量仅经 `ctx.vars` 传入，不改旧脚本的入参。
#[allow(clippy::too_many_arguments)]
fn run_script_in_sandbox_with_vars(
    script: &str,
    username: &str,
    password: &Zeroizing<String>,
    isp: &str,
    auth_url: &str,
    local_ip: &str,
    local_mac: &str,
    page: String,
    vars: BTreeMap<String, String>,
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
            "isp": isp,
            "auth_url": auth_url,
            "page": page,
            "local_ip": local_ip,
            "local_mac": local_mac,
            "vars": vars,
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
    // 深澜 challenge 协议：token 作为 HMAC-MD5 密钥，密码作为消息。
    register!(
        boa_engine::JsString::from("hmac_md5"),
        2,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            let key = js_arg(args, 0, ctx)?;
            let data = js_arg(args, 1, ctx)?;
            type HmacMd5 = hmac::Hmac<md5::Md5>;
            let mut mac = HmacMd5::new_from_slice(key.as_bytes()).map_err(|e| {
                boa_engine::JsError::from_opaque(boa_engine::JsString::from(e.to_string()).into())
            })?;
            mac.update(data.as_bytes());
            Ok(boa_engine::JsString::from(format!("{:x}", mac.finalize().into_bytes())).into())
        })
    );
    register!(
        boa_engine::JsString::from("srun_info"),
        2,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            let info = js_arg(args, 0, ctx)?;
            let challenge = js_arg(args, 1, ctx)?;
            let encoded = crate::login::srun::encode_info(&info, &challenge).map_err(|error| {
                boa_engine::JsError::from_opaque(boa_engine::JsString::from(error).into())
            })?;
            Ok(boa_engine::JsString::from(encoded).into())
        })
    );
    // 仅适用于参考项目中的上海大学锐捷公钥与 MAC 拼接规则。
    register!(
        boa_engine::JsString::from("shu_ruijie_password"),
        2,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            let password = js_arg(args, 0, ctx)?;
            let mac = js_arg(args, 1, ctx)?;
            let encrypted =
                crate::login::ruijie::shu_password(&password, &mac).map_err(|error| {
                    boa_engine::JsError::from_opaque(boa_engine::JsString::from(error).into())
                })?;
            Ok(boa_engine::JsString::from(encrypted).into())
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

    /// 保存闸口（`tasks` 层）与执行闸口（本模块）的体积上限必须同值。
    ///
    /// 两处各写一份常量是历史形态，而"保存侧比执行侧松"的后果很具体：用户能存下一份
    /// **必然登不上**的任务（登录一开始就被 [`HttpLoginRequest::validate`] 拒掉），
    /// 界面上没有任何提示，`docs/guides/http-login-guide.md` 写的又正是执行侧那个数
    /// （"请求头 64 KiB …… 超过上限时保存会被拒绝"）。请求头那一档曾经就是 256 KiB vs
    /// 64 KiB。这条测试是"必须同值"的钉子：改任一处而不改另一处会立刻失败。
    #[test]
    fn login_and_task_size_limits_agree() {
        assert_eq!(MAX_URL_BYTES, crate::tasks::MAX_HTTP_URL_BYTES);
        assert_eq!(MAX_HEADERS_BYTES, crate::tasks::MAX_HTTP_HEADERS_BYTES);
        assert_eq!(MAX_REQUEST_BODY_BYTES, crate::tasks::MAX_HTTP_BODY_BYTES);
    }

    fn request(url: String) -> HttpLoginRequest {
        HttpLoginRequest {
            steps: Vec::new(),
            result_step_id: String::new(),
            method: HttpRequestMethod::Get,
            url,
            headers: "X-User: {username}".into(),
            body: String::new(),
            success_pattern: "登录成功".into(),
            failure_pattern: "密码错误".into(),
            failure_action: crate::tasks::HttpFailureAction::Credential,
            success_check: crate::tasks::HttpSuccessCheck::default(),
            crypto_script: String::new(),
            pre_request: None,
            logout_request: None,
            username: "abc".into(),
            password: Zeroizing::new("abcdef".into()),
            isp: String::new(),
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

    /// 跨源跳转继续访问目标，但新请求不能带自定义凭据头或 POST 凭据体。
    #[tokio::test]
    async fn cross_origin_redirect_never_sends_credentials() {
        for status in [302, 307, 308] {
            let (target, received) = spawn_capturing_response(200, "登录成功", "").await;
            let (source, original) =
                spawn_capturing_response(status, "", &format!("Location: {target}\r\n")).await;
            let mut req = request(source);
            req.method = HttpRequestMethod::Post;
            req.headers = "X-Password: {password}".into();
            req.body = "password={password}".into();
            let report = run_once(&req).await;
            assert_eq!(
                report.outcome,
                Outcome::Success,
                "{status}: {}",
                report.message
            );
            let original = original.await.unwrap();
            assert!(
                original.to_ascii_lowercase().contains("x-password: abcdef"),
                "{status}: {original}"
            );
            let redirected = received.await.unwrap();
            assert!(
                !redirected.to_ascii_lowercase().contains("x-password"),
                "{status}: {redirected}"
            );
            assert!(
                !redirected.contains("password=abcdef"),
                "{status}: {redirected}"
            );
            if status == 302 {
                assert!(redirected.starts_with("GET "), "{redirected}");
            } else {
                assert!(redirected.starts_with("POST "), "{redirected}");
            }
        }
    }

    /// 门户自己的相对跳转仍可正常落到结果页，不影响常见登录流程。
    #[tokio::test]
    async fn same_origin_redirect_is_followed() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n登录成功",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        let report = run_once(&request(format!("http://{addr}/login"))).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
    }

    #[test]
    fn redirect_origin_and_downgrade_are_distinguished() {
        let origin = url::Url::parse("https://portal.example/login").unwrap();
        assert!(redirect_has_same_origin(
            &origin,
            &url::Url::parse("https://portal.example/done").unwrap()
        ));
        for target in [
            "http://portal.example/done",
            "https://other.example/done",
            "https://portal.example:8443/done",
        ] {
            assert!(!redirect_has_same_origin(
                &origin,
                &url::Url::parse(target).unwrap()
            ));
        }
        assert!(redirect_is_https_downgrade(
            &origin,
            &url::Url::parse("http://other.example/done").unwrap()
        ));
        assert!(!redirect_is_https_downgrade(
            &origin,
            &url::Url::parse("https://other.example/done").unwrap()
        ));
    }

    /// 网络检测判定模式：响应体与状态码都不参与成功判定——成功关键字未命中
    /// 也判候选成功（Outcome::Success），最终成败由会话层登录后网络验证决定。
    #[tokio::test]
    async fn network_check_mode_ignores_response_body() {
        let (url, _rx) = spawn_capturing_response(200, "门户返回了意料之外的内容", "").await;
        let mut req = request(url);
        req.success_check = crate::tasks::HttpSuccessCheck::Network;
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        assert!(
            report.message.contains("网络检测"),
            "消息应说明将由网络检测判定: {}",
            report.message
        );
    }

    /// 网络检测模式下失败关键字仍生效：门户明确报错（密码错误等）快速失败，
    /// 不必等探测结果。
    #[tokio::test]
    async fn network_check_mode_still_honors_failure_pattern() {
        let (url, _rx) =
            spawn_capturing_response(200, "{\"code\":1,\"msg\":\"密码错误\"}", "").await;
        let mut req = request(url);
        req.success_check = crate::tasks::HttpSuccessCheck::Network;
        let report = run_once(&req).await;
        assert_eq!(
            report.outcome,
            Outcome::InvalidCredential,
            "{}",
            report.message
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

    /// 空请求地址必须明确指向「任务 · 直连任务」而不是方案——直连配置已整体搬到
    /// 任务里，报错文案若仍说"请在方案里填写"，用户会去错页面找不到该字段。
    #[test]
    fn from_task_rejects_empty_url() {
        let task = HttpTaskConfig::default();
        // HttpLoginRequest 未派生 Debug（内含 Zeroizing 凭据），故不能用 expect_err/unwrap_err
        let err = match HttpLoginRequest::from_task(&task, "u", "p", "", "", false, true) {
            Ok(_) => panic!("空地址必须被拒"),
            Err(e) => e,
        };
        assert!(err.contains("直连任务缺少请求地址"), "{err}");
        assert!(err.contains("任务 · 直连任务"), "文案须指向任务页: {err}");

        // 仅空白同样视为空
        let blank = HttpTaskConfig {
            url: "   ".into(),
            ..HttpTaskConfig::default()
        };
        assert!(HttpLoginRequest::from_task(&blank, "u", "p", "", "", false, true).is_err());
    }

    /// 证书策略缺省解析：任务未设置时跟随全局（与浏览器渠道同口径），
    /// 显式值覆盖全局
    #[test]
    fn from_task_falls_back_to_global_cert_policy() {
        let task = HttpTaskConfig {
            url: "http://10.0.0.1/login".into(),
            ..HttpTaskConfig::default()
        };

        let followed = HttpLoginRequest::from_task(&task, "u", "p", "", "", false, true).unwrap();
        assert!(followed.ignore_https_errors, "未设置时应跟随全局 true");

        let followed_strict = HttpLoginRequest::from_task(&task, "u", "p", "", "", false, false)
            .expect("全局 false 同样可构成合法请求");
        assert!(
            !followed_strict.ignore_https_errors,
            "未设置时应跟随全局 false"
        );

        // 显式覆盖优先于全局
        let strict = HttpTaskConfig {
            ignore_https_errors: Some(false),
            ..task.clone()
        };
        let overridden =
            HttpLoginRequest::from_task(&strict, "u", "p", "", "", false, true).unwrap();
        assert!(!overridden.ignore_https_errors, "任务显式设置必须覆盖全局");
    }

    /// 任务字段逐项落到请求上（方法/头/体/判定关键字/脚本/认证地址/抓页开关），
    /// 且账号 trim、密码进 Zeroizing —— 与旧的「方案快照构造」逐字段映射等价
    #[test]
    fn from_task_maps_all_fields_and_trims() {
        let task = HttpTaskConfig {
            method: HttpRequestMethod::Post,
            url: "  http://10.0.0.1/login  ".into(),
            headers: "X-User: {username}".into(),
            body: "u={username}&p={password}".into(),
            success_pattern: "登录成功".into(),
            failure_pattern: "密码错误".into(),
            crypto_script: "function transform(ctx) { return {}; }".into(),
            ..HttpTaskConfig::default()
        };
        let req = HttpLoginRequest::from_task(
            &task,
            " 20230001 ",
            "pw",
            " 移动 ",
            " http://10.0.0.1/ ",
            true,
            true,
        )
        .unwrap();
        assert_eq!(req.method, HttpRequestMethod::Post);
        assert_eq!(req.url, "http://10.0.0.1/login", "地址须 trim");
        assert_eq!(req.headers, "X-User: {username}");
        assert_eq!(req.body, "u={username}&p={password}");
        assert_eq!(req.success_pattern, "登录成功");
        assert_eq!(req.failure_pattern, "密码错误");
        assert!(req.uses_crypto_script());
        assert_eq!(req.username, "20230001", "账号须 trim");
        assert_eq!(req.password.as_str(), "pw");
        assert_eq!(req.isp, "移动", "运营商须 trim");
        assert_eq!(req.auth_url, "http://10.0.0.1/", "认证地址须 trim");
        assert!(req.fetch_page, "抓页开关由调用方决定（测试端点可关）");
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
            "",
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
            "",
            "http://portal.example/",
            "10.20.30.40",
            "00:1a:2b:3c:4d:5e",
            String::new(),
        )
        .unwrap();
        assert_eq!(values["ip"], "10.20.30.40");
        assert_eq!(values["mac"], "00:1a:2b:3c:4d:5e");
    }

    #[test]
    fn script_exposes_srun_helpers() {
        let password = Zeroizing::new("p".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { digest: hmac_md5('abc', ctx.password), info: srun_info('{\"username\":\"u\",\"password\":\"p\",\"ip\":\"1.2.3.4\",\"acid\":1,\"enc_ver\":\"srun_bx1\"}', 'abc') }; }",
            "u", &password, "", "", "", "", String::new(),
        ).unwrap();
        assert_eq!(values["digest"], "05f894e7889213b5f9bb6aaae84f75f6");
        assert!(values["info"].starts_with("{SRBX1}"));
    }

    #[test]
    fn script_exposes_shu_ruijie_helper() {
        let password = Zeroizing::new("123".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { encrypted: shu_ruijie_password(ctx.password, '5ae915bf808f82732e98e01f704f00cd') }; }",
            "u", &password, "", "", "", "", String::new(),
        ).unwrap();
        assert!(values["encrypted"].starts_with("91a0e02175f6a0b22ad"));
    }

    /// ctx 暴露方案运营商（原样透传，门户侧表示法由任务脚本映射——Dr.COM
    /// eportal 的 @cmcc 后缀类任务依赖它把方案的「移动/联通/电信」转成后缀）。
    #[test]
    fn script_ctx_exposes_profile_isp() {
        let password = Zeroizing::new("pw".to_string());
        let values = run_script_in_sandbox(
            "function transform(ctx) { return { suffix: ctx.isp === \"移动\" ? \"@cmcc\" : ctx.isp }; }",
            "user",
            &password,
            "移动",
            "http://portal.example/",
            "",
            "",
            String::new(),
        )
        .unwrap();
        assert_eq!(values["suffix"], "@cmcc");
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
    async fn retryable_failure_pattern_uses_retry_path() {
        let url = spawn_response(200, "网关繁忙，请稍后重试").await;
        let mut req = request(url);
        req.failure_pattern = "网关繁忙".into();
        req.failure_action = crate::tasks::HttpFailureAction::Retry;
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::AssertionFailed);
    }

    #[tokio::test]
    async fn manual_failure_pattern_stops_retry() {
        let url = spawn_response(200, "请完成短信验证").await;
        let mut req = request(url);
        req.failure_pattern = "短信验证".into();
        req.failure_action = crate::tasks::HttpFailureAction::Manual;
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::ManualRequired);
        assert_eq!(
            crate::login::session::classify(report.outcome),
            crate::login::session::ResultAction::Terminal(
                crate::login::session::LoginTerminal::Failed
            )
        );
    }

    /// 旧式任务与单请求有序流程必须保持同一判定口径；消息可按各自格式展示。
    #[tokio::test]
    async fn legacy_and_flow_response_verdicts_match() {
        use crate::tasks::{HttpFailureAction, HttpSuccessCheck};

        let cases = [
            (
                200,
                "登录成功，但密码错误",
                HttpFailureAction::Credential,
                HttpSuccessCheck::Response,
                "登录成功",
                "密码错误",
                Outcome::InvalidCredential,
            ),
            (
                200,
                "网关繁忙",
                HttpFailureAction::Retry,
                HttpSuccessCheck::Response,
                "登录成功",
                "网关繁忙",
                Outcome::AssertionFailed,
            ),
            (
                200,
                "请完成短信验证",
                HttpFailureAction::Manual,
                HttpSuccessCheck::Network,
                "登录成功",
                "短信验证",
                Outcome::ManualRequired,
            ),
            (
                503,
                "未知响应",
                HttpFailureAction::Credential,
                HttpSuccessCheck::Network,
                "登录成功",
                "密码错误",
                Outcome::Success,
            ),
            (
                503,
                "未知响应",
                HttpFailureAction::Credential,
                HttpSuccessCheck::Response,
                "",
                "密码错误",
                Outcome::AssertionFailed,
            ),
            (
                503,
                "登录成功",
                HttpFailureAction::Credential,
                HttpSuccessCheck::Response,
                "登录成功",
                "密码错误",
                Outcome::Success,
            ),
        ];
        for (status, body, action, check, success, failure, expected) in cases {
            for use_flow in [false, true] {
                let url = spawn_response(status, body).await;
                let mut req = request(url.clone());
                req.failure_action = action;
                req.success_check = check;
                req.success_pattern = success.into();
                req.failure_pattern = failure.into();
                if use_flow {
                    req.steps = vec![HttpFlowStep {
                        id: "login".into(),
                        name: "登录请求".into(),
                        url,
                        ..Default::default()
                    }];
                    req.result_step_id = "login".into();
                }
                let report = run_once(&req).await;
                assert_eq!(
                    report.outcome, expected,
                    "flow={use_flow}: {}",
                    report.message
                );
                assert!(!report.rendered_url.contains("abcdef"));
                assert!(!report.response_snippet.contains("abcdef"));
            }
        }
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

    // ===== 前置请求（CSRF 令牌绑定 TCP 连接的门户） =====

    /// 起一个"令牌绑连接"的极简门户，返回 `(base_url, 收到的 (来源端口, 路径) 流)`。
    ///
    /// 复现实测过的门户约束（河南科技大学「大学掌」体系那类）：`GET /api/csrf-token`
    /// 发一个令牌，`POST /api/account/login` 必须带上**同一条连接**上取到的令牌，
    /// 换连接即回 `400 {"error":"CSRF token mismatch"}`。响应刻意不写
    /// `Connection: close`，连接因此可被客户端 keep-alive 复用——这正是被测代码要用的性质。
    async fn spawn_csrf_portal() -> (String, tokio::sync::mpsc::Receiver<(u16, String)>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            // 令牌按"来源端口"记账：换了连接就查不到自己的令牌
            let tokens: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<u16, String>>> =
                std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    break;
                };
                let tokens = tokens.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.into_split();
                    let mut buf: Vec<u8> = Vec::new();
                    loop {
                        // 读到"请求头 + Content-Length 指定的体"齐全为止
                        let (head_end, content_length) = loop {
                            if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                                let len = head
                                    .lines()
                                    .find_map(|line| {
                                        let (k, v) = line.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse::<usize>().ok())?
                                    })
                                    .unwrap_or(0);
                                if buf.len() >= head_end + 4 + len {
                                    break (head_end, len);
                                }
                            }
                            let mut chunk = [0_u8; 4096];
                            match reader.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };

                        let request =
                            String::from_utf8_lossy(&buf[..head_end + 4 + content_length])
                                .to_string();
                        buf.drain(..head_end + 4 + content_length);
                        let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                        let _ = tx.send((peer.port(), path.clone())).await;

                        let (status, reason, body) = if path.starts_with("/api/csrf-token") {
                            let token = format!("tok-{}", peer.port());
                            tokens.lock().await.insert(peer.port(), token.clone());
                            (200, "OK", format!("{{\"csrf_token\":\"{token}\"}}"))
                        } else if path.starts_with("/api/account/login") {
                            let sent = request
                                .lines()
                                .find_map(|line| {
                                    let (k, v) = line.split_once(':')?;
                                    k.eq_ignore_ascii_case("x-csrf-token")
                                        .then(|| v.trim().to_string())
                                })
                                .unwrap_or_default();
                            let expected = tokens
                                .lock()
                                .await
                                .get(&peer.port())
                                .cloned()
                                .unwrap_or_default();
                            if !expected.is_empty() && sent == expected {
                                (200, "OK", "{\"code\":0,\"msg\":\"ok\"}".to_string())
                            } else {
                                (
                                    400,
                                    "Bad Request",
                                    "{\"error\":\"CSRF token mismatch\"}".to_string(),
                                )
                            }
                        } else {
                            (404, "Not Found", "{}".to_string())
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if writer.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        (format!("http://{addr}"), rx)
    }

    /// 前置请求与登录请求必须落在**同一条连接**上——这是令牌绑连接的门户能否登录的
    /// 唯一判据，也是把 Client 提到整次尝试一处的原因。
    #[tokio::test]
    async fn pre_request_shares_connection_with_login_request() {
        let (base, mut seen) = spawn_csrf_portal().await;
        let mut req = request(format!("{base}/api/account/login"));
        req.method = HttpRequestMethod::Post;
        req.body = "username={username}&password={password}".into();
        req.headers = "X-CSRF-Token: {csrf}".into();
        req.success_pattern = "\"code\":0".into();
        req.failure_pattern.clear();
        req.pre_request = Some(HttpPreRequest {
            method: HttpRequestMethod::Get,
            url: format!("{base}/api/csrf-token"),
            extract: "json:csrf_token".into(),
            name: "csrf".into(),
            ..Default::default()
        });

        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);

        let token_call = seen.recv().await.unwrap();
        let login_call = seen.recv().await.unwrap();
        assert!(
            token_call.1.starts_with("/api/csrf-token"),
            "{token_call:?}"
        );
        assert!(
            login_call.1.starts_with("/api/account/login"),
            "{login_call:?}"
        );
        assert_eq!(
            token_call.0, login_call.0,
            "前置请求与登录请求落在了两条连接上（CSRF 绑连接的门户会拒登）"
        );
        // 取到的令牌属秘密：报告里不得出现明文
        assert!(
            !report.rendered_headers.contains("tok-"),
            "报告泄露令牌: {}",
            report.rendered_headers
        );
        assert!(report.rendered_headers.contains("***"));
    }

    /// 有序流程能在取回令牌之后计算字段，再发送登录请求；每一步的报告都须脱敏。
    #[tokio::test]
    async fn flow_extract_transform_and_login_share_state() {
        let (base, mut seen) = spawn_csrf_portal().await;
        let mut req = request(String::new());
        req.success_pattern = "\"code\":0".into();
        req.failure_pattern.clear();
        req.result_step_id = "login".into();
        req.steps = vec![
            HttpFlowStep {
                id: "challenge".into(),
                name: "获取令牌".into(),
                url: format!("{base}/api/csrf-token"),
                extract: "json:csrf_token".into(),
                extract_as: "csrf".into(),
                ..Default::default()
            },
            HttpFlowStep {
                id: "sign".into(),
                name: "计算字段".into(),
                kind: HttpFlowStepKind::Transform,
                script: "function transform(ctx) { return { signed: ctx.vars.csrf }; }".into(),
                ..Default::default()
            },
            HttpFlowStep {
                id: "login".into(),
                name: "提交登录".into(),
                method: HttpRequestMethod::Post,
                url: format!("{base}/api/account/login"),
                headers: "X-CSRF-Token: {signed}".into(),
                body: "username={username}&password={password}".into(),
                ..Default::default()
            },
        ];
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        assert_eq!(report.step_reports.len(), 3);
        assert!(
            report
                .step_reports
                .iter()
                .all(|step| step.outcome == "success")
        );
        assert!(!report.step_reports[0].response_snippet.contains("tok-"));
        assert!(!report.step_reports[2].rendered_headers.contains("tok-"));
        assert!(report.step_reports[2].rendered_headers.contains("***"));
        let challenge = seen.recv().await.unwrap();
        let login = seen.recv().await.unwrap();
        assert_eq!(challenge.0, login.0);
    }

    /// 同一步骤的取值必须整体生效：忽略第二条取值失败时，第一条不能覆盖已有变量。
    #[tokio::test]
    async fn flow_ignored_extract_failure_keeps_previous_variables() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel(1);
        let app = axum::Router::new()
            .route(
                "/extract",
                axum::routing::get(|| async { r#"{"token":"new-secret"}"# }),
            )
            .route(
                "/check",
                axum::routing::get(move |uri: axum::http::Uri| {
                    let seen_tx = seen_tx.clone();
                    async move {
                        seen_tx.send(uri.to_string()).await.unwrap();
                        "登录成功"
                    }
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut req = request(String::new());
        req.result_step_id = "check".into();
        req.steps = vec![
            HttpFlowStep {
                id: "extract".into(),
                name: "提取字段".into(),
                url: format!("{base}/extract"),
                extracts: vec![
                    crate::tasks::HttpExtractRule {
                        source: "json:token".into(),
                        name: "password".into(),
                    },
                    crate::tasks::HttpExtractRule {
                        source: "json:missing".into(),
                        name: "missing".into(),
                    },
                ],
                on_error: HttpStepOnError::Continue,
                ..Default::default()
            },
            HttpFlowStep {
                id: "check".into(),
                name: "检查变量".into(),
                url: format!("{base}/check?value={{password}}"),
                ..Default::default()
            },
        ];

        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        assert_eq!(report.step_reports[0].outcome, "ignored");
        assert!(report.step_reports[0].produced_vars.is_empty());
        assert!(
            !report.step_reports[0]
                .response_snippet
                .contains("new-secret")
        );
        assert_eq!(seen_rx.recv().await.as_deref(), Some("/check?value=abcdef"));
    }

    /// 同一次流程的后续请求能带上门户设置的 Cookie，且无需把它写入任务模板。
    #[tokio::test]
    async fn flow_reuses_portal_cookie() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route(
                "/session",
                axum::routing::get(|| async {
                    (
                        [(axum::http::header::SET_COOKIE, "sid=flow-test; Path=/")],
                        "ok",
                    )
                }),
            )
            .route(
                "/login",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    if headers
                        .get(axum::http::header::COOKIE)
                        .is_some_and(|value| {
                            value
                                .as_bytes()
                                .windows(13)
                                .any(|part| part == b"sid=flow-test")
                        })
                    {
                        "logged-in"
                    } else {
                        "missing-cookie"
                    }
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let mut req = request(String::new());
        req.success_pattern = "logged-in".into();
        req.failure_pattern = "missing-cookie".into();
        req.result_step_id = "login".into();
        req.steps = vec![
            HttpFlowStep {
                id: "session".into(),
                name: "创建会话".into(),
                url: format!("{base}/session"),
                ..Default::default()
            },
            HttpFlowStep {
                id: "login".into(),
                name: "提交登录".into(),
                url: format!("{base}/login"),
                ..Default::default()
            },
        ];
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        assert!(
            report.step_reports[0]
                .response_headers
                .contains("set-cookie: ***")
        );
    }

    /// 后续计算覆盖密码时，先前步骤的请求与响应仍须用原密码脱敏。
    #[tokio::test]
    async fn flow_redacts_values_before_and_after_override() {
        let (base, mut seen) = spawn_recording_portal().await;
        let mut req = request(String::new());
        req.password = Zeroizing::new("old pass".into());
        req.result_step_id = "login".into();
        req.steps = vec![
            HttpFlowStep {
                id: "first".into(),
                name: "首次请求".into(),
                method: HttpRequestMethod::Post,
                url: format!("{base}/first?secret={{password}}"),
                body: "password={password}".into(),
                ..Default::default()
            },
            HttpFlowStep {
                id: "change".into(),
                name: "变换密码".into(),
                kind: HttpFlowStepKind::Transform,
                script: "function transform(ctx) { const out = { password: 'newpass' }; out[ctx.password] = 'x'; return out; }".into(),
                ..Default::default()
            },
            HttpFlowStep {
                id: "login".into(),
                name: "登录请求".into(),
                method: HttpRequestMethod::Post,
                url: format!("{base}/login"),
                body: "password={password}".into(),
                ..Default::default()
            },
        ];
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        let diagnostics = serde_json::to_string(&report.step_reports).unwrap();
        assert!(!diagnostics.contains("old pass"), "{diagnostics}");
        assert!(!diagnostics.contains("old%20pass"), "{diagnostics}");
        assert!(!diagnostics.contains("newpass"), "{diagnostics}");
        assert!(diagnostics.contains("***"));
        assert_eq!(seen.recv().await.unwrap().1, "/first?secret=old%20pass");
        assert_eq!(seen.recv().await.unwrap().1, "/login");
    }

    /// 旧式任务的脚本异常也可能包含凭据，错误消息和结构化字段都必须脱敏。
    #[tokio::test]
    async fn legacy_script_error_redacts_password() {
        let mut req = request("http://127.0.0.1/login".into());
        req.password = Zeroizing::new("legacy-secret".into());
        req.crypto_script = "function transform(ctx) { throw new Error(ctx.password); }".into();
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::UnknownError);
        assert!(!report.message.contains("legacy-secret"));
        assert!(
            !report
                .script_error
                .unwrap_or_default()
                .contains("legacy-secret")
        );
    }

    /// 对照组：不带前置请求时同一门户直接拒登——证明上一条测试不是白过
    #[tokio::test]
    async fn csrf_portal_rejects_login_without_token() {
        let (base, _seen) = spawn_csrf_portal().await;
        let mut req = request(format!("{base}/api/account/login"));
        req.method = HttpRequestMethod::Post;
        req.body = "username={username}".into();
        req.success_pattern = "\"code\":0".into();
        req.failure_pattern.clear();

        let report = run_once(&req).await;
        assert_eq!(
            report.outcome,
            Outcome::AssertionFailed,
            "{}",
            report.message
        );
        assert!(
            report.response_snippet.contains("CSRF token mismatch"),
            "{}",
            report.response_snippet
        );
    }

    /// 前置请求取不到字段 → 终态失败，并把**前置请求**的请求与响应带进报告
    /// （否则测试面板只会显示一片空白，用户无从判断是 token 接口还是登录接口不对）
    #[tokio::test]
    async fn pre_request_missing_field_is_terminal_and_reports_its_own_exchange() {
        let (base, _seen) = spawn_csrf_portal().await;
        let mut req = request(format!("{base}/api/account/login"));
        req.pre_request = Some(HttpPreRequest {
            url: format!("{base}/api/csrf-token"),
            extract: "json:not_here".into(),
            ..Default::default()
        });

        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::UnknownError);
        assert!(
            report.message.contains("前置请求未取到占位符"),
            "{}",
            report.message
        );
        assert!(report.message.contains("`not_here`"), "{}", report.message);
        assert!(
            report.rendered_url.ends_with("/api/csrf-token"),
            "报告应展示前置请求的地址: {}",
            report.rendered_url
        );
        assert!(
            report.response_snippet.contains("csrf_token"),
            "报告应带回前置请求的响应: {}",
            report.response_snippet
        );
    }

    /// 旧任务改成有序步骤后，取不到前置字段仍是终态，不额外重试登录。
    #[tokio::test]
    async fn flow_missing_pre_request_field_keeps_legacy_terminal_outcome() {
        let (base, mut seen) = spawn_csrf_portal().await;
        let mut req = request(String::new());
        req.result_step_id = "login".into();
        req.steps = vec![
            HttpFlowStep {
                id: "prepare".into(),
                name: "获取登录参数".into(),
                url: format!("{base}/api/csrf-token"),
                extract: "json:not_here".into(),
                ..Default::default()
            },
            HttpFlowStep {
                id: "login".into(),
                name: "登录请求".into(),
                method: HttpRequestMethod::Post,
                url: format!("{base}/api/account/login"),
                body: "token={not_here}".into(),
                ..Default::default()
            },
        ];
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::UnknownError);
        assert_eq!(report.step_reports.len(), 1);
        assert!(
            report.message.contains("提取变量失败"),
            "{}",
            report.message
        );
        assert!(seen.recv().await.unwrap().1.starts_with("/api/csrf-token"));
        assert!(seen.try_recv().is_err(), "登录请求不应继续发送");
    }

    /// 前置请求的地址/取值方式在保存（validate）阶段就要拦：空地址、非法取值前缀
    #[test]
    fn pre_request_validation_rejects_empty_url_and_unknown_extract_prefix() {
        let mut req = request("http://portal.example/login".into());
        req.pre_request = Some(HttpPreRequest {
            url: String::new(),
            extract: "json:csrf_token".into(),
            ..Default::default()
        });
        assert!(
            req.validate().unwrap_err().contains("前置请求缺少请求地址"),
            "{:?}",
            req.validate().unwrap_err()
        );

        req.pre_request = Some(HttpPreRequest {
            url: "http://portal.example/token".into(),
            extract: "regex:name=\"(.*?)\"".into(),
            ..Default::default()
        });
        assert!(
            req.validate()
                .unwrap_err()
                .contains("只支持 `json:字段路径`"),
            "{:?}",
            req.validate().unwrap_err()
        );

        req.pre_request = Some(HttpPreRequest {
            url: "ftp://portal.example/token".into(),
            extract: "json:csrf_token".into(),
            ..Default::default()
        });
        assert!(req.validate().is_err());
    }

    /// 用得到本机 IP 就必须查一次：脚本要读 `ctx.local_ip`，或模板里直接写了 `{local_ip}`
    /// （锐捷 ePortal 这类门户把本机 IP 当必填参数；少查一次 = 静默发出空 IP）
    #[test]
    fn needs_local_address_covers_script_and_placeholders() {
        let mut req = request("http://portal.example/login".into());
        assert!(
            !req.needs_local_address(),
            "既无脚本也无占位符时不该白跑一次网卡探测"
        );

        req.url = "http://portal.example/login?ip={local_ip}".into();
        assert!(req.needs_local_address(), "地址里的占位符要算进去");

        req.url = "http://portal.example/login".into();
        req.body = "mac={local_mac}".into();
        assert!(req.needs_local_address(), "请求体里的占位符要算进去");

        req.body.clear();
        req.crypto_script = "function transform(ctx) { return { ip: ctx.local_ip }; }".into();
        assert!(req.needs_local_address(), "脚本要读 ctx.local_ip");

        req.crypto_script.clear();
        req.pre_request = Some(HttpPreRequest {
            url: "http://portal.example/t?ip={local_ip}".into(),
            extract: "json:token".into(),
            ..Default::default()
        });
        assert!(req.needs_local_address(), "前置请求里的占位符要算进去");
    }

    /// 取值：容忍 JSONP 包裹与前后脏字符（这类接口由门户前端 AJAX 调用）
    #[test]
    fn pre_request_extract_tolerates_jsonp_and_dirty_body() {
        assert_eq!(
            extract_pre_value("csrf_token", "dr1003({\"csrf_token\":\"t1\"})").unwrap(),
            "t1"
        );
        assert_eq!(
            extract_pre_value("data.token", "\n {\"data\":{\"token\":\"t2\"}} \n").unwrap(),
            "t2"
        );
        // 非字符串值取其 JSON 文本（数字令牌也照用）
        assert_eq!(extract_pre_value("code", "{\"code\":0}").unwrap(), "0");
        assert!(extract_pre_value("missing", "{\"code\":0}").is_err());
    }

    #[test]
    fn flow_extraction_reads_header_url_redirect_html_and_regex() {
        let url = url::Url::parse("http://portal.example/entry?session=a%2Bb").unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-session", "header-token".parse().unwrap());
        headers.insert(
            reqwest::header::LOCATION,
            "/next?challenge=redirect-token".parse().unwrap(),
        );
        let body = r#"<INPUT value='a&amp;b' type='hidden' name='sign'><p>code: 1234</p>"#;
        for (source, expected) in [
            ("header:x-session", "header-token"),
            ("url:session", "a+b"),
            ("redirect:challenge", "redirect-token"),
            ("html:sign", "a&b"),
            ("regex:code: ([0-9]+)", "1234"),
        ] {
            let rule = HttpExtractRule {
                source: source.into(),
                name: "value".into(),
            };
            rule.validate().unwrap();
            assert_eq!(
                extract_flow_value(&rule, body, &headers, &url).unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn flow_uses_two_redirect_fields_in_later_request() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route(
                "/entry",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::FOUND,
                        [(
                            axum::http::header::LOCATION,
                            "/login?token=t-123&account=alice",
                        )],
                        "",
                    )
                }),
            )
            .route(
                "/login",
                axum::routing::get(|url: axum::extract::OriginalUri| async move {
                    if url.0.query().unwrap_or_default() == "token=t-123&account=alice" {
                        "ok"
                    } else {
                        "bad"
                    }
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let mut req = request(String::new());
        req.success_pattern = "ok".into();
        req.result_step_id = "login".into();
        req.steps = vec![
            HttpFlowStep {
                id: "entry".into(),
                url: format!("{base}/entry"),
                stop_on_redirect: true,
                extracts: vec![
                    HttpExtractRule {
                        source: "redirect:token".into(),
                        name: "token".into(),
                    },
                    HttpExtractRule {
                        source: "redirect:account".into(),
                        name: "account".into(),
                    },
                ],
                ..Default::default()
            },
            HttpFlowStep {
                id: "login".into(),
                url: format!("{base}/login?token={{token}}&account={{account}}"),
                ..Default::default()
            },
        ];
        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);
        assert_eq!(report.step_reports[0].produced_vars, ["token", "account"]);
        assert!(
            !serde_json::to_string(&report.step_reports)
                .unwrap()
                .contains("t-123")
        );
    }

    // ===== 退出登录动作（登录前踢掉旧会话） =====

    /// 起一个对任何请求都回 200「登录成功」、并按序记录 `(来源端口, 路径)` 的极简门户。
    ///
    /// 供下线动作的顺序与容错断言使用：响应体恒为成功关键字，下线请求回什么都
    /// 不影响登录判定；连接不写 `Connection: close`，同一条 keep-alive 上的多个
    /// 请求保持同端口——这正是要断言的性质。
    async fn spawn_recording_portal() -> (String, tokio::sync::mpsc::Receiver<(u16, String)>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    break;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.into_split();
                    let mut buf: Vec<u8> = Vec::new();
                    loop {
                        // 读到"请求头 + Content-Length 指定的体"齐全为止（与 CSRF 门户同构）
                        let (head_end, content_length) = loop {
                            if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                                let len = head
                                    .lines()
                                    .find_map(|line| {
                                        let (k, v) = line.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse::<usize>().ok())?
                                    })
                                    .unwrap_or(0);
                                if buf.len() >= head_end + 4 + len {
                                    break (head_end, len);
                                }
                            }
                            let mut chunk = [0_u8; 4096];
                            match reader.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };

                        let request =
                            String::from_utf8_lossy(&buf[..head_end + 4 + content_length])
                                .to_string();
                        buf.drain(..head_end + 4 + content_length);
                        let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                        let _ = tx.send((peer.port(), path.clone())).await;

                        let body = "登录成功";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if writer.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        (format!("http://{addr}"), rx)
    }

    /// 下线请求必须**先于**登录请求发出，且模板占位符被渲染。
    ///
    /// "IP 已在线拒绝重复登录"的门户依赖这个顺序：旧会话还在时先发的登录请求
    /// 会被拒，先踢后登才有意义。
    #[tokio::test]
    async fn logout_request_is_sent_before_login_request() {
        let (base, mut seen) = spawn_recording_portal().await;
        let mut req = request(format!("{base}/api/login"));
        req.method = HttpRequestMethod::Post;
        req.body = "username={username}&password={password}".into();
        req.success_pattern = "登录成功".into();
        req.failure_pattern.clear();
        req.logout_request = Some(HttpActionRequest {
            method: HttpRequestMethod::Post,
            url: format!("{base}/api/logout?u={{username}}"),
            body: "bye={username}".into(),
            ..Default::default()
        });

        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);

        let (first_port, first_path) = seen.recv().await.unwrap();
        let (second_port, second_path) = seen.recv().await.unwrap();
        assert!(
            first_path.starts_with("/api/logout"),
            "下线请求必须先到（实际先到 {first_path:?}）"
        );
        assert!(
            second_path.starts_with("/api/login"),
            "登录请求必须后到（实际后到 {second_path:?}）"
        );
        assert_eq!(
            first_port, second_port,
            "下线与登录应落在同一条连接（部分门户要求会话关联）"
        );
        // 占位符渲染：下线请求里的 {username} 已被替换
        assert!(
            first_path.contains("u=abc"),
            "下线地址未渲染占位符: {first_path}"
        );
        assert!(report.rendered_url.contains("/api/login"));
    }

    /// 下线必须排在**登录页抓取**之前。
    ///
    /// 这类门户在旧会话仍在线时，登录页会被重定向到"已在线"页，脚本据此产出的字段
    /// 全是错的——本功能要治的正是这类门户，所以"先清场"必须也覆盖取令牌这一步
    /// （此前只排在前置请求之前，登录页却已经先抓完了）。
    #[tokio::test]
    async fn logout_request_precedes_login_page_fetch() {
        let (base, mut seen) = spawn_recording_portal().await;
        let mut req = request(format!("{base}/api/login"));
        req.method = HttpRequestMethod::Post;
        req.body = "username={username}&password={password}".into();
        req.success_pattern = "登录成功".into();
        req.failure_pattern.clear();
        // 打开抓页开关并带一段脚本：抓登录页这一步才会真的发请求
        req.fetch_page = true;
        req.auth_url = format!("{base}/api/login-page");
        req.crypto_script = "function transform(ctx) { return { token: \"t\" }; }".into();
        req.logout_request = Some(HttpActionRequest {
            method: HttpRequestMethod::Post,
            url: format!("{base}/api/logout"),
            ..Default::default()
        });

        let report = run_once(&req).await;
        assert_eq!(report.outcome, Outcome::Success, "{}", report.message);

        let (_, first_path) = seen.recv().await.unwrap();
        assert!(
            first_path.starts_with("/api/logout"),
            "下线请求必须排在登录页抓取之前（实际先到 {first_path:?}）"
        );
    }

    /// 下线请求失败**不得**影响登录：动作语义是"触达即可"，门户没开下线接口、
    /// 地址写错都只留一条 warn 日志，登录照常进行并按自己的判定走。
    #[tokio::test]
    async fn logout_request_failure_does_not_block_login() {
        let (base, mut seen) = spawn_recording_portal().await;
        let mut req = request(format!("{base}/api/login"));
        req.method = HttpRequestMethod::Post;
        req.body = "username={username}&password={password}".into();
        req.success_pattern = "登录成功".into();
        req.failure_pattern.clear();
        req.logout_request = Some(HttpActionRequest {
            // 指向未监听端口：请求必然网络失败
            url: "http://127.0.0.1:1/logout".into(),
            ..Default::default()
        });

        let report = run_once(&req).await;
        assert_eq!(
            report.outcome,
            Outcome::Success,
            "下线失败不应拦住登录: {}",
            report.message
        );
        // 登录请求确实发出并成功（下线请求打到了别人的端口，不算门户收到的请求）
        let login_call = seen.recv().await.unwrap();
        assert!(login_call.1.starts_with("/api/login"));
    }

    /// from_task 必须把任务的 logout_request 带到执行参数上（from_task 映射完整性）
    #[test]
    fn from_task_carries_logout_request() {
        let task = HttpTaskConfig {
            url: "http://10.0.0.1/login".into(),
            logout_request: Some(HttpActionRequest {
                url: "http://10.0.0.1/logout".into(),
                wait_secs: 1.0,
                ..Default::default()
            }),
            ..HttpTaskConfig::default()
        };
        let req = HttpLoginRequest::from_task(&task, "u", "p", "", "", false, true).unwrap();
        let logout = req.logout_request.expect("logout_request 必须被映射");
        assert_eq!(logout.url, "http://10.0.0.1/logout");
        assert!((logout.wait_secs - 1.0).abs() < f64::EPSILON);

        // 默认任务不带下线：老配置照旧
        let plain = HttpTaskConfig {
            url: "http://10.0.0.1/login".into(),
            ..HttpTaskConfig::default()
        };
        assert!(
            HttpLoginRequest::from_task(&plain, "u", "p", "", "", false, true)
                .unwrap()
                .logout_request
                .is_none()
        );
    }

    /// 保存/执行两侧的体积校验同口径：超限下线配置必须在 validate 阶段被拒
    #[test]
    fn validate_logout_request_rejects_oversized_fields() {
        let long_body = "a".repeat(MAX_REQUEST_BODY_BYTES + 1);
        let err = HttpLoginRequest::validate_logout_request(&HttpActionRequest {
            url: "http://10.0.0.1/logout".into(),
            body: long_body,
            ..Default::default()
        })
        .expect_err("超限下线请求体必须被拒");
        assert!(err.contains("退出登录请求体过长"), "{err}");

        // 执行参数级 validate 也要走到下线分支
        let mut req = request("http://portal.example/login".into());
        req.logout_request = Some(HttpActionRequest {
            url: "http://portal.example/logout".into(),
            wait_secs: 1.0,
            ..Default::default()
        });
        assert!(req.validate().is_ok());
        req.logout_request.as_mut().unwrap().url = String::new();
        assert!(req.validate().is_err(), "空地址的下线配置必须被拒");
    }
}
