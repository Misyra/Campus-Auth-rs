//! 任务数据模型：TaskKind / TaskConfig / HttpTaskConfig / StepConfig 等
//!
//! 定义浏览器/脚本/直连（http）三类任务的 serde 数据模型。`TaskKind` 为内部
//! 标记枚举，`type` 字段缺失或为空时默认归为浏览器任务（旧版 JSON 兼容），
//! 存在但未知时反序列化报错（与保存路径的校验一致）。步骤配置做
//! `code`→`script` 与 `frame` 类型规范化。

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// task_id 校验正则
pub const TASK_ID_PATTERN: &str = r"^[a-zA-Z0-9_-]{1,64}$";
/// 浏览器任务默认超时（毫秒）
pub const DEFAULT_TASK_TIMEOUT_MS: u64 = 30000;
/// 浏览器任务超时下限（毫秒）：低于此值任务必然秒超时，无意义
pub const MIN_TASK_TIMEOUT_MS: u64 = 1000;
/// 浏览器任务超时上限（毫秒）：浏览器会话槽位全局互斥，超长超时会
/// 占住槽位导致期间所有浏览器任务/登录被拒
pub const MAX_TASK_TIMEOUT_MS: u64 = 600_000;
/// 步骤间延迟（秒）
pub const DEFAULT_STEP_DELAY: f64 = 0.5;
/// 页面加载后等待（秒）
pub const DEFAULT_NAVIGATION_WAIT: f64 = 1.0;
/// 步骤默认超时（毫秒）
pub const DEFAULT_STEP_TIMEOUT_MS: u64 = 10000;
/// 脚本默认超时（秒）
pub const DEFAULT_SCRIPT_TIMEOUT: u64 = 60;
/// 脚本超时下限（秒）
pub const MIN_SCRIPT_TIMEOUT: u64 = 1;
/// 脚本超时上限（秒）
pub const MAX_SCRIPT_TIMEOUT: u64 = 3600;
/// 脚本内容大小上限（字节）
pub const MAX_SCRIPT_CONTENT_SIZE: usize = 100 * 1024;
/// http 直连任务凭据变换脚本大小上限（字节）
///
/// 与 `src/login/http_login.rs` 的 `MAX_SCRIPT_BYTES` 同口径：任务的
/// `crypto_script` 与方案里的 `http_crypto_script` 最终由同一套脚本引擎执行，
/// 任务侧若放行更大体积，会出现「任务保存成功、登录必然失败」的错位，故此处
/// 用与登录侧完全相同的上限，让超限在保存时就被拒绝。
pub const MAX_HTTP_SCRIPT_BYTES: usize = 128 * 1024;
/// http 直连任务请求地址大小上限（字节，防呆）
///
/// 与 `src/login/http_login.rs` 的 `MAX_URL_BYTES` 同口径：超限的地址在登录侧会被
/// 拒绝，保存侧若放行就又是"存得下、登不上"，故两处用同一个数。
pub const MAX_HTTP_URL_BYTES: usize = 8 * 1024;
/// http 直连任务请求头大小上限（字节，防呆）
///
/// 请求头为纯文本、每行一项，正常门户只有几百字节；上限只为拦住误粘贴的大段
/// 内容（撑大任务文件、拖慢请求构造），不表达任何安全边界。
///
/// 与 `src/login/http_login.rs` 的 `MAX_HEADERS_BYTES` 同口径（**必须是同一个数**）：
/// 那边是 64 KiB，而这里原先是 256 KiB —— 于是 64~256 KiB 的请求头"存得下、必然登不上"
/// （登录侧 `from_task → validate` 会在每次登录开始前直接判失败），
/// `docs/guides/http-login-guide.md` 也明写"请求头 64 KiB、超过上限时保存会被拒绝"。
/// 两处一致性由 `http_login.rs` 的单测钉住（`login_and_task_size_limits_agree`）。
pub const MAX_HTTP_HEADERS_BYTES: usize = 64 * 1024;
/// http 直连任务请求体大小上限（字节，防呆）
///
/// 与请求头同理：POST 表单体通常几十字节，上限只拦误粘贴。
pub const MAX_HTTP_BODY_BYTES: usize = 256 * 1024;
/// stdout/stderr 截断长度
pub const OUTPUT_TRUNCATE_LEN: usize = 500;
/// 有效步骤类型集合
pub const VALID_STEP_TYPES: &[&str] = &[
    "input",
    "click",
    "select",
    "click_select",
    "wait",
    "wait_url",
    "eval",
    "screenshot",
    "sleep",
    "ocr",
    "custom_js",
    // Python Worker 侧的执行别名（step_handlers._STEP_HANDLERS 同名注册），
    // 校验必须与执行器对齐，否则录制/AI 产物保存时被误拒
    "evaluate",
    "custom",
    "navigate",
    "goto",
    "assert_text",
    "upload_file",
    "wait_for_selector",
];

fn default_task_timeout_ms() -> u64 {
    DEFAULT_TASK_TIMEOUT_MS
}
fn default_step_delay() -> f64 {
    DEFAULT_STEP_DELAY
}
fn default_navigation_wait() -> f64 {
    DEFAULT_NAVIGATION_WAIT
}
fn default_script_timeout() -> u64 {
    DEFAULT_SCRIPT_TIMEOUT
}
fn default_value_obj() -> Value {
    Value::Object(Default::default())
}

/// 三类任务共享的字段
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonFields {
    /// 任务唯一标识（从文件名推导，JSON 中可省略）
    pub task_id: String,
    /// 显示名称
    pub name: String,
    /// 任务描述
    pub description: String,
}

impl Default for CommonFields {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            name: "未命名任务".to_string(),
            description: String::new(),
        }
    }
}

