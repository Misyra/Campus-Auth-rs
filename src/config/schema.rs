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
    /// 绑定的代理地址
    pub bind_proxy: String,
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
    /// 探测间隔（秒，默认 120；Engine 消费时钳制到 20~1200）
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
    /// 是否启用物理网卡连接检查（步骤 2：list_interfaces 判定是否存在在线网卡）
    pub local_check_enabled: bool,
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
    /// 登录前是否先用 TCP 直连确认认证地址可达（不可达则直接判失败，不启动浏览器）
    ///
    /// 仅作用于**手动登录 / 单次登录**：自动登录由监测侧的 Captive 判定驱动，不走此预检。
    /// 默认关闭——部分校园网对裸 TCP 直连有限制，开启后一旦误判不可达会拦掉本可成功的
    /// 登录；且手动登录失败与否浏览器都会给出明确错误，预检的止损价值有限。
    pub check_auth_url: bool,
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
    /// （`list_interfaces`，与手动诊断同一路径），凡未确认在线即升级为门户并建议登录。
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
            check_interval: 120,
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
            local_check_enabled: false,
            disable_proxy: true,
            profile_check_interval: 180,
            tcp_timeout: 2,
            http_timeout: 10,
            url_timeout: 10,
            auth_url_timeout: 5,
            check_auth_url: false,
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
    /// 是否已注册系统自启动
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
    /// 是否在 Rust 进程内完成登录（不需要 Python 环境、浏览器与 Worker）。
    ///
    /// 三种渠道里只有浏览器渠道要拉起 Worker：直连在进程内发 HTTP，脚本在进程内
    /// 起子进程。凡"是否需要环境就绪 / 是否占用浏览器会话槽位 / 是否能取消 Bridge
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

/// 单个 Profile 文件内容（`config/profiles/{id}.json`）
///
/// 凭证类字段（如 [`ProfileData::password`]）在磁盘上以 `ENC:` 前缀的密文存储，
/// 内存 [`RuntimeConfig`] 中解密为明文。
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct ProfileData {
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
}

impl Default for ProfileData {
    fn default() -> Self {
        Self {
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

    #[test]
    fn monitor_defaults_to_http_204_only() {
        let monitor = MonitorSettings::default();
        assert!(!monitor.tcp_enabled, "TCP 仅作为可选补充证据");
        assert!(monitor.http_enabled, "HTTP 204 是默认主探测");
        assert!(!monitor.url_enabled, "URL 内容探测默认关闭");
        assert!(!monitor.local_check_enabled, "本地链路诊断默认关闭");
        assert!(!monitor.check_auth_url, "手动登录前认证入口预检默认关闭");
        assert!(
            monitor.strict_login_mode,
            "严格登录模式默认开启（关闭即退化为宽松触发，属行为变化）"
        );
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
}
