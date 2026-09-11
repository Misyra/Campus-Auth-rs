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
            auto_switch: true,
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
            post_login_delay: 5,
        }
    }
}

/// 暂停时段配置
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PauseSettings {
    /// 是否启用暂停时段
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
    /// 是否启用开发者模式（开放开发者辅助能力）
    pub developer_mode: bool,
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
            startup_action: StartupAction::Monitor,
            port: 50721,
            autostart_enabled: false,
            task_notification: true,
            developer_mode: false,
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
    /// 认证页面 URL
    pub auth_url: String,
    /// 重定向触发地址（仅劫持型门户）：非空即重定向模式，Worker 首导航到此 http 地址并跟随 302 到真门户；为空保持直连模式
    pub trigger_url: String,
    /// 运营商（已从 carrier 重命名）
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 活跃任务 ID
    pub active_task: String,
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
        }
    }
}

/// 启动动作枚举
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum StartupAction {
    /// 不自动执行任何动作
    None,
    /// 启动后进入网络监测
    #[default]
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
