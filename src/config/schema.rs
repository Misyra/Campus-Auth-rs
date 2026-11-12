//! 配置数据模型（serde 持久化结构）
//!
//! 本文件定义 `settings.json` 和 `config/profiles/{id}.json` 的完整 serde 数据模型。
//! 所有结构体使用 `#[derive(Deserialize, Serialize, Clone, Debug)]` 与 `#[serde(default)]`，
//! 缺失字段自动填充 `impl Default` 中定义的默认值，保证向前兼容。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// settings.json 顶层结构（v6 schema）
///
/// 聚合全局配置 [`GlobalConfig`] 与活跃 Profile 引用。
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct SettingsData {
    /// 配置 schema 版本号，当前代码支持最高 [`crate::config::CURRENT_CONFIG_VERSION`]
    pub config_version: u32,
    /// 当前活跃 Profile 的 ID
    pub active_profile_id: String,
    /// 是否启用根据网关/SSID 自动切换 Profile
    pub auto_switch: bool,
    /// 全局共享配置
    pub global: GlobalConfig,
}

impl Default for SettingsData {
    fn default() -> Self {
        Self {
            config_version: crate::config::CURRENT_CONFIG_VERSION,
            active_profile_id: "default".to_string(),
            // 默认关闭自动方案切换：多数用户只有一个网络环境，用不到按网关/SSID
            // 自动选方案；开启后 Engine 每 60s 检测并切方案，对单方案用户是空转。
            // 需要多网络自动切换的场景请在「方案」页显式开启。
            auto_switch: false,
            global: GlobalConfig::default(),
        }
    }
}

/// 全局共享设置（所有 Profile 共享）
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct GlobalConfig {
    /// 浏览器启动参数
    pub browser: BrowserSettings,
    /// 网络探测目标与参数
    pub monitor: MonitorSettings,
    /// 暂停时段配置
    pub pause: PauseSettings,
    /// 日志配置
    pub logging: LoggingSettings,
    /// 会话级重试策略
    pub retry_settings: RetrySettings,
    /// Python Worker 管理
    pub worker: WorkerSettings,
    /// 应用级设置
    pub app: AppSettings,
    /// 自动更新配置
    pub updater: UpdaterSettings,
    /// 推送通知配置（webhook / Bark / Server酱 / Telegram 通道）
    pub notification: NotificationSettings,
}

/// 浏览器启动参数
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct BrowserSettings {
    /// 是否无头模式
    pub headless: bool,
    /// 浏览器渠道（如 playwright）
    pub browser_channel: String,
    /// 自定义浏览器可执行文件路径
    pub browser_custom_path: String,
    /// 自定义浏览器引擎（chromium/firefox/webkit）
    pub custom_browser_engine: String,
    /// 是否使用持久化上下文（保留登录态）
    pub persistent_context: bool,
    /// 纯净模式（禁用扩展，默认开启）
    pub pure_mode: bool,
    /// 反自动化检测（默认开启：注入 webdriver/plugins/languages 伪装脚本，无副作用，可手动关闭）
    pub stealth_mode: bool,
    /// 隐身模式自定义脚本
    pub stealth_custom_script: String,
    /// 低资源模式
    pub low_resource_mode: bool,
    /// 禁用 Web 安全（仅调试用）
    pub disable_web_security: bool,
    /// 附加浏览器启动参数
    pub browser_args: String,
    /// 视口宽度
    pub viewport_width: u32,
    /// 视口高度
    pub viewport_height: u32,
    /// 语言区域
    pub locale: String,
    /// 时区 ID
    pub timezone_id: String,
    /// 忽略 HTTPS 证书错误
    pub ignore_https_errors: bool,
    /// 自定义 User-Agent
    pub user_agent: String,
    /// 附加请求头 JSON
    pub extra_headers_json: String,
    /// 页面/操作超时（秒）
    pub timeout: u32,
    /// 导航超时（秒）
    pub navigation_timeout: u32,
    /// 登录超时（秒）
    pub login_timeout: u32,
    /// 旧版浏览器绑定代理；无方案网络配置时用于推导统一策略
    pub bind_proxy: String,
    /// 旧 Worker 的重定向检测代理兼容字段；统一策略存在时不再单独生效
    pub redirect_test_use_proxy: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            headless: true,
            browser_channel: "msedge".to_string(),
            browser_custom_path: String::new(),
            custom_browser_engine: "chromium".to_string(),
            persistent_context: false,
            pure_mode: true,
            stealth_mode: true,
            stealth_custom_script: String::new(),
            low_resource_mode: false,
            disable_web_security: false,
            browser_args: String::new(),
            viewport_width: 1280,
            viewport_height: 720,
            locale: "zh-CN".to_string(),
            timezone_id: "Asia/Shanghai".to_string(),
            ignore_https_errors: true,
            user_agent: String::new(),
            extra_headers_json: String::new(),
            timeout: 30,
            navigation_timeout: 15,
            login_timeout: 120,
            bind_proxy: String::new(),
            redirect_test_use_proxy: false,
        }
    }
}