/// 浏览器任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// 认证页面 URL（支持 `{{LOGIN_URL}}` 模板）
    pub url: String,
    /// 任务超时（毫秒）
    #[serde(default = "default_task_timeout_ms")]
    pub timeout: u64,
    /// 步骤间延迟（秒）
    #[serde(default = "default_step_delay")]
    pub step_delay: f64,
    /// 页面加载后等待秒数
    #[serde(default = "default_navigation_wait")]
    pub navigation_wait: f64,
    /// 是否揭示隐藏输入框
    pub reveal_hidden: bool,
    /// 自定义模板变量
    pub variables: HashMap<String, String>,
    /// 步骤列表
    pub steps: Vec<StepConfig>,
    /// 成功判定变量名（eval 步骤 store_as 写入，非空时登录成功以该变量真值判定）
    pub success_condition: String,
    /// 成功回调配置
    pub on_success: Value,
    /// 失败回调配置
    pub on_failure: Value,
    /// 用户自定义元数据（执行器不使用）
    pub metadata: Value,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            url: String::new(),
            timeout: DEFAULT_TASK_TIMEOUT_MS,
            step_delay: DEFAULT_STEP_DELAY,
            navigation_wait: DEFAULT_NAVIGATION_WAIT,
            reveal_hidden: false,
            variables: HashMap::new(),
            steps: Vec::new(),
            success_condition: String::new(),
            on_success: default_value_obj(),
            on_failure: default_value_obj(),
            metadata: default_value_obj(),
        }
    }
}

/// 脚本任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptTaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// 脚本路径（相对 tasks/scripts/ 或绝对路径）
    pub script_path: Option<String>,
    /// 内联脚本内容（写入临时文件执行）
    pub content: Option<String>,
    /// 命令行参数
    pub args: Vec<String>,
    /// 工作目录，为空时用 script_path 所在目录
    pub work_dir: Option<String>,
    /// 超时秒数，钳制到 [1, 3600]
    #[serde(default = "default_script_timeout")]
    pub timeout: u64,
    /// 执行二进制路径，为空则自动检测
    pub binary_path: Option<String>,
}

impl Default for ScriptTaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            script_path: None,
            content: None,
            args: Vec::new(),
            work_dir: None,
            timeout: DEFAULT_SCRIPT_TIMEOUT,
            binary_path: None,
        }
    }
}

/// http 直连任务的请求方法
///
/// 序列化取大写形式（`"GET"` / `"POST"`），与 HTTP 报文里的方法名逐字一致，
/// 避免任务 JSON 里出现 `"get"` 这类需要在每个消费方再规范化一次的写法。
/// 默认 GET：直连门户最常见形态是带查询串的 GET，POST 需显式写 method。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpRequestMethod {
    /// GET 请求
    #[default]
    Get,
    /// POST 请求
    Post,
}

impl HttpRequestMethod {
    /// 方法名的线上表示（`"GET"` / `"POST"`）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

impl std::fmt::Display for HttpRequestMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 直连登录的前置请求（可选）：先发一次请求、取出一个值，再发登录请求。
///
/// 使用场景与连接复用口径见 [`HttpTaskConfig`] 的「前置请求」小节。典型配置
/// （河南科技大学「大学掌」体系这类 CSRF 绑连接的门户）：
///
/// ```json
/// "pre_request": {
///   "method": "GET",
///   "url": "http://10.100.51.1/api/csrf-token",
///   "extract": "json:csrf_token",
///   "name": "csrf"
/// }
/// ```
///
/// 随后登录请求的请求头里写 `X-CSRF-Token: {csrf}` 即可。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct HttpPreRequest {
    /// 请求方法
    pub method: HttpRequestMethod,
    /// 请求地址（支持与登录请求同一套占位符）
    pub url: String,
    /// 请求头，每行一项 `名称: 值`
    pub headers: String,
    /// 请求体（POST 使用）
    pub body: String,
    /// 取值方式：`json:<字段>[.<字段>...]`（按点号路径取 JSON 值，字符串取原值、
    /// 其余类型取其 JSON 文本）。
    ///
    /// 只支持 JSON 路径而不支持正则：令牌在 HTML 里的门户用「登录页原文 + 凭据变换
    /// 脚本」表达更合适（脚本的 `ctx.page` 就是登录页原文），不必再引入正则依赖。
    pub extract: String,
    /// 取到的值注册成哪个占位符（留空时取 `extract` 路径的最后一段）
    pub name: String,
}

impl HttpPreRequest {
    /// 取值方式的 JSON 点号路径（`json:data.csrf_token` → `data.csrf_token`）。
    ///
    /// 写在 tasks 层而不是执行层：保存路径（`TaskManager::validate_task`，只拿到
    /// JSON 值）与执行路径（`login::http_login`）都要判同一件事——用户写错取值方式是
    /// "保存通过、登录必然失败"的错位配置，两处口径必须同源，故解析放这里共用。
    pub fn extract_path(&self) -> Result<&str, String> {
        let trimmed = self.extract.trim();
        if trimmed.is_empty() {
            return Err("前置请求缺少取值方式（例如 `json:csrf_token`）".to_string());
        }
        match trimmed.strip_prefix("json:") {
            Some(path) if !path.trim().is_empty() => Ok(path.trim()),
            Some(_) => Err("前置请求的取值方式缺少字段名（例如 `json:csrf_token`）".to_string()),
            None => Err(format!(
                "前置请求的取值方式只支持 `json:字段路径`（当前为 `{trimmed}`）"
            )),
        }
    }

    /// 取到的值注册成哪个占位符：显式 `name` 优先，留空取路径最后一段
    pub fn placeholder_name(&self) -> String {
        let explicit = self.name.trim();
        if !explicit.is_empty() {
            return explicit.to_string();
        }
        self.extract_path()
            .ok()
            .and_then(|path| path.rsplit('.').next())
            .unwrap_or("pre")
            .to_string()
    }

    /// 结构校验：请求地址必填且为 http/https、取值方式可解析。
    ///
    /// 只管形状，不管体积（体积上限由保存路径与执行层各自的常量把关，与
    /// `auth_url` 的分工一致）。空地址的前置请求等于"每次登录必然失败"的配置，
    /// 必须在保存/测试闸口拦下，而不是等登录失败才暴露。
    pub fn validate(&self) -> Result<(), String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err("前置请求缺少请求地址（不需要前置请求就把整块留空）".to_string());
        }
        let (scheme, rest) = url.split_once("://").unwrap_or(("", ""));
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
            return Err("前置请求的地址仅支持 http/https".to_string());
        }
        if host.is_empty() {
            return Err("前置请求的地址缺少主机名".to_string());
        }
        self.extract_path()?;
        Ok(())
    }
}

