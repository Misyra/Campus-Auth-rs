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
    /// 纯净模式（禁用扩展）
    pub pure_mode: bool,
    /// 隐身模式
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
            browser_channel: "playwright".to_string(),
            browser_custom_path: String::new(),
            custom_browser_engine: "chromium".to_string(),
            persistent_context: false,
            pure_mode: false,
            stealth_mode: false,
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
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct MonitorSettings {
    /// 是否启用网络监测
    pub enabled: bool,
    /// 探测间隔（秒）
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
            enabled: true,
            check_interval: 300,
            tcp_targets: vec![
                "8.8.8.8:53".to_string(),
                "114.114.114.114:53".to_string(),
                "www.baidu.com:443".to_string(),
            ],
            http_targets: vec![
                "https://connect.rom.miui.com/generate_204".to_string(),
                "https://connectivitycheck.platform.hicloud.com/generate_204".to_string(),
            ],
            url_targets: vec![
                "http://captive.apple.com/hotspot-detect.html".to_string(),
                "http://detectportal.firefox.com/success.txt".to_string(),
                "http://www.msftconnecttest.com/connecttest.txt".to_string(),
            ],
            url_expected_responses,
            tcp_enabled: false,
            http_enabled: true,
            url_enabled: true,
            local_check_enabled: true,
            profile_check_interval: 180,
            tcp_timeout: 2,
            http_timeout: 10,
            url_timeout: 10,
            auth_url_timeout: 5,
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
    /// 日志级别（OFF/ERROR/WARN/INFO/DEBUG/TRACE）
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
    /// 重试间隔（秒）
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
    /// 是否自动检查更新
    pub auto_update: bool,
    /// Web 服务监听端口
    pub port: u16,
    /// 是否已注册系统自启动
    pub autostart_enabled: bool,
    /// 任务脚本超时（秒）
    pub task_script_timeout: u32,
    /// 是否发送任务相关系统通知
    pub task_notification: bool,
    /// 是否启用开发者模式（启用后将下载 MinGit 等开发者工具）
    pub developer_mode: bool,
    /// 是否显示系统托盘图标（关闭后程序仅在 Web 控制台运行，无桌面图标）
    pub show_tray: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_start_browser: true,
            runtime_mode: "full".to_string(),
            startup_action: StartupAction::Monitor,
            auto_update: true,
            port: 50721,
            autostart_enabled: false,
            task_script_timeout: 30,
            task_notification: true,
            developer_mode: false,
            show_tray: true,
        }
    }
}

/// 自动更新配置
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct UpdaterSettings {
    /// 启动时是否检查更新
    pub check_on_startup: bool,
    /// 发布源 URL
    pub release_source_url: String,
    /// 检查间隔（小时）
    pub check_interval_hours: u32,
}

impl Default for UpdaterSettings {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            release_source_url: "https://api.github.com/repos/Misyra/Campus-Auth/releases/latest"
                .to_string(),
            check_interval_hours: 24,
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