/// 网络探测目标与参数
///
/// 注：原 `enabled` 死字段已删除——监测启停由引擎 Start/Stop 控制，各探测
/// 类型另有 tcp_enabled/http_enabled/url_enabled 独立开关。
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct MonitorSettings {
    /// 探测间隔（秒，默认 300；Engine 消费时钳制到 20~1200）
    pub check_interval: u32,
    /// TCP 探测目标列表（host:port）
    pub tcp_targets: Vec<String>,
    /// HTTP 探测目标列表（URL）
    pub http_targets: Vec<String>,
    /// URL 标题探测目标列表
    pub url_targets: Vec<String>,
    /// URL 标题期望响应（URL -> 期望包含的标题片段）
    pub url_expected_responses: HashMap<String, String>,
    /// 是否启用 TCP 探测
    pub tcp_enabled: bool,
    /// 是否启用 HTTP 探测
    pub http_enabled: bool,
    /// 是否启用 URL 标题探测
    pub url_enabled: bool,
    /// 网络检测是否禁用代理（默认 true：检测直连，避免代理故障误判离线；
    /// 关闭后 HTTP/URL 探测跟随系统代理）
    pub disable_proxy: bool,
    /// Profile 切换检测间隔（秒）
    pub profile_check_interval: u32,
    /// TCP 探测超时（秒）
    pub tcp_timeout: u32,
    /// HTTP 探测超时（秒）
    pub http_timeout: u32,
    /// URL 探测超时（秒）
    pub url_timeout: u32,
    /// 认证页探测超时（秒）
    pub auth_url_timeout: u32,
    /// 严格登录模式：仅在拿到明确门户结论时才尝试自动登录（默认开启）
    ///
    /// 开启（默认）＝严格口径：要求探测给出明确门户证据（Captive 命中，或外网全失败且
    /// 认证入口可达），证据不足时不打扰用户，落 WaitForNetwork/NoAction。
    ///
    /// 关闭＝宽松口径：本地网卡已连接且探测未确认在线即尝试自动登录。应对
    /// 「学校门户 → 校园网认证」两级认证：门户决定账号，校园网再选运营商。这类网络的
    /// 网关常放行 204 探测域名（直通判 Online）或完全不返回劫持证据，且认证入口 TCP
    /// 预检可能失败；严格口径下这些情况都落 WaitForNetwork，自动登录永不触发。
    ///
    /// 关闭严格模式后以「本地链路可用」为触发下限：多启用一次网卡枚举
    /// （`list_interfaces`），凡未确认在线即升级为门户并建议登录。
    /// 认证地址 TCP 预检仍不参与（预检失败一律不构成拦截理由）。
    ///
    /// 代价：认证地址填错或门户确实无需登录时，会真去拉起浏览器，靠连续失败冷却
    /// （3 次 → 300s）与暂停时段节流。
    pub strict_login_mode: bool,
    /// 登录后等待 portal 生效的延迟（秒，0-60）
    pub post_login_delay: u32,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        let mut url_expected_responses = HashMap::new();
        url_expected_responses.insert(
            "http://captive.apple.com/hotspot-detect.html".to_string(),
            "Success".to_string(),
        );
        url_expected_responses.insert(
            "http://detectportal.firefox.com/success.txt".to_string(),
            "success".to_string(),
        );
        url_expected_responses.insert(
            "http://www.msftconnecttest.com/connecttest.txt".to_string(),
            "Microsoft Connect Test".to_string(),
        );
        Self {
            check_interval: 300,
            tcp_targets: vec![
                "8.8.8.8:53".to_string(),
                "114.114.114.114:53".to_string(),
                "www.baidu.com:443".to_string(),
            ],
            // 204 门户检测目标：统一使用主流厂商的 generate_204 端点（Android/captive
            // 检测事实标准）。用明文 http——门户劫持下才能收到 200 响应（劫持证据），
            // https 在劫持下 TLS 握手直接失败，只能得到 Fail 而非 Captive
            http_targets: vec![
                "http://connect.rom.miui.com/generate_204".to_string(),
                "http://connectivitycheck.platform.hicloud.com/generate_204".to_string(),
                "http://wifi.vivo.com.cn/generate_204".to_string(),
            ],
            url_targets: vec![
                "http://captive.apple.com/hotspot-detect.html".to_string(),
                "http://detectportal.firefox.com/success.txt".to_string(),
                "http://www.msftconnecttest.com/connecttest.txt".to_string(),
            ],
            url_expected_responses,
            tcp_enabled: false,
            // 默认仅启用 204 门户检测：204 端点语义单一（204=在线/200=劫持），厂商覆盖广，
            // 是误判率最低的单探测方案；URL 内容探测、TCP 探测与网卡检查默认关闭，按需启用
            http_enabled: true,
            url_enabled: false,
            disable_proxy: true,
            profile_check_interval: 180,
            tcp_timeout: 2,
            http_timeout: 10,
            url_timeout: 10,
            auth_url_timeout: 5,
            // 默认开启严格模式：只有明确门户证据才自动登录，行为与历史版本一致；
            // 关闭后退化为「网卡连着就试」，属行为变化，须用户显式选择
            strict_login_mode: true,
            post_login_delay: 5,
        }
    }
}

/// 暂停时段配置
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct PauseSettings {
    /// 是否启用暂停时段（默认启用：23:00–06:00 夜间不自动登录，宿舍定时
    /// 断网时段反复重连无意义；与运行模式「默认模式」预设的取值一致）
    pub enabled: bool,
    /// 暂停开始小时（0-23）
    pub start_hour: u8,
    /// 暂停开始分钟（0-59）
    pub start_minute: u8,
    /// 暂停结束小时（0-23）
    pub end_hour: u8,
    /// 暂停结束分钟（0-59）
    pub end_minute: u8,
}

impl Default for PauseSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            start_hour: 23,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
        }
    }
}

/// 日志配置
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct LoggingSettings {
    /// 日志级别（ERROR/WARN/INFO/DEBUG/TRACE，大小写不敏感；无效值回退 INFO）
    pub level: String,
    /// 是否写入日志文件
    pub file_enabled: bool,
    /// 日志保留天数
    pub retention_days: u32,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: "INFO".to_string(),
            file_enabled: true,
            retention_days: 7,
        }
    }
}

/// 会话级重试策略
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct RetrySettings {
    /// 最大重试次数
    pub max_retries: u32,
    /// 首次重试间隔（秒），逐次重试翻倍（指数退避，如 5 → 10 → 20）
    pub retry_interval: u32,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_interval: 5,
        }
    }
}

/// Python Worker 管理
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct WorkerSettings {
    /// Worker 空闲超时（秒），超时后关闭释放内存
    pub idle_timeout_seconds: u32,
    /// 是否在两次任务之间保持 Worker 存活
    pub keep_alive: bool,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            idle_timeout_seconds: 300,
            keep_alive: false,
        }
    }
}

/// 应用级设置
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct AppSettings {
    /// 启动后是否自动打开浏览器控制台
    pub auto_start_browser: bool,
    /// 运行模式（full / lightweight），启动期由 launcher 按字符串解析消费
    pub runtime_mode: String,
    /// 启动动作（none / monitor / login_once）
    pub startup_action: StartupAction,
    /// Web 服务监听端口
    pub port: u16,
    /// 自启动配置标记；Windows 仅作缓存，有效状态以系统注册与审批为准。
    pub autostart_enabled: bool,
    /// 是否发送任务相关系统通知（任务完成后经 notification 日志源提示）
    pub task_notification: bool,
    /// 是否显示系统托盘图标（关闭后程序仅在 Web 控制台运行，无桌面图标）
    pub show_tray: bool,
    /// 定时自重启间隔（小时，0 = 不启用）
    ///
    /// 长期运行场景下浏览器/Worker 常驻内存会缓慢增长，按周期优雅自重启回收。
    /// 运行时修改无需重启即生效（重启计时任务每分钟读一次该值）。
    pub auto_restart_hours: u32,
    /// 管理密码：非回环绑定（Docker / 局域网显式暴露）下经 `POST /api/auth/exchange`
    /// 的 password 分支换取鉴权 token 的唯一凭据；回环绑定下该分支一律拒绝。
    ///
    /// 空串 = 未配置（此时非回环绑定会因 fail-closed 门禁拒绝启动，除非以
    /// `--allow-unauthenticated-bind` 显式豁免）。CLI/env 覆盖值
    /// （`--management-password` / `CAMPUS_AUTH_MANAGEMENT_PASSWORD`）优先于本字段；
    /// exchange 每次请求实时读取本字段，UI 修改即时生效。
    /// 注意：明文存储（与 proxy_url 同级安全档），经 GET/PATCH /api/config 透出，
    /// 暴露面由 token 鉴权兜底；局域网 HTTP 明文传输下的凭据可见性风险见 docker/README.md。
    pub management_password: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_start_browser: true,
            runtime_mode: "full".to_string(),
            // 默认不自动开始监测：见 StartupAction::None 的说明
            startup_action: StartupAction::None,
            port: 50721,
            autostart_enabled: false,
            task_notification: true,
            show_tray: true,
            // 默认启用 24h 周期自重启：长期运行内存缓慢增长的主要回收手段，
            // 新装即受保护；存量用户 settings.json 已显式存值，不受默认值影响
            auto_restart_hours: 24,
            // 管理密码默认未配置：回环绑定（默认部署形态）无需它，非回环绑定
            // 由启动期 fail-closed 门禁提示用户显式配置或豁免
            management_password: String::new(),
        }
    }
}

/// 更新通道
///
/// - `Stable` 正式版：仅跟随稳定发布（GitHub releases/latest 语义）
/// - `Prerelease` 测试版：仅跟随预发布（alpha/beta 等 prerelease）
/// - `All` 全通道最新版：正式版与预发布一起比较，哪个高跟哪个
#[derive(Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// 正式版（默认）
    #[default]
    Stable,
    /// 测试版（预发布）
    Prerelease,
    /// 全通道最新版
    All,
}