/// 直连任务的动作请求（目前用于退出登录）：一次"发了不判成败"的附加请求。
///
/// 与 [`HttpPreRequest`] 的分工：前置请求**取值**（取不到即登录流程终态失败），
/// 动作请求**触达**（门户收没收到都照常走主流程）——退出登录正是这一类：强制下线
/// 通常只为把「IP 已在线，拒绝重复登录」的旧会话踢掉，门户没开这个接口、或请求
/// 形状不对时，登录本身仍然可能成功，把整次登录判死反而更糟。
///
/// 支持的占位符与登录请求同一套（`{username}` / `{password}` / 脚本产出字段等），
/// 由执行器在与登录请求**同一条 keep-alive 连接**上发出（部分门户的下线接口
/// 同样要求会话关联）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct HttpActionRequest {
    /// 请求方法
    pub method: HttpRequestMethod,
    /// 请求地址（支持与登录请求同一套占位符）
    pub url: String,
    /// 请求头，每行一项 `名称: 值`
    pub headers: String,
    /// 请求体（POST 使用）
    pub body: String,
    /// 请求发出后等待秒数（0 = 不等待）。
    ///
    /// 典型用途：门户下线是异步生效的（立即重连会被旧会话占住的 IP 拒绝），
    /// 等 1~3 秒再发登录请求能显著提高一次成功率。上限由执行侧钳制。
    pub wait_secs: f64,
}

impl HttpActionRequest {
    /// 下线后等待秒数的钳制上限：等待的本质是拖慢登录节奏，超过它说明配置
    /// 写错了用途（比如把轮询间隔写进来），钳到 30 秒防止登录窗口被拖爆。
    pub const MAX_WAIT_SECS: f64 = 30.0;

    /// 形状校验：请求地址必填且为 http/https、等待秒数在 [0, 30]。
    ///
    /// 与 [`HttpPreRequest::validate`] 同口径地放在 tasks 层：保存路径（只拿到
    /// JSON 值）与执行路径共用同一份判据，避免"存得下、必然错位"的配置。
    pub fn validate(&self) -> Result<(), String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err("退出登录请求缺少请求地址（不需要就把整块留空）".to_string());
        }
        let (scheme, rest) = url.split_once("://").unwrap_or(("", ""));
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
            return Err("退出登录请求的地址仅支持 http/https".to_string());
        }
        if host.is_empty() {
            return Err("退出登录请求的地址缺少主机名".to_string());
        }
        if !self.wait_secs.is_finite() || self.wait_secs < 0.0 {
            return Err("退出登录请求的等待秒数需 ≥ 0".to_string());
        }
        if self.wait_secs > Self::MAX_WAIT_SECS {
            return Err(format!(
                "退出登录请求的等待秒数最大 {}（当前 {}）",
                Self::MAX_WAIT_SECS,
                self.wait_secs
            ));
        }
        Ok(())
    }
}

/// http 直连任务配置（把「直连请求」的登录参数固化为可复用的具名任务）
///
/// 只装请求本身（方法/地址/认证页/头/体/成败判定/凭据变换），**不装凭据**：账号
/// 密码永远来自方案（`ProfileData`），任务只提供可被多个方案共享的请求模板，避免
/// 凭据在任务文件里出现第二份副本、也避免改密码要逐个改任务。
/// 认证页地址（[`Self::auth_url`]）是本层唯一的"共享式"字段：它本就两渠道共用，
/// 任务填了只作回退链的优先项，方案里的同名字段本轮不动。
///
/// # 前置请求
///
/// 部分门户的登录需要一个先从**别的接口**取回的令牌（典型是 CSRF token），且该令牌
/// **绑定 TCP 连接**——取 token 与发登录必须在同一条 keep-alive 连接上，换连接就报
/// `CSRF token mismatch`。单请求模型表达不了这种门户，凭据变换脚本又跑在无网络沙箱里，
/// 故在任务里加一层可选的前置请求（见 [`HttpPreRequest`]）：它先发一次请求、从响应里
/// 取出一个值注册成占位符，再渲染并发出登录请求。两次请求由同一个 `reqwest::Client`
/// 顺序发出，连接池按 `(scheme, host, port)` 复用，token 绑连接的门户因此才能成功。
///
/// # 退出登录请求
///
/// 与前置请求互补的另一层：[`logout_request`](HttpTaskConfig::logout_request) 在登录前
/// 先发一次**下线请求**（见 [`HttpActionRequest`]），治「IP 已在线，拒绝重复登录」类
/// 门户的重复登录失败。动作只求触达：下线请求无论成败都继续登录，不判终态。
///
/// 它在整个流程里排在**最前**（先于抓登录页与凭据变换脚本）：旧会话还在线时，这类
/// 门户的登录页会被重定向到"已在线"页，脚本据此产出的字段全是错的——先清场再取令牌
/// 才是正确的因果链。代价是下线请求只支持内置占位符（脚本还没跑）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpTaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// 请求方法
    pub method: HttpRequestMethod,
    /// 直连请求地址（支持 `{username}` 等占位符替换）
    pub url: String,
    /// 认证页地址（门户登录页；直连请求里作为脚本 `ctx.auth_url` 与「抓取登录页原文」的来源）
    ///
    /// 注意它**不是**登录请求地址——登录请求地址是 [`Self::url`]。留空时运行时回退用
    /// 方案的 `ProfileData::auth_url`（该字段浏览器/直连两个渠道共用，本轮不动），
    /// 所以老配置不填也照旧工作；填了则本任务自带认证页地址。导入他人分享的任务时
    /// 通常需要它，否则会在使用者的方案上取到与自己门户不匹配的地址。
    pub auth_url: String,
    /// 请求头，每行一项 `名称: 值`
    pub headers: String,
    /// 请求体（POST 使用，同样支持占位符）
    pub body: String,
    /// 成功判定模式（响应体匹配）
    pub success_pattern: String,
    /// 失败判定模式（响应体匹配）
    pub failure_pattern: String,
    /// 凭据变换脚本（生成加密后的表单字段等）
    pub crypto_script: String,
    /// 前置请求：登录前先取回一个值（如 CSRF token）供登录请求引用；`None` = 不需要
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_request: Option<HttpPreRequest>,
    /// 退出登录请求（可选）：登录前先发一次下线请求；`None` = 不需要
    ///
    /// 见 [`HttpActionRequest`]：动作请求只求触达不求结果——门户没配下线接口时
    /// 登录仍照常进行。写在这里而不是方案：下线地址是门户属性，与登录地址一样
    /// 属于"同一门户多账号共用"的任务配置。
    ///
    /// 请求地址里只能用**内置占位符**（`{username}` / `{password}` / `{auth_url}` /
    /// `{local_ip}` / `{local_mac}`）：它排在整个流程最前，脚本产出的字段此时还不存在。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logout_request: Option<HttpActionRequest>,
    /// 是否忽略 HTTPS 证书错误；`None` = 跟随全局 `browser.ignore_https_errors`
    pub ignore_https_errors: Option<bool>,
    /// 用户自定义元数据（执行器不使用，供仓库来源等标注）
    pub metadata: Value,
}