/// 自动更新配置
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct UpdaterSettings {
    /// 启动时是否检查更新（`auto_check_enabled` 开启时生效）
    ///
    /// 关闭后启动阶段不检查，首轮检查按 `check_interval_hours` 周期等待；
    /// 总开关运行期由关到开时会立即补查一次（见 updater 后台循环）。
    pub check_on_startup: bool,
    /// 是否启用自动检查更新（总开关）
    ///
    /// 关闭后后台循环与启动检查全部静默，仅保留手动"立即检查"；运行时改回
    /// 无需重启（后台循环低频轮询该值）。
    pub auto_check_enabled: bool,
    /// 更新通道
    pub channel: UpdateChannel,
    /// 发布源 URL
    pub release_source_url: String,
    /// 检查间隔（小时，0 = 仅启动检查不周期检查）
    pub check_interval_hours: u32,
    /// 是否使用显式代理下载更新（地址见 [`Self::resolved_proxy_url`])
    pub use_proxy: bool,
    /// 代理地址（如 `http://127.0.0.1:7890`，支持非本机代理）
    pub proxy_url: String,
    /// 旧版"本地代理端口"字段：仅用于兼容读取，新配置一律写 `proxy_url`
    pub proxy_port: u16,
}

impl UpdaterSettings {
    /// 解析实际生效的代理地址：
    /// `proxy_url` 非空优先；为空时回退旧版 `proxy_port` 派生 `http://127.0.0.1:{port}`
    /// （兼容只写了端口的存量配置），两者都无则返回空串（跟随系统代理）。
    ///
    /// 注意 `Default` 里 `proxy_url` 必须保持空串：serde 缺字段时按 Default 填充，
    /// 若填完整地址会让旧配置自定义的 `proxy_port` 被默认值覆盖。
    pub fn resolved_proxy_url(&self) -> String {
        if !self.proxy_url.is_empty() {
            return self.proxy_url.clone();
        }
        if self.proxy_port > 0 {
            return format!("http://127.0.0.1:{}", self.proxy_port);
        }
        String::new()
    }
}

impl Default for UpdaterSettings {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            auto_check_enabled: true,
            channel: UpdateChannel::default(),
            release_source_url:
                "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest".to_string(),
            check_interval_hours: 24,
            use_proxy: false,
            proxy_url: String::new(),
            proxy_port: 7890,
        }
    }
}

/// 推送通知支持的事件类型白名单（与 `events::AppEvent::kind()` 同值；前端
/// 「事件类型开关组」的可选项也以此为准）
pub const NOTIFICATION_EVENT_TYPES: &[&str] = &[
    "login_succeeded",
    "login_failed",
    "network_down",
    "network_up",
    "task_finished",
    "task_failed",
    "update_available",
    "environment_ready",
    "profile_switched",
];

/// 推送通道数量上限（防误配刷屏与端点滥用；前端同口径）
pub const NOTIFICATION_MAX_CHANNELS: usize = 10;

/// 单通道超时上下界（秒）
pub const NOTIFICATION_TIMEOUT_MIN: u32 = 1;
pub const NOTIFICATION_TIMEOUT_MAX: u32 = 60;

/// 重试退避上下界（重试次数不含首次；基准间隔为首次重试前的等待）
pub const NOTIFICATION_RETRY_MAX_ATTEMPTS_MAX: u32 = 10;
pub const NOTIFICATION_RETRY_BASE_INTERVAL_MAX: u32 = 60;

/// webhook 自定义请求头数量上限
pub const NOTIFICATION_MAX_HEADERS: usize = 16;

/// 通道默认超时（秒）
fn default_notification_timeout() -> u32 {
    10
}

/// 推送通道类型
///
/// `Webhook` 为自定义 POST JSON 通道；`Bark` / `Serverchan` / `Telegram`
/// 为预设通道（各自只需凭据字段，消息格式内建模板化，无需用户写模板）；
/// `Os` 为本机桌面系统通知（无凭据、无远端端点）。
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannelKind {
    /// 自定义 Webhook（POST JSON 模板）
    #[default]
    Webhook,
    /// Bark（iOS 推送）
    Bark,
    /// Server酱（微信推送）
    Serverchan,
    /// Telegram Bot
    Telegram,
    /// 系统通知（本机桌面通知中心）
    Os,
}

impl NotificationChannelKind {
    /// 中文展示名（通道 `name` 留空时回退使用）
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Webhook => "Webhook",
            Self::Bark => "Bark",
            Self::Serverchan => "Server酱",
            Self::Telegram => "Telegram",
            Self::Os => "系统通知",
        }
    }
}

/// webhook 自定义请求头（`value` 可为密钥：明文存储，口径与 `proxy_url` 同级，
/// 暴露面由 GET/PATCH /api/config 的 token 鉴权兜底）
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct NotificationHeader {
    /// 头名称（如 `Authorization`）
    pub name: String,
    /// 头值（可为密钥）
    pub value: String,
}

/// 单个推送通道（serde tagged，`type` 为判别字段，snake_case）
///
/// 各通道共有 `id` / `name` / `enabled` / `timeout_seconds`；预设通道只需
/// 凭据字段，webhook 额外携带自定义头与 JSON 模板，系统通知无任何凭据字段。
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationChannel {
    /// 自定义 Webhook：向 `url` POST 渲染后的 JSON 模板
    Webhook {
        /// 通道唯一 id（同一 settings 内去重）
        id: String,
        /// 显示名（空时前端/日志回退类型名）
        name: String,
        /// 是否启用
        enabled: bool,
        /// 目标 URL（仅 http/https）
        url: String,
        /// 自定义请求头（可含密钥头；未含 `content-type` 时补 `application/json`）
        #[serde(default)]
        headers: Vec<NotificationHeader>,
        /// POST JSON 模板，占位符 `{{event_type}}/{{summary}}/{{profile}}/{{time}}/{{detail}}`；
        /// 留空使用内建默认模板
        #[serde(default)]
        body_template: String,
        /// 单次请求超时（秒）
        #[serde(default = "default_notification_timeout")]
        timeout_seconds: u32,
    },
    /// Bark：POST `{server}/{device_key}`，内建 `{"title","body"}` JSON
    Bark {
        /// 通道唯一 id
        id: String,
        /// 显示名
        name: String,
        /// 是否启用
        enabled: bool,
        /// 自建服务器地址（留空使用官方 `https://api.day.app`）
        #[serde(default)]
        server: String,
        /// 设备 key
        device_key: String,
        /// 单次请求超时（秒）
        #[serde(default = "default_notification_timeout")]
        timeout_seconds: u32,
    },
    /// Server酱：POST `https://sctapi.ftqq.com/{send_key}.send`（表单 title/desp）
    Serverchan {
        /// 通道唯一 id
        id: String,
        /// 显示名
        name: String,
        /// 是否启用
        enabled: bool,
        /// SendKey（sct 开头）
        send_key: String,
        /// 单次请求超时（秒）
        #[serde(default = "default_notification_timeout")]
        timeout_seconds: u32,
    },
    /// Telegram Bot：POST `https://api.telegram.org/bot{token}/sendMessage`
    Telegram {
        /// 通道唯一 id
        id: String,
        /// 显示名
        name: String,
        /// 是否启用
        enabled: bool,
        /// Bot token
        bot_token: String,
        /// 目标 chat id（数字或 @channel）
        chat_id: String,
        /// 单次请求超时（秒）
        #[serde(default = "default_notification_timeout")]
        timeout_seconds: u32,
    },
    /// 系统通知：本机桌面通知中心（Windows toast / macOS 通知中心 / Linux freedesktop）
    ///
    /// 无凭据字段（走本机桌面会话，没有远端端点可配）；`timeout_seconds`
    /// 对系统通知语义较弱（是阻塞发送调用的放弃上限，而非 HTTP 超时），
    /// 保留共字段一致性便于统一校验与展示。
    Os {
        /// 通道唯一 id
        id: String,
        /// 显示名
        name: String,
        /// 是否启用
        enabled: bool,
        /// 阻塞发送调用的放弃上限（秒）
        #[serde(default = "default_notification_timeout")]
        timeout_seconds: u32,
    },
}

impl NotificationChannel {
    /// 通道类型判别
    pub fn kind(&self) -> NotificationChannelKind {
        match self {
            Self::Webhook { .. } => NotificationChannelKind::Webhook,
            Self::Bark { .. } => NotificationChannelKind::Bark,
            Self::Serverchan { .. } => NotificationChannelKind::Serverchan,
            Self::Telegram { .. } => NotificationChannelKind::Telegram,
            Self::Os { .. } => NotificationChannelKind::Os,
        }
    }

    /// 通道唯一 id
    pub fn id(&self) -> &str {
        match self {
            Self::Webhook { id, .. }
            | Self::Bark { id, .. }
            | Self::Serverchan { id, .. }
            | Self::Telegram { id, .. }
            | Self::Os { id, .. } => id,
        }
    }

    /// 用户填写的显示名（可能为空）
    pub fn name(&self) -> &str {
        match self {
            Self::Webhook { name, .. }
            | Self::Bark { name, .. }
            | Self::Serverchan { name, .. }
            | Self::Telegram { name, .. }
            | Self::Os { name, .. } => name,
        }
    }

    /// 展示名：`name` 非空用它，否则回退类型中文名
    pub fn display_name(&self) -> &str {
        let name = self.name().trim();
        if name.is_empty() {
            self.kind().display_label()
        } else {
            name
        }
    }

    /// 是否启用
    pub fn enabled(&self) -> bool {
        match self {
            Self::Webhook { enabled, .. }
            | Self::Bark { enabled, .. }
            | Self::Serverchan { enabled, .. }
            | Self::Telegram { enabled, .. }
            | Self::Os { enabled, .. } => *enabled,
        }
    }

    /// 单次请求超时（秒）
    pub fn timeout_seconds(&self) -> u32 {
        match self {
            Self::Webhook {
                timeout_seconds, ..
            }
            | Self::Bark {
                timeout_seconds, ..
            }
            | Self::Serverchan {
                timeout_seconds, ..
            }
            | Self::Telegram {
                timeout_seconds, ..
            }
            | Self::Os {
                timeout_seconds, ..
            } => *timeout_seconds,
        }
    }
}

/// 推送通知设置（`global.notification` 节；serde default 免迁移，不 bump config_version）
///
/// 保存侧校验见 [`Self::validate`]（PATCH /api/config 合并后调用）；
/// 消费方为 `crate::notification`（订阅 EventBus 后按配置推送）。
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct NotificationSettings {
    /// 总开关（关闭后服务空转，不发送任何通知；测试发送端点不受此开关限制）
    pub enabled: bool,
    /// 推送通道列表（最多 [`NOTIFICATION_MAX_CHANNELS`] 个）
    pub channels: Vec<NotificationChannel>,
    /// 触发通知的事件类型集合（白名单校验；空集合 = 不推送任何事件）
    pub events: Vec<String>,
    /// 是否启用免打扰时段
    pub quiet_enabled: bool,
    /// 免打扰开始小时（0-23）
    pub quiet_start_hour: u8,
    /// 免打扰开始分钟（0-59）
    pub quiet_start_minute: u8,
    /// 免打扰结束小时（0-23；支持跨零点，如 22:00–08:00）
    pub quiet_end_hour: u8,
    /// 免打扰结束分钟（0-59）
    pub quiet_end_minute: u8,
    /// 发送失败的重试次数（不含首次；指数退避），0 = 只发一次
    pub retry_max_attempts: u32,
    /// 首次重试前等待的基准秒数，之后逐次翻倍（如 2 → 4 → 8）
    pub retry_base_interval_seconds: u32,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            // 默认关闭：推送是可选能力，需用户显式添加通道并开启
            enabled: false,
            channels: Vec::new(),
            // 默认只推最值得知道的两类：登录失败与网络断开（成功/恢复类噪音大）
            events: vec!["login_failed".to_string(), "network_down".to_string()],
            quiet_enabled: false,
            quiet_start_hour: 22,
            quiet_start_minute: 0,
            quiet_end_hour: 8,
            quiet_end_minute: 0,
            retry_max_attempts: 3,
            retry_base_interval_seconds: 2,
        }
    }
}

/// 校验 URL 仅 http/https 且带主机名（推送通道 URL 与 Bark 自建服务器共用）
fn validate_notification_url(url: &str, label: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| format!("{label}格式非法: {url}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!("{label}仅支持 http/https，当前为: {other}"));
        }
    }
    let host = parsed.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(format!("{label}缺少主机名: {url}"));
    }
    Ok(())
}

impl NotificationSettings {
    /// 事件类型是否在推送集合内
    pub fn is_event_enabled(&self, kind: &str) -> bool {
        self.events.iter().any(|e| e == kind)
    }

    /// 免打扰窗口（本地时间判定）；时/分越界（手改配置文件）时返回 `None`
    /// 视为未启用窗口，避免解析失败误伤
    pub fn quiet_window(&self) -> Option<(chrono::NaiveTime, chrono::NaiveTime)> {
        if !self.quiet_enabled {
            return None;
        }
        let start = chrono::NaiveTime::from_hms_opt(
            u32::from(self.quiet_start_hour),
            u32::from(self.quiet_start_minute),
            0,
        )?;
        let end = chrono::NaiveTime::from_hms_opt(
            u32::from(self.quiet_end_hour),
            u32::from(self.quiet_end_minute),
            0,
        )?;
        Some((start, end))
    }