impl Default for HttpTaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            method: HttpRequestMethod::default(),
            url: String::new(),
            auth_url: String::new(),
            headers: String::new(),
            body: String::new(),
            success_pattern: String::new(),
            failure_pattern: String::new(),
            crypto_script: String::new(),
            pre_request: None,
            logout_request: None,
            ignore_https_errors: None,
            metadata: default_value_obj(),
        }
    }
}

/// 统一任务类型（内部标记枚举）
///
/// `type` 字段缺失、为空或 = "browser" 时归为浏览器任务；"script" 归为脚本任务；
/// "http" 归为直连任务；历史 `type=shell` 已移除，遇到时明确报错并提示改用脚本
/// 任务；其余未知值在反序列化时报错（与保存路径 `validate_task` 拒绝未知类型
/// 的行为一致，避免拼错类型名时被静默当作浏览器任务执行）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    /// 浏览器任务
    Browser(TaskConfig),
    /// 脚本任务
    Script(ScriptTaskConfig),
    /// http 直连任务
    Http(HttpTaskConfig),
}

impl TaskKind {
    /// 借用三类任务共享的 [`CommonFields`]（收敛三臂 match 样板）
    pub fn common(&self) -> &CommonFields {
        match self {
            TaskKind::Browser(c) => &c.common,
            TaskKind::Script(c) => &c.common,
            TaskKind::Http(c) => &c.common,
        }
    }

    /// 可变借用共享字段（写回 task_id / name 等）
    pub fn common_mut(&mut self) -> &mut CommonFields {
        match self {
            TaskKind::Browser(c) => &mut c.common,
            TaskKind::Script(c) => &mut c.common,
            TaskKind::Http(c) => &mut c.common,
        }
    }