    /// 保存侧全量校验（PATCH /api/config 合并后调用；全部中文错误信息）
    ///
    /// 校验项：通道数量 / id 唯一且非空 / URL 仅 http-https / 超时与重试上下界 /
    /// 事件类型白名单 / 免打扰时段时-分范围 / webhook 自定义头数量与头名 /
    /// webhook 模板用样例值渲染后必须是合法 JSON。
    pub fn validate(&self) -> Result<(), String> {
        if self.channels.len() > NOTIFICATION_MAX_CHANNELS {
            return Err(format!(
                "推送通道最多 {} 个，当前 {} 个",
                NOTIFICATION_MAX_CHANNELS,
                self.channels.len()
            ));
        }
        let mut seen_ids = std::collections::HashSet::new();
        for channel in &self.channels {
            let id = channel.id().trim();
            if id.is_empty() {
                return Err(format!(
                    "推送通道 {} 的 id 不能为空",
                    channel.display_name()
                ));
            }
            if !seen_ids.insert(id.to_string()) {
                return Err(format!("推送通道 id 重复: {id}"));
            }
            if !(NOTIFICATION_TIMEOUT_MIN..=NOTIFICATION_TIMEOUT_MAX)
                .contains(&channel.timeout_seconds())
            {
                return Err(format!(
                    "推送通道 {} 的超时必须在 {}-{} 秒之间",
                    channel.display_name(),
                    NOTIFICATION_TIMEOUT_MIN,
                    NOTIFICATION_TIMEOUT_MAX
                ));
            }
            match channel {
                NotificationChannel::Webhook {
                    url,
                    headers,
                    body_template,
                    ..
                } => {
                    validate_notification_url(url, "推送通道 Webhook 地址")?;
                    if headers.len() > NOTIFICATION_MAX_HEADERS {
                        return Err(format!("自定义请求头最多 {} 个", NOTIFICATION_MAX_HEADERS));
                    }
                    for header in headers {
                        if header.name.trim().is_empty() {
                            return Err("自定义请求头名称不能为空".to_string());
                        }
                    }
                    // 模板用样例值渲染后必须可解析为 JSON：占位符写在 JSON 字符串
                    // 值内才合法（如 {"title":"{{summary}}"}）；裸占位符渲染即碎，
                    // 保存时拦下远好于每次推送都静默失败
                    if !body_template.trim().is_empty()
                        && let Err(e) = serde_json::from_str::<serde_json::Value>(
                            &crate::notification::template::render_webhook_body(
                                body_template,
                                &crate::notification::template::MessageContent::sample(),
                            ),
                        )
                    {
                        return Err(format!("Webhook 消息模板渲染后不是合法 JSON: {e}"));
                    }
                }
                NotificationChannel::Bark {
                    server, device_key, ..
                } => {
                    if device_key.trim().is_empty() {
                        return Err("推送通道 Bark 的 device_key 不能为空".to_string());
                    }
                    // 自建服务器留空 = 官方地址，非空才校验
                    let server = server.trim();
                    if !server.is_empty() {
                        validate_notification_url(server, "推送通道 Bark 服务器地址")?;
                    }
                }
                NotificationChannel::Serverchan { send_key, .. } => {
                    if send_key.trim().is_empty() {
                        return Err("推送通道 Server酱 的 SendKey 不能为空".to_string());
                    }
                }
                NotificationChannel::Telegram {
                    bot_token, chat_id, ..
                } => {
                    if bot_token.trim().is_empty() {
                        return Err("推送通道 Telegram 的 Bot Token 不能为空".to_string());
                    }
                    if chat_id.trim().is_empty() {
                        return Err("推送通道 Telegram 的 Chat ID 不能为空".to_string());
                    }
                }
                // 系统通知无凭据字段可错：id 唯一/非空与超时上下界已由上方公共校验覆盖
                NotificationChannel::Os { .. } => {}
            }
        }
        for event in &self.events {
            if !NOTIFICATION_EVENT_TYPES.contains(&event.as_str()) {
                return Err(format!(
                    "通知事件类型 {event} 不在支持列表，可选值: {}",
                    NOTIFICATION_EVENT_TYPES.join(", ")
                ));
            }
        }
        for (hour, minute, label) in [
            (self.quiet_start_hour, self.quiet_start_minute, "免打扰开始"),
            (self.quiet_end_hour, self.quiet_end_minute, "免打扰结束"),
        ] {
            if hour > 23 {
                return Err(format!("{label}小时必须在 0-23 之间"));
            }
            if minute > 59 {
                return Err(format!("{label}分钟必须在 0-59 之间"));
            }
        }
        if self.retry_max_attempts > NOTIFICATION_RETRY_MAX_ATTEMPTS_MAX {
            return Err(format!(
                "重试次数最多 {} 次",
                NOTIFICATION_RETRY_MAX_ATTEMPTS_MAX
            ));
        }
        if self.retry_base_interval_seconds == 0
            || self.retry_base_interval_seconds > NOTIFICATION_RETRY_BASE_INTERVAL_MAX
        {
            return Err(format!(
                "重试基准间隔必须在 1-{} 秒之间",
                NOTIFICATION_RETRY_BASE_INTERVAL_MAX
            ));
        }
        Ok(())
    }
}

/// 登录执行渠道
///
/// `Browser` 走 Python Worker 浏览器自动化（默认，兼容存量）；`Http` 为 Rust
/// 进程内直连请求，不启动 Worker/浏览器，也不要求 Python 环境就绪；`Script` 为
/// 自定义脚本——把登录动作整个交给用户写的脚本任务，由 Rust 起本地子进程执行，
/// 同样不启动 Worker/浏览器（凭据经环境变量注入，契约见
/// `docs/guides/custom-script-guide.md` 的「用脚本登录」一节）。
#[derive(Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginChannel {
    /// 浏览器自动化（默认）
    #[default]
    Browser,
    /// 直连 HTTP 请求
    Http,
    /// 自定义脚本
    Script,
}

impl LoginChannel {
    /// 当前构建是否允许使用该渠道；路由器版不提供浏览器自动化。
    pub fn is_supported(self) -> bool {
        !cfg!(feature = "openwrt") || self.is_in_process()
    }

    /// 是否由 Rust 控制登录且不需要 Python 环境、浏览器与 Worker。
    ///
    /// 三种渠道里只有浏览器渠道要拉起 Worker：HTTP 请求在主进程发送，其纯计算
    /// 脚本在临时子进程执行；自定义脚本也由主进程启动子进程。凡"是否需要环境就绪 / 是否占用浏览器会话槽位 / 是否能取消 Bridge
    /// 任务"这类判定都应走本方法，避免各处各写一遍 `== Http || == Script`。
    pub fn is_in_process(self) -> bool {
        matches!(self, LoginChannel::Http | LoginChannel::Script)
    }

    /// 该渠道是否用某个任务承载"登录怎么做"（直连用直连任务，脚本用脚本任务）。
    ///
    /// 浏览器渠道也有 `active_task`，但它的登录参数（账号/地址）在方案里，
    /// 任务只是"操作步骤"——所以本方法只覆盖后两种渠道。
    pub fn binding_field(self) -> Option<&'static str> {
        match self {
            LoginChannel::Browser => None,
            LoginChannel::Http => Some("active_http_task"),
            LoginChannel::Script => Some("active_script_task"),
        }
    }
}

/// 持久会话录制动作（`profile_session_stop` 返回的 `recorded_actions` 元素）
///
/// 字段与 Python Worker 的录制契约逐字对齐（`python_worker/config_session.py`
/// 的汇聚逻辑 + README「录制契约」一节）：`type`/`selector`/`strategy`/`ts` 恒存在，
/// `value` 仅 `change` 动作携带（点击不录值，缺键反序列化为 `None`）。
///
/// `type` 与 `strategy` 刻意保持 `String` 而非枚举：两者都是 Worker 侧演进的
/// 受控词表（当前 `type` 为 `click`/`change`，`strategy` 为 `id`/`name`/
/// `data-testid`/`data`/`text`/`structure`），枚举会在未来 Worker 新增词表值时
/// 让旧主程序读不动方案文件；存储与回放按字面值处理即可。
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct RecordedAction {
    /// 动作类型：`click`（按钮/提交类点击）或 `change`（`<select>` 选择）
    #[serde(rename = "type")]
    pub action_type: String,
    /// 目标元素选择器（Worker 按启发式优先级取首个命中）
    pub selector: String,
    /// 选择器采用的启发式级别（同上，与 [`RecordedAction::selector`] 配对）
    pub strategy: String,
    /// 事件时间戳（毫秒；Worker 侧按此升序返回并做 500ms 去重）
    pub ts: i64,
    /// `change` 动作的选中值；`click` 无此键（序列化时跳过，与 Worker 返回形状一致）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// 单个 Profile 文件内容（`config/profiles/{id}.json`）
///
/// 凭证类字段（如 [`ProfileData::password`]）在磁盘上以 `ENC:` 前缀的密文存储，
/// 内存 `RuntimeConfig` 中解密为明文。
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct ProfileData {
    /// 统一校园网出口；缺省时在运行时兼容旧代理设置。
    pub network: Option<crate::network::settings::NetworkSettings>,
    /// Profile 唯一 ID
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 登录用户名
    pub username: String,
    /// 登录密码（明文提交时由 ProfileService 加密为 `ENC:` 密文）
    pub password: String,
    /// 固定登录网址；浏览器渠道留空时自动使用重定向触发地址
    pub auth_url: String,
    /// 自定义重定向触发地址；固定登录网址留空且本字段也为空时使用内置默认值
    pub trigger_url: String,
    /// 运营商（已从 carrier 重命名）
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 活跃任务 ID
    pub active_task: String,
    /// 登录执行渠道（browser=浏览器自动化默认；http=直连请求；script=自定义脚本）
    pub login_channel: LoginChannel,
    /// 直连渠道使用的任务 ID
    pub active_http_task: String,
    /// 脚本渠道使用的脚本任务 ID（空 = 未绑定，脚本登录不可用）
    ///
    /// 与 `active_http_task` 同一口径：脚本渠道**没有**内置兜底任务（登录逻辑只能
    /// 由用户写），故空值不是"用默认"，而是"脚本登录不可用"，保存与登录两侧都会拦。
    pub active_script_task: String,
    /// 是否启用持久浏览器会话（门户「记住密码」模式）
    ///
    /// 启用后浏览器渠道的登录走「无头持久上下文打开门户页 + 回放
    /// [`ProfileData::recorded_actions`] + 网络验证兜底」；会话数据持久在
    /// `<base>/browser_profiles/<id>/`（见 `utils::paths::browser_profile_dir`）。
    /// 字段在结构体末尾追加：serde 按声明序序列化，追加不打乱既有键序。
    pub persistent_session_enabled: bool,
    /// 持久会话录制动作（配置会话 `profile_session_stop` 取回，按 `ts` 升序）
    ///
    /// 由「配置会话」编排写入（Worker 返回什么存什么，上不封顶条数由 Worker 侧
    /// 截断并给 `truncated` 标记），不是用户在界面上手编的字段。
    pub recorded_actions: Vec<RecordedAction>,
}

impl Default for ProfileData {
    fn default() -> Self {
        Self {
            network: None,
            id: "default".to_string(),
            name: "默认网络".to_string(),
            username: String::new(),
            password: String::new(),
            auth_url: String::new(),
            trigger_url: String::new(),
            isp: String::new(),
            gateway_ip: String::new(),
            wifi_ssid: String::new(),
            active_task: String::new(),
            login_channel: LoginChannel::default(),
            active_http_task: String::new(),
            active_script_task: String::new(),
            persistent_session_enabled: false,
            recorded_actions: Vec::new(),
        }
    }
}

/// 启动动作枚举
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum StartupAction {
    /// 不自动执行任何动作
    ///
    /// 默认值：启动后不自动开始监测（用户可在控制台点「开始检测」）。
    /// 此前默认 `Monitor`，但多数用户不希望在启动时就拉起检测；
    /// 需要「开机即自动重连」的场景请在「设置 · 系统」显式选择「开始检测」。
    #[default]
    None,
    /// 启动后进入网络监测
    Monitor,
    /// 启动后执行一次登录
    LoginOnce,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn monitor_defaults_to_http_204_only() {
        let monitor = MonitorSettings::default();
        assert!(!monitor.tcp_enabled, "TCP 仅作为可选补充证据");
        assert!(monitor.http_enabled, "HTTP 204 是默认主探测");
        assert!(!monitor.url_enabled, "URL 内容探测默认关闭");
        assert!(
            monitor.strict_login_mode,
            "严格登录模式默认开启（关闭即退化为宽松触发，属行为变化）"
        );
    }

    /// 旧配置的手动检查开关不再生效，也不会再次写入配置文件。
    #[test]
    fn legacy_manual_checks_are_discarded() {
        let monitor: MonitorSettings = serde_json::from_value(serde_json::json!({
            "local_check_enabled": true,
            "check_auth_url": true,
            "strict_login_mode": false,
        }))
        .unwrap();
        let stored = serde_json::to_value(&monitor).unwrap();
        assert!(stored.get("local_check_enabled").is_none());
        assert!(stored.get("check_auth_url").is_none());
        assert!(!monitor.strict_login_mode);
    }

    /// 登录渠道枚举的 serde 字面量与判定方法。
    ///
    /// 字面量是前后端契约（前端 `LoginChannel` 联合类型、方案分享文件的
    /// `login_channel` 字段），改名即破坏存量配置与分享文件。
    #[test]
    fn login_channel_serde_and_helpers() {
        for (channel, literal) in [
            (LoginChannel::Browser, "browser"),
            (LoginChannel::Http, "http"),
            (LoginChannel::Script, "script"),
        ] {
            assert_eq!(
                serde_json::to_value(channel).unwrap(),
                serde_json::json!(literal)
            );
            assert_eq!(
                serde_json::from_value::<LoginChannel>(serde_json::json!(literal)).unwrap(),
                channel
            );
        }
        // 是否进程内完成登录：只有浏览器渠道要拉起 Worker 与 Playwright
        assert!(!LoginChannel::Browser.is_in_process());
        assert!(LoginChannel::Http.is_in_process());
        assert!(LoginChannel::Script.is_in_process());
        // 渠道 → 承载"登录怎么做"的绑定字段（浏览器渠道没有：它的登录参数在方案里）
        assert_eq!(LoginChannel::Browser.binding_field(), None);
        assert_eq!(LoginChannel::Http.binding_field(), Some("active_http_task"));
        assert_eq!(
            LoginChannel::Script.binding_field(),
            Some("active_script_task")
        );
        assert_eq!(LoginChannel::default(), LoginChannel::Browser);
    }

    /// 持久会话字段免迁移：旧方案 JSON 无新字段 → 反序列化得默认值，无需 bump
    /// config_version；再序列化时新字段追加在末尾（serde 按声明序产出），不打乱
    /// 既有键在磁盘文件里的先后。
    #[test]
    fn persistent_session_fields_default_and_key_order() {
        let legacy = serde_json::json!({
            "network": null,
            "id": "dorm",
            "name": "宿舍",
            "username": "2023xxxx",
            "password": "",
            "auth_url": "",
            "trigger_url": "",
            "isp": "telecom",
            "gateway_ip": "",
            "wifi_ssid": "",
            "active_task": "",
            "login_channel": "browser",
            "active_http_task": "",
            "active_script_task": "",
        });
        let profile: ProfileData = serde_json::from_value(legacy).unwrap();
        assert!(!profile.persistent_session_enabled, "缺省关闭");
        assert!(profile.recorded_actions.is_empty(), "缺省空表");

        // 新字段声明在结构体末尾：磁盘序列化时既有键序保持不变（json! 的 Map 是
        // BTreeMap 测不出顺序，这里直接对 struct 序列化文本断言相对位置）
        let text = serde_json::to_string_pretty(&profile).unwrap();
        let script_task_pos = text
            .find("\"active_script_task\"")
            .expect("既有字段不得丢失");
        let enabled_pos = text
            .find("\"persistent_session_enabled\"")
            .expect("新开关应落盘");
        let actions_pos = text.find("\"recorded_actions\"").expect("新动作表应落盘");
        assert!(
            script_task_pos < enabled_pos && enabled_pos < actions_pos,
            "新字段必须追加在既有字段之后，避免方案文件键序漂移"
        );
    }

    /// RecordedAction 与 Worker 录制契约（python_worker/config_session.py）逐字对齐：
    /// `type`/`selector`/`strategy`/`ts` 恒存在，`value` 仅 change 携带（click 缺键
    /// → None，回写时同样不带 value 键，与 Worker 返回形状一致）。
    #[test]
    fn recorded_action_matches_worker_contract() {
        let worker_shape = serde_json::json!([
            {
                "type": "click",
                "selector": "#login-btn",
                "strategy": "id",
                "ts": 1731475200123i64,
            },
            {
                "type": "change",
                "selector": "[name=\"isp\"]",
                "strategy": "name",
                "ts": 1731475200456i64,
                "value": "telecom",
            },
        ]);
        let actions: Vec<RecordedAction> = serde_json::from_value(worker_shape).unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action_type, "click");
        assert_eq!(actions[0].selector, "#login-btn");
        assert_eq!(actions[0].strategy, "id");
        assert_eq!(actions[0].ts, 1731475200123);
        assert_eq!(actions[0].value, None, "click 无 value 键");
        assert_eq!(actions[1].action_type, "change");
        assert_eq!(actions[1].value.as_deref(), Some("telecom"));

        // 回写形状：click 不得出现 "value": null（Worker 契约里 click 不带该键）
        let click_json = serde_json::to_value(&actions[0]).unwrap();
        assert!(click_json.get("value").is_none(), "{click_json}");
        assert_eq!(click_json["type"], "click");
        // change 回写保留 value
        let change_json = serde_json::to_value(&actions[1]).unwrap();
        assert_eq!(change_json["value"], "telecom");
    }

    #[test]
    fn openwrt_channel_support_preserves_in_process_login() {
        assert!(LoginChannel::Http.is_supported());
        assert!(LoginChannel::Script.is_supported());
        assert_eq!(
            LoginChannel::Browser.is_supported(),
            !cfg!(feature = "openwrt")
        );
    }

    /// 存量方案文件没有 `active_script_task` 时必须照旧解析（缺省空串 = 未绑定）
    #[test]
    fn profile_data_tolerates_missing_script_binding() {
        let legacy: ProfileData = serde_json::from_value(serde_json::json!({
            "id": "dorm",
            "name": "宿舍",
            "username": "u",
            "login_channel": "browser"
        }))
        .expect("老文件必须能解析（字段带 serde default）");
        assert_eq!(legacy.active_script_task, "");
        assert_eq!(legacy.login_channel, LoginChannel::Browser);
    }

    #[test]
    fn redirect_test_proxy_defaults_off_and_roundtrips() {
        let legacy: BrowserSettings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.redirect_test_use_proxy);
        let configured: BrowserSettings =
            serde_json::from_str(r#"{"redirect_test_use_proxy":true}"#).unwrap();
        assert!(configured.redirect_test_use_proxy);
        let restored: BrowserSettings =
            serde_json::from_value(serde_json::to_value(configured).unwrap()).unwrap();
        assert!(restored.redirect_test_use_proxy);
    }

    #[test]
    fn pause_defaults_to_night_window_enabled() {
        let pause = PauseSettings::default();
        assert!(pause.enabled, "暂停时段默认启用（夜间不自动登录）");
        assert_eq!(
            (pause.start_hour, pause.start_minute),
            (23, 0),
            "暂停开始默认 23:00"
        );
        assert_eq!(
            (pause.end_hour, pause.end_minute),
            (6, 0),
            "暂停结束默认次日 6:00"
        );
    }

    #[test]
    fn test_resolved_proxy_url() {
        // 新配置：proxy_url 优先
        let s = UpdaterSettings {
            proxy_url: "http://192.168.1.5:7890".into(),
            proxy_port: 7891,
            ..Default::default()
        };
        assert_eq!(s.resolved_proxy_url(), "http://192.168.1.5:7890");

        // 旧配置：proxy_url 为空时回退 proxy_port 派生
        let legacy = UpdaterSettings {
            proxy_url: String::new(),
            proxy_port: 7891,
            ..Default::default()
        };
        assert_eq!(legacy.resolved_proxy_url(), "http://127.0.0.1:7891");

        // 两者皆无 → 空串（跟随系统代理）
        let none = UpdaterSettings {
            proxy_url: String::new(),
            proxy_port: 0,
            ..Default::default()
        };
        assert_eq!(none.resolved_proxy_url(), "");

        // serde 缺字段兼容：旧 JSON（无 proxy_url）反序列化后走端口派生，
        // 且自定义端口不被新字段默认值覆盖
        let legacy_json = r#"{"check_on_startup":true,"release_source_url":"https://x","check_interval_hours":24,"use_proxy":true,"proxy_port":7891}"#;
        let parsed: UpdaterSettings = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.resolved_proxy_url(), "http://127.0.0.1:7891");
    }

    // ============ 推送通知（notification 节） ============

    /// 构造合法的 webhook 通道（测试正反例的基线）
    fn webhook_channel() -> NotificationChannel {
        NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: "自建机器人".into(),
            enabled: true,
            url: "https://example.com/hook".into(),
            headers: vec![NotificationHeader {
                name: "Authorization".into(),
                value: "Bearer secret".into(),
            }],
            body_template: String::new(),
            timeout_seconds: 10,
        }
    }

    /// 指定 URL 与消息模板的 webhook 通道（正反例局部变形用）
    fn webhook_with(url: &str, body_template: &str) -> NotificationChannel {
        NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: String::new(),
            enabled: true,
            url: url.into(),
            headers: Vec::new(),
            body_template: body_template.into(),
            timeout_seconds: 10,
        }
    }

    /// serde tagged 通道：type 判别字段字面量与默认超时
    #[test]
    fn notification_channel_serde_tagged() {
        let value = serde_json::to_value(webhook_channel()).unwrap();
        assert_eq!(value["type"], "webhook");
        assert_eq!(value["id"], "ch-1");
        assert_eq!(value["timeout_seconds"], 10);
        let back: NotificationChannel = serde_json::from_value(value).unwrap();
        assert_eq!(back, webhook_channel());
        // 未知 type 显式报错（不静默落入其他变体）
        assert!(
            serde_json::from_value::<NotificationChannel>(serde_json::json!({
                "type": "pigeon", "id": "x"
            }))
            .is_err()
        );
        // 缺 type 判别字段同样报错
        assert!(
            serde_json::from_value::<NotificationChannel>(serde_json::json!({ "id": "x" }))
                .is_err()
        );
    }

    /// 缺字段兼容：timeout_seconds / headers / body_template / server 均可省略
    #[test]
    fn notification_channel_tolerates_missing_fields() {
        let channel: NotificationChannel = serde_json::from_value(serde_json::json!({
            "type": "bark", "id": "b1", "name": "", "enabled": true, "device_key": "key"
        }))
        .unwrap();
        assert_eq!(channel.timeout_seconds(), 10, "缺省超时回退 10 秒");
        assert_eq!(channel.display_name(), "Bark", "空 name 回退类型名");
    }

    /// os 通道：serde tagged 往返 + 无凭据字段（缺 timeout_seconds 用默认）+ 校验通过
    #[test]
    fn notification_channel_os_serde_and_validate() {
        let channel = NotificationChannel::Os {
            id: "os-1".into(),
            name: "桌面提醒".into(),
            enabled: true,
            timeout_seconds: 5,
        };
        let value = serde_json::to_value(&channel).unwrap();
        assert_eq!(value["type"], "os");
        assert_eq!(value["id"], "os-1");
        assert_eq!(value["timeout_seconds"], 5);
        let back: NotificationChannel = serde_json::from_value(value).unwrap();
        assert_eq!(back, channel, "os 通道 serde 往返保形");

        // 缺 timeout_seconds：走共字段默认值；空 name 回退类型中文名
        let minimal: NotificationChannel = serde_json::from_value(serde_json::json!({
            "type": "os", "id": "os-1", "name": "", "enabled": true
        }))
        .unwrap();
        assert_eq!(minimal.timeout_seconds(), 10, "缺省超时回退 10 秒");
        assert_eq!(minimal.display_name(), "系统通知", "空 name 回退类型名");

        // 无字段可错：默认校验基线直接通过
        let s = NotificationSettings {
            channels: vec![channel],
            ..NotificationSettings::default()
        };
        assert!(
            s.validate().is_ok(),
            "os 通道应通过校验: {:?}",
            s.validate()
        );
    }

    /// 默认值：总开关关、事件集默认 login_failed + network_down、免打扰 22:00-08:00
    #[test]
    fn notification_defaults() {
        let s = NotificationSettings::default();
        assert!(!s.enabled, "推送默认关闭");
        assert!(s.channels.is_empty());
        assert_eq!(s.events, vec!["login_failed", "network_down"]);
        assert!(!s.quiet_enabled);
        assert_eq!((s.quiet_start_hour, s.quiet_start_minute), (22, 0));
        assert_eq!((s.quiet_end_hour, s.quiet_end_minute), (8, 0));
        assert_eq!(s.retry_max_attempts, 3);
        assert_eq!(s.retry_base_interval_seconds, 2);
        // 旧 settings.json 无 notification 节 → serde default 免迁移
        let legacy: crate::config::SettingsData =
            serde_json::from_str(r#"{"config_version":10,"active_profile_id":"default","auto_switch":false,"global":{"browser":{},"monitor":{},"pause":{},"logging":{},"retry_settings":{},"worker":{},"app":{},"updater":{}}}"#)
                .unwrap();
        assert_eq!(legacy.global.notification, NotificationSettings::default());
    }

    /// 校验正反例：合法配置通过；URL scheme / 超时越界 / 事件白名单 / 重复 id /
    /// 免打扰范围 / 重试界限 / 凭据缺失逐一拦截
    #[test]
    fn notification_validate_positive_and_negative() {
        let mut s = NotificationSettings {
            channels: vec![webhook_channel()],
            ..NotificationSettings::default()
        };
        assert!(s.validate().is_ok(), "基线配置应通过: {:?}", s.validate());

        // 事件类型白名单
        s.events = vec!["hack_the_planet".into()];
        assert!(s.validate().is_err());
        s.events = vec!["login_failed".into(), "network_down".into()];

        // 非法 scheme
        s.channels = vec![NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: String::new(),
            enabled: true,
            url: "ftp://example.com/hook".into(),
            headers: Vec::new(),
            body_template: String::new(),
            timeout_seconds: 10,
        }];
        let err = s.validate().unwrap_err();
        assert!(err.contains("http/https"), "{err}");
        // 缺主机名（WHATWG 解析器会吞掉多余斜杠把 ///path 当主机，用空 authority 触发）
        s.channels = vec![webhook_with("http://", "")];
        assert!(s.validate().is_err());
        s.channels = vec![webhook_channel()];

        // 超时越界
        let bark = |timeout_seconds: u32| NotificationChannel::Bark {
            id: "b1".into(),
            name: String::new(),
            enabled: true,
            server: String::new(),
            device_key: "k".into(),
            timeout_seconds,
        };
        s.channels = vec![bark(0)];
        assert!(s.validate().is_err());
        s.channels = vec![bark(61)];
        assert!(s.validate().is_err());

        // 重复 id
        s.channels = vec![webhook_channel(), webhook_channel()];
        let err = s.validate().unwrap_err();
        assert!(err.contains("id 重复"), "{err}");
        // 空 id
        s.channels = vec![NotificationChannel::Serverchan {
            id: "  ".into(),
            name: String::new(),
            enabled: true,
            send_key: "sct".into(),
            timeout_seconds: 10,
        }];
        assert!(s.validate().is_err());
        // 预设通道凭据缺失
        s.channels = vec![NotificationChannel::Telegram {
            id: "t1".into(),
            name: String::new(),
            enabled: true,
            bot_token: "tk".into(),
            chat_id: "  ".into(),
            timeout_seconds: 10,
        }];
        let err = s.validate().unwrap_err();
        assert!(err.contains("Chat ID"), "{err}");

        // 免打扰范围
        s.channels = Vec::new();
        s.quiet_start_hour = 24;
        assert!(s.validate().is_err());
        s.quiet_start_hour = 22;
        s.quiet_end_minute = 60;
        assert!(s.validate().is_err());
        s.quiet_end_minute = 0;

        // 重试界限
        s.retry_max_attempts = 11;
        assert!(s.validate().is_err());
        s.retry_max_attempts = 3;
        s.retry_base_interval_seconds = 0;
        assert!(s.validate().is_err());
        s.retry_base_interval_seconds = 2;
        assert!(s.validate().is_ok());
    }

    /// 非法 webhook 模板（渲染后不是合法 JSON）在保存时拦下
    #[test]
    fn notification_validate_rejects_broken_webhook_template() {
        // 裸占位符：渲染后 JSON 碎掉
        let mut s = NotificationSettings {
            channels: vec![NotificationChannel::Webhook {
                id: "ch-1".into(),
                name: String::new(),
                enabled: true,
                url: "https://example.com/hook".into(),
                headers: Vec::new(),
                body_template: r#"{"title": {{summary}}}"#.into(),
                timeout_seconds: 10,
            }],
            ..NotificationSettings::default()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("合法 JSON"), "{err}");
        // 合法模板（占位符位于字符串值内）通过
        s.channels = vec![webhook_with(
            "https://example.com/hook",
            r#"{"title": "{{summary}}", "text": "{{detail}}"}"#,
        )];
        assert!(s.validate().is_ok());
    }

    /// 免打扰窗口解析：越界时/分返回 None（手改配置文件的防御口径）
    #[test]
    fn notification_quiet_window_bounds() {
        let mut s = NotificationSettings::default();
        assert!(s.quiet_window().is_none(), "未启用时无窗口");
        s.quiet_enabled = true;
        let (start, end) = s.quiet_window().unwrap();
        assert_eq!(
            (start.hour(), start.minute(), end.hour(), end.minute()),
            (22, 0, 8, 0)
        );
        s.quiet_start_hour = 25;
        assert!(s.quiet_window().is_none(), "越界时/分不得构造窗口");
    }
}