    /// 任务类型名（`"browser"` / `"script"` / `"http"`，与 serde tag 取值一致）
    pub fn type_name(&self) -> &'static str {
        match self {
            TaskKind::Browser(_) => "browser",
            TaskKind::Script(_) => "script",
            TaskKind::Http(_) => "http",
        }
    }

    /// 列表摘要用的「任务地址」：浏览器任务=登录页地址，直连任务=请求地址，脚本任务=空。
    ///
    /// 收敛在这里而不是各调用点自己 match：任务列表的每一行都要显示它，摘要与详情
    /// 两条构造路径（`summary_from_value` / `get_task_detail`）必须给出同一个答案。
    pub fn summary_url(&self) -> &str {
        match self {
            TaskKind::Browser(c) => &c.url,
            TaskKind::Script(_) => "",
            TaskKind::Http(c) => &c.url,
        }
    }

    /// 直连任务的请求方法；非直连任务为 `None`（前端据此决定要不要渲染方法标签）
    pub fn http_request_method(&self) -> Option<HttpRequestMethod> {
        match self {
            TaskKind::Http(c) => Some(c.method),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for TaskKind {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(d)?;
        // 区分「type 缺失/为空 → 默认 Browser」与「type 存在但未知 → 报错」：
        // 前者是旧版 JSON（无 type 字段）的向后兼容，后者若静默回退为浏览器任务
        // 会与保存路径 loader::validate_task 拒绝未知类型的行为不一致（G6）
        let kind = match value.get("type") {
            None => "browser",
            Some(v) if v.as_str() == Some("") => "browser",
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    return Err(serde::de::Error::custom(format!(
                        "type 字段必须为字符串: {v}"
                    )));
                }
            },
        };
        match kind {
            "browser" => Ok(TaskKind::Browser(
                parse::<TaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
            "script" => Ok(TaskKind::Script(
                parse::<ScriptTaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
            "http" => Ok(TaskKind::Http(
                parse::<HttpTaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
            // Shell 任务已移除：历史存量 type=shell 明确报错，提示改用脚本任务
            "shell" => Err(serde::de::Error::custom(
                "任务类型 shell 已移除，请改用 script 类型（.sh/.bat/.py/.exe）",
            )),
            other => Err(serde::de::Error::custom(format!(
                "未知任务类型: {other}（有效值: browser / script / http）"
            ))),
        }
    }
}

/// 从包含 `type` 字段的 JSON 值反序列化具体任务配置
fn parse<T: DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(value)
}

/// 步骤配置（扁平 struct，通过 `type` 字段区分步骤类型）
///
/// Rust 侧仅存储与传递，具体解释执行由 Python Worker 完成。`code`→`script` 规范化与
/// `frame` 类型规范化在反序列化时完成，未知字段收集到 `extra`。
#[derive(Debug, Clone, Serialize)]
pub struct StepConfig {
    /// 步骤标识符
    pub id: String,
    /// 步骤类型
    #[serde(rename = "type")]
    pub step_type: String,
    /// 步骤描述
    pub description: String,
    /// 单步超时（ms），覆盖任务级默认值
    pub timeout: Option<u64>,
    /// CSS/文本选择器
    pub selector: Option<String>,
    /// 填写值
    pub value: Option<String>,
    /// URL 匹配正则（wait_url）
    pub pattern: Option<String>,
    /// JavaScript 代码（eval）；`code` 历史别名会规范化为此字段
    pub script: Option<String>,
    /// 结果存储变量名（eval/ocr）
    pub store_as: Option<String>,
    /// 是否清空输入框再填写（input）
    pub clear: bool,
    /// 截图路径（screenshot）
    pub path: Option<String>,
    /// 延时毫秒（sleep）
    pub duration: u64,
    /// iframe 选择器（非字符串值静默忽略）
    pub frame: Option<String>,
    /// 是否必须成功
    pub required: bool,
    /// 选项容器选择器（click_select）
    pub option_selector: Option<String>,
    /// 验证码输入框选择器（ocr）
    pub target_selector: Option<String>,
    /// 是否使用旧版 OCR 模型（ocr）
    pub old: bool,
    /// OCR 字符范围（string 或 int）
    pub char_range: Option<Value>,
    /// 未知字段收集
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(default)]
struct StepHelper {
    id: String,
    #[serde(rename = "type")]
    step_type: String,
    description: String,
    timeout: Option<u64>,
    selector: Option<String>,
    value: Option<String>,
    pattern: Option<String>,
    script: Option<String>,
    code: Option<String>,
    store_as: Option<String>,
    clear: bool,
    path: Option<String>,
    duration: u64,
    frame: Value,
    required: bool,
    option_selector: Option<String>,
    target_selector: Option<String>,
    old: bool,
    char_range: Option<Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

impl Default for StepHelper {
    fn default() -> Self {
        Self {
            id: String::new(),
            step_type: String::new(),
            description: String::new(),
            timeout: None,
            selector: None,
            value: None,
            pattern: None,
            script: None,
            code: None,
            store_as: None,
            clear: true,
            path: None,
            duration: 1000,
            frame: Value::Null,
            // 与 Python 侧 models.py 的步骤默认值契约对齐（B5）：required 默认 true，
            // 登录步骤失败不应被当作可选而静默吞掉（前端编辑器不写 required 字段，
            // 若默认 false 会全部落入"假成功"陷阱）。clear 保持 true、duration 保持
            // 1000ms，与 Python 侧的最终对齐由双方各自确认。
            required: true,
            option_selector: None,
            target_selector: None,
            old: false,
            char_range: None,
            extra: HashMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for StepConfig {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let h = StepHelper::deserialize(d)?;
        // frame 非字符串（如布尔 true）静默设为 None
        let frame = match h.frame {
            Value::String(s) => Some(s),
            _ => None,
        };
        // code 历史别名规范化为 script
        let script = h.script.or(h.code);
        Ok(StepConfig {
            id: h.id,
            step_type: h.step_type,
            description: h.description,
            timeout: h.timeout,
            selector: h.selector,
            value: h.value,
            pattern: h.pattern,
            script,
            store_as: h.store_as,
            clear: h.clear,
            path: h.path,
            duration: h.duration,
            frame,
            required: h.required,
            option_selector: h.option_selector,
            target_selector: h.target_selector,
            old: h.old,
            char_range: h.char_range,
            extra: h.extra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ TaskKind 反序列化 ============

    #[test]
    fn test_task_kind_deserialize_browser() {
        // type=browser 应反序列化为 TaskKind::Browser
        let json = r#"{
            "type": "browser",
            "name": "登录测试",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
        if let TaskKind::Browser(cfg) = task {
            assert_eq!(cfg.url, "http://example.com");
            assert_eq!(cfg.common.name, "登录测试");
        }
    }

    #[test]
    fn test_task_kind_deserialize_script() {
        // type=script 应反序列化为 TaskKind::Script
        let json = r#"{
            "type": "script",
            "name": "脚本任务",
            "script_path": "test.py",
            "content": "print('hello')"
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Script(_)));
        if let TaskKind::Script(cfg) = task {
            assert_eq!(cfg.script_path, Some("test.py".to_string()));
            assert_eq!(cfg.content, Some("print('hello')".to_string()));
        }
    }

    #[test]
    fn test_task_kind_deserialize_shell_rejected() {
        // type=shell 已移除：明确报错并提示改用 script
        let json = r#"{
            "type": "shell",
            "name": "Shell 任务",
            "command": "echo hello"
        }"#;
        let result: Result<TaskKind, _> = serde_json::from_str(json);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("shell 已移除"));
    }

    #[test]
    fn test_task_kind_default_type_is_browser() {
        // type 字段缺失时默认归为浏览器任务
        let json = r#"{
            "name": "默认类型",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
    }

    #[test]
    fn test_task_kind_empty_type_is_browser() {
        // type 为空串视同缺失，默认归为浏览器任务（向后兼容）
        let json = r#"{
            "type": "",
            "name": "空类型",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
    }

    #[test]
    fn test_task_kind_unknown_type_rejected() {
        // 未知 type 值应反序列化报错，不再静默回退为浏览器任务（G6）
        let json = r#"{
            "type": "unknown_type",
            "name": "未知类型",
            "url": "http://example.com",
            "steps": []
        }"#;
        let result: Result<TaskKind, _> = serde_json::from_str(json);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("未知任务类型"));
        assert!(err.to_string().contains("unknown_type"));
    }

    #[test]
    fn test_task_kind_non_string_type_rejected() {
        // type 为非字符串（如数字）应报错，而不是误路由到浏览器任务
        let json = r#"{
            "type": 123,
            "name": "数字类型",
            "url": "http://example.com"
        }"#;
        let result: Result<TaskKind, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_task_kind_accessors() {
        // common()/common_mut()/type_name() 访问器覆盖两个变体
        let mut browser = TaskKind::Browser(TaskConfig::default());
        assert_eq!(browser.type_name(), "browser");
        assert_eq!(browser.common().name, "未命名任务");
        browser.common_mut().task_id = "b1".to_string();
        assert_eq!(browser.common().task_id, "b1");

        let mut script = TaskKind::Script(ScriptTaskConfig::default());
        assert_eq!(script.type_name(), "script");
        assert_eq!(script.common().description, "");
        script.common_mut().task_id = "s1".to_string();
        assert_eq!(script.common().task_id, "s1");
    }

    // ============ StepConfig 反序列化 ============

    #[test]
    fn test_step_config_basic_deserialize() {
        // 基本步骤配置反序列化
        let json = r##"{
            "id": "step1",
            "type": "input",
            "selector": "#username",
            "value": "test_user",
            "description": "填写用户名"
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.id, "step1");
        assert_eq!(step.step_type, "input");
        assert_eq!(step.selector, Some("#username".to_string()));
        assert_eq!(step.value, Some("test_user".to_string()));
    }

    #[test]
    fn test_step_config_code_normalized_to_script() {
        // 历史别名 `code` 应规范化为 `script` 字段
        let json = r#"{
            "id": "step1",
            "type": "eval",
            "code": "document.title"
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.script, Some("document.title".to_string()));
    }

    #[test]
    fn test_step_config_script_takes_precedence_over_code() {
        // script 优先于 code
        let json = r#"{
            "id": "step1",
            "type": "eval",
            "script": "window.location",
            "code": "document.title"
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.script, Some("window.location".to_string()));
    }

    #[test]
    fn test_step_config_frame_string_value() {
        // frame 字符串值应正确解析
        let json = r##"{
            "id": "step1",
            "type": "click",
            "frame": "#my-iframe"
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.frame, Some("#my-iframe".to_string()));
    }

    #[test]
    fn test_step_config_frame_non_string_is_none() {
        // frame 为布尔 true 等非字符串值时静默设为 None
        let json = r#"{
            "id": "step1",
            "type": "click",
            "frame": true
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert!(step.frame.is_none());
    }

    // ============ 无效输入处理 ============

    #[test]
    fn test_task_config_default_values() {
        // 测试 TaskConfig 默认值
        let cfg = TaskConfig::default();
        assert_eq!(cfg.timeout, DEFAULT_TASK_TIMEOUT_MS);
        assert_eq!(cfg.step_delay, DEFAULT_STEP_DELAY);
        assert_eq!(cfg.navigation_wait, DEFAULT_NAVIGATION_WAIT);
        assert!(!cfg.reveal_hidden);
        assert!(cfg.steps.is_empty());
        assert!(cfg.success_condition.is_empty());
    }

    #[test]
    fn test_task_config_deserialize_success_condition() {
        // success_condition 字段应正确反序列化
        let json = r#"{ "success_condition": "logged_in" }"#;
        let cfg: TaskConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.success_condition, "logged_in");
    }

    #[test]
    fn test_valid_step_types_include_goto_and_assert_text() {
        // 功能对齐 v4.2.3：goto / assert_text / navigate 应为合法步骤类型
        assert!(VALID_STEP_TYPES.contains(&"goto"));
        assert!(VALID_STEP_TYPES.contains(&"assert_text"));
        assert!(VALID_STEP_TYPES.contains(&"navigate"));
    }

    #[test]
    fn test_script_task_config_default_values() {
        // 测试 ScriptTaskConfig 默认值
        let cfg = ScriptTaskConfig::default();
        assert_eq!(cfg.timeout, DEFAULT_SCRIPT_TIMEOUT);
        assert!(cfg.script_path.is_none());
        assert!(cfg.content.is_none());
        assert!(cfg.args.is_empty());
    }

    #[test]
    fn test_task_kind_serde_roundtrip_script() {
        // 脚本任务序列化-反序列化往返
        let original = TaskKind::Script(ScriptTaskConfig {
            content: Some("print('test')".to_string()),
            ..Default::default()
        });
        let json = serde_json::to_string(&original).unwrap();
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        if let TaskKind::Script(cfg) = back {
            assert_eq!(cfg.content, Some("print('test')".to_string()));
        } else {
            panic!("应为 Script 类型");
        }
    }

    #[test]
    fn test_step_config_default_duration() {
        // 测试 StepHelper 默认 duration 为 1000ms
        let json = r#"{
            "id": "s1",
            "type": "sleep",
            "duration": 500
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.duration, 500);
    }

    #[test]
    fn test_step_config_default_required_true() {
        // 未写 required 字段的步骤默认为 true（B5：与 Python 侧 models.py 契约对齐，
        // 登录步骤失败不应被当作可选而静默吞掉）
        let json = r##"{
            "id": "s1",
            "type": "input",
            "selector": "#user",
            "value": "u"
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert!(step.required);
        // 显式 false 仍应保留（真正的可选步骤）
        let json = r##"{
            "id": "s1",
            "type": "input",
            "selector": "#user",
            "value": "u",
            "required": false
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert!(!step.required);
    }

    #[test]
    fn test_task_kind_serde_roundtrip_browser() {
        // 浏览器任务序列化-反序列化往返
        let original = TaskKind::Browser(TaskConfig {
            url: "http://example.com".to_string(),
            ..Default::default()
        });
        let json = serde_json::to_string(&original).unwrap();
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        if let TaskKind::Browser(cfg) = back {
            assert_eq!(cfg.url, "http://example.com");
        } else {
            panic!("应为 Browser 类型");
        }
    }

    // ============ http 直连任务 ============

    #[test]
    fn test_http_request_method_as_str_display_and_default() {
        // 方法名取大写（与 HTTP 报文一致），默认 GET
        assert_eq!(HttpRequestMethod::default(), HttpRequestMethod::Get);
        assert_eq!(HttpRequestMethod::Get.as_str(), "GET");
        assert_eq!(HttpRequestMethod::Post.as_str(), "POST");
        assert_eq!(HttpRequestMethod::Get.to_string(), "GET");
        assert_eq!(HttpRequestMethod::Post.to_string(), "POST");

        // serde 往返同样是大写字面量
        let json = serde_json::to_string(&HttpRequestMethod::Post).unwrap();
        assert_eq!(json, r#""POST""#);
        let back: HttpRequestMethod = serde_json::from_str(r#""GET""#).unwrap();
        assert_eq!(back, HttpRequestMethod::Get);
    }

    #[test]
    fn test_http_task_config_defaults() {
        // 默认：GET、字段全空、证书策略为 None（跟随全局）、metadata 为空对象
        let cfg = HttpTaskConfig::default();
        assert_eq!(cfg.method, HttpRequestMethod::Get);
        assert!(cfg.url.is_empty());
        // 认证页地址默认为空 = 未填，运行时回退方案的 auth_url（老配置不填照旧可用）
        assert!(cfg.auth_url.is_empty());
        assert!(cfg.headers.is_empty());
        assert!(cfg.body.is_empty());
        assert!(cfg.success_pattern.is_empty());
        assert!(cfg.failure_pattern.is_empty());
        assert!(cfg.crypto_script.is_empty());
        assert_eq!(cfg.ignore_https_errors, None);
        assert!(cfg.metadata.is_object());
    }

    #[test]
    fn test_task_kind_deserialize_http() {
        // type=http 应反序列化为 TaskKind::Http；缺省字段走默认值
        let json = r#"{
            "type": "http",
            "name": "直连登录",
            "url": "http://portal.example.com/login?user={username}",
            "method": "POST",
            "headers": "Content-Type: application/x-www-form-urlencoded",
            "body": "user={username}&pass={password}"
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Http(_)));
        if let TaskKind::Http(cfg) = task {
            assert_eq!(cfg.common.name, "直连登录");
            assert_eq!(cfg.method, HttpRequestMethod::Post);
            assert_eq!(cfg.url, "http://portal.example.com/login?user={username}");
            assert!(cfg.body.contains("{password}"));
            // 未写 ignore_https_errors → None（跟随全局），而非静默 false
            assert_eq!(cfg.ignore_https_errors, None);
            // 未写 auth_url → 空串（回退方案的认证地址），不是解析失败
            assert!(cfg.auth_url.is_empty());
        } else {
            panic!("应为 Http 类型");
        }
    }

    #[test]
    fn test_http_task_config_serde_roundtrip() {
        // 序列化带 "type":"http" → 反序列化字段一致（None 与 Some(false) 两态都覆盖）
        let original = TaskKind::Http(HttpTaskConfig {
            common: CommonFields {
                task_id: "portal_http".to_string(),
                name: "门户直连".to_string(),
                description: "复用直连参数".to_string(),
            },
            method: HttpRequestMethod::Post,
            url: "https://portal.example.com/auth".to_string(),
            auth_url: "https://portal.example.com/login".to_string(),
            headers: "Content-Type: application/x-www-form-urlencoded".to_string(),
            body: "username={username}&password={password}".to_string(),
            success_pattern: "登录成功".to_string(),
            failure_pattern: "密码错误".to_string(),
            crypto_script: "function transform(ctx) { return ctx; }".to_string(),
            pre_request: Some(HttpPreRequest {
                method: HttpRequestMethod::Get,
                url: "https://portal.example.com/api/csrf-token".to_string(),
                headers: "X-Requested-With: XMLHttpRequest".to_string(),
                body: String::new(),
                extract: "json:csrf_token".to_string(),
                name: "csrf".to_string(),
            }),
            logout_request: Some(crate::tasks::HttpActionRequest {
                method: HttpRequestMethod::Get,
                url: "https://portal.example.com/logout".to_string(),
                headers: String::new(),
                body: String::new(),
                wait_secs: 1.5,
            }),
            ignore_https_errors: None,
            metadata: serde_json::json!({ "source": "repo" }),
        });
        let json = serde_json::to_string(&original).unwrap();
        assert!(
            json.contains(r#""type":"http""#),
            "序列化必须带 http 标记: {json}"
        );
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        let TaskKind::Http(cfg) = back else {
            panic!("应为 Http 类型");
        };
        assert_eq!(cfg.common.task_id, "portal_http");
        assert_eq!(cfg.common.name, "门户直连");
        assert_eq!(cfg.common.description, "复用直连参数");
        assert_eq!(cfg.method, HttpRequestMethod::Post);
        assert_eq!(cfg.url, "https://portal.example.com/auth");
        assert_eq!(
            cfg.auth_url, "https://portal.example.com/login",
            "认证页地址必须原样往返（不是登录请求地址）"
        );
        assert_eq!(
            cfg.headers,
            "Content-Type: application/x-www-form-urlencoded"
        );
        assert_eq!(cfg.body, "username={username}&password={password}");
        assert_eq!(cfg.success_pattern, "登录成功");
        assert_eq!(cfg.failure_pattern, "密码错误");
        assert_eq!(cfg.crypto_script, "function transform(ctx) { return ctx; }");
        let pre = cfg.pre_request.expect("前置请求必须原样往返");
        assert_eq!(pre.method, HttpRequestMethod::Get);
        assert_eq!(pre.url, "https://portal.example.com/api/csrf-token");
        assert_eq!(pre.headers, "X-Requested-With: XMLHttpRequest");
        assert_eq!(pre.extract, "json:csrf_token");
        assert_eq!(pre.name, "csrf");
        let logout = cfg.logout_request.expect("退出登录请求必须原样往返");
        assert_eq!(logout.url, "https://portal.example.com/logout");
        assert!((logout.wait_secs - 1.5).abs() < f64::EPSILON);
        assert_eq!(cfg.ignore_https_errors, None);
        assert_eq!(cfg.metadata["source"], "repo");

        // 显式 Some(false)：必须原样往返，不得被当作「未设置」而退回 None
        let strict = TaskKind::Http(HttpTaskConfig {
            ignore_https_errors: Some(false),
            ..HttpTaskConfig::default()
        });
        let json = serde_json::to_string(&strict).unwrap();
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        if let TaskKind::Http(cfg) = back {
            assert_eq!(cfg.ignore_https_errors, Some(false));
        } else {
            panic!("应为 Http 类型");
        }
    }

    #[test]
    fn test_task_kind_type_name_three_kinds() {
        // 三类任务的类型名与 serde tag 一致
        assert_eq!(
            TaskKind::Browser(TaskConfig::default()).type_name(),
            "browser"
        );
        assert_eq!(
            TaskKind::Script(ScriptTaskConfig::default()).type_name(),
            "script"
        );
        assert_eq!(
            TaskKind::Http(HttpTaskConfig::default()).type_name(),
            "http"
        );
    }

    #[test]
    fn test_task_kind_http_common_accessors() {
        // 新臂同样可经 common()/common_mut() 读写共享字段
        let mut task = TaskKind::Http(HttpTaskConfig::default());
        assert_eq!(task.common().name, "未命名任务");
        task.common_mut().task_id = "h1".to_string();
        assert_eq!(task.common().task_id, "h1");
    }

    #[test]
    fn test_task_kind_unknown_type_error_lists_http() {
        // 未知类型的有效值列表必须包含新增的 http，否则用户按报错改仍会被拒
        let json = r#"{
            "type": "httpp",
            "name": "拼错类型",
            "url": "http://example.com"
        }"#;
        let result: Result<TaskKind, _> = serde_json::from_str(json);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("未知任务类型"));
        assert!(err.to_string().contains("httpp"));
        assert!(
            err.to_string().contains("browser / script / http"),
            "有效值列表应含 http: {err}"
        );
    }

    /// 老任务文件（没有 `pre_request` 键）必须照旧解析为 `None`：仓库里已有的直连
    /// 任务与用户磁盘上的历史文件都不能因为新增字段而失效
    #[test]
    fn legacy_http_task_without_pre_request_parses_as_none() {
        let json = r#"{
            "type": "http",
            "task_id": "old_portal",
            "name": "旧直连任务",
            "method": "GET",
            "url": "http://10.0.0.1/login?u={username}"
        }"#;
        let TaskKind::Http(cfg) = serde_json::from_str(json).unwrap() else {
            panic!("应为 Http 类型");
        };
        assert!(cfg.pre_request.is_none());
        assert!(cfg.logout_request.is_none(), "退出登录请求同样按缺省关闭");

        // 未配置时不落盘该键（不往老文件里塞 `"pre_request": null` 噪音）
        let roundtrip = serde_json::to_string(&TaskKind::Http(cfg)).unwrap();
        assert!(
            !roundtrip.contains("pre_request"),
            "未配置前置请求时不应序列化该键: {roundtrip}"
        );
        assert!(
            !roundtrip.contains("logout_request"),
            "未配置退出登录时不应序列化该键: {roundtrip}"
        );
    }

    /// 退出登录请求的往返与缺省：老文件没有该键时照旧解析，配置后原样带回
    #[test]
    fn logout_request_roundtrip_and_defaults() {
        let json = r#"{
            "type": "http",
            "name": "带下线的直连",
            "url": "http://10.0.0.1/login?u={username}",
            "logout_request": {
                "method": "POST",
                "url": "http://10.0.0.1/logout",
                "headers": "Content-Type: application/x-www-form-urlencoded",
                "body": "username={username}",
                "wait_secs": 2
            }
        }"#;
        let TaskKind::Http(cfg) = serde_json::from_str(json).unwrap() else {
            panic!("应为 Http 类型");
        };
        let logout = cfg
            .logout_request
            .as_ref()
            .expect("退出登录请求必须解析出来");
        assert_eq!(logout.method, HttpRequestMethod::Post);
        assert_eq!(logout.url, "http://10.0.0.1/logout");
        assert_eq!(logout.body, "username={username}");
        assert!((logout.wait_secs - 2.0).abs() < f64::EPSILON);

        // 缺省方法 GET、等待 0
        let bare = HttpActionRequest::default();
        assert_eq!(bare.method, HttpRequestMethod::Get);
        assert_eq!(bare.wait_secs, 0.0);

        // 序列化只带非默认键
        let roundtrip = serde_json::to_string(&TaskKind::Http(cfg)).unwrap();
        assert!(roundtrip.contains("logout_request"), "{roundtrip}");
    }

    /// 退出登录请求的形状校验：地址必填、仅 http(s)、等待秒数钳制
    #[test]
    fn logout_request_validate_checks_url_shape_and_wait() {
        let mut action = HttpActionRequest {
            url: "http://10.0.0.1/logout".into(),
            ..Default::default()
        };
        assert!(action.validate().is_ok());

        action.url = String::new();
        assert!(
            action
                .validate()
                .unwrap_err()
                .contains("退出登录请求缺少请求地址")
        );

        action.url = "ftp://10.0.0.1/logout".into();
        assert!(action.validate().unwrap_err().contains("仅支持 http/https"));

        action.url = "http:///logout".into();
        assert!(action.validate().unwrap_err().contains("缺少主机名"));

        action.url = "http://10.0.0.1/logout".into();
        action.wait_secs = -1.0;
        assert!(action.validate().unwrap_err().contains("等待秒数需 ≥ 0"));

        action.wait_secs = HttpActionRequest::MAX_WAIT_SECS + 0.5;
        assert!(action.validate().unwrap_err().contains("等待秒数最大 30"));

        action.wait_secs = f64::NAN;
        assert!(action.validate().unwrap_err().contains("等待秒数需 ≥ 0"));
    }

    /// 前置请求的取值方式与占位符名（形状判据在 tasks 层，保存与执行共用）
    #[test]
    fn pre_request_extract_path_and_placeholder_name() {
        let pre = HttpPreRequest {
            extract: "json:data.csrf_token".into(),
            ..Default::default()
        };
        assert_eq!(pre.extract_path().unwrap(), "data.csrf_token");
        assert_eq!(pre.placeholder_name(), "csrf_token");

        // 显式 name 优先于路径末段
        let named = HttpPreRequest {
            name: "csrf".into(),
            ..pre.clone()
        };
        assert_eq!(named.placeholder_name(), "csrf");

        for (extract, expected) in [
            ("", "缺少取值方式"),
            ("json:", "缺少字段名"),
            ("regex:(.*)", "只支持 `json:字段路径`"),
        ] {
            let bad = HttpPreRequest {
                extract: extract.into(),
                ..Default::default()
            };
            let err = bad.extract_path().unwrap_err();
            assert!(
                err.contains(expected),
                "`{extract}` 的报错应含「{expected}」: {err}"
            );
        }
    }

    /// 前置请求的形状校验：地址必填、仅 http(s)、必须有主机名
    #[test]
    fn pre_request_validate_checks_url_shape() {
        let base = HttpPreRequest {
            extract: "json:csrf_token".into(),
            ..Default::default()
        };

        let empty = HttpPreRequest {
            url: "   ".into(),
            ..base.clone()
        };
        assert!(empty.validate().unwrap_err().contains("缺少请求地址"));

        let no_host = HttpPreRequest {
            url: "http:///api/token".into(),
            ..base.clone()
        };
        assert!(no_host.validate().unwrap_err().contains("缺少主机名"));

        let wrong_scheme = HttpPreRequest {
            url: "ftp://portal/token".into(),
            ..base.clone()
        };
        assert!(
            wrong_scheme
                .validate()
                .unwrap_err()
                .contains("仅支持 http/https")
        );

        let ok = HttpPreRequest {
            url: "http://10.100.51.1/api/csrf-token".into(),
            ..base
        };
        assert!(ok.validate().is_ok());
    }
}
