//! 运行时配置快照（RuntimeConfig）
//!
//! [`RuntimeConfig`] 是从 [`SettingsData`] 全局配置与当前活跃 [`ProfileData`]
//! 合并而成的不可变快照。各服务通过 `ConfigService::runtime().load()` 无锁读取。
//! 配置变更时构建全新的 `RuntimeConfig` 并原子替换。

use zeroize::Zeroizing;

use crate::config::crypto::PasswordCrypto;
use crate::config::schema::{
    AppSettings, BrowserSettings, LoggingSettings, MonitorSettings, PauseSettings, ProfileData,
    RetrySettings, SettingsData, UpdaterSettings, WorkerSettings,
};
use crate::config::ConfigError;

/// 配置变更信号，通过 mpsc 通道从 ConfigService 通知 SchedulerService 等消费者
///
/// 仅传递"配置变了"这一事实，不携带内容——消费者收到后主动重新读取 ConfigService。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigReloadSignal {
    /// 全局配置变更（影响所有服务的公共参数）
    GlobalChanged,
    /// 活跃 Profile 切换（仅影响凭证/匹配规则，不影响调度器任务表）
    ProfileSwitched {
        /// 切换后的 Profile ID
        id: String,
    },
}

/// 活跃 Profile 的不可变快照（含解密后的密码）
///
/// 内嵌于 [`RuntimeConfig`]，不独立序列化。密码字段为 [`Zeroizing<String>]，
/// drop 时自动清零。
#[derive(Clone, Debug)]
pub struct ProfileSnapshot {
    /// Profile ID
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 登录用户名
    pub username: String,
    /// 登录密码（明文，已解密并清零保护）
    pub password: Zeroizing<String>,
    /// 认证页面 URL
    pub auth_url: String,
    /// 运营商
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 活跃任务 ID
    pub active_task: String,
    /// 密码是否解密失败、需要用户重新输入
    ///
    /// 解密失败（旧格式/密钥不匹配）时置 `true`，前端据此弹出重新输入提示。
    pub password_reinput_needed: bool,
}

/// 运行时配置不可变快照
///
/// 合并全局配置与活跃 Profile。构建后不可变，配置变更时整体原子替换。
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// 浏览器启动参数
    pub browser: BrowserSettings,
    /// 网络探测参数
    pub monitor: MonitorSettings,
    /// 暂停时段配置
    pub pause: PauseSettings,
    /// 日志配置
    pub logging: LoggingSettings,
    /// 会话级重试策略
    pub retry: RetrySettings,
    /// Python Worker 管理
    pub worker: WorkerSettings,
    /// 应用级设置
    pub app: AppSettings,
    /// 自动更新配置
    pub updater: UpdaterSettings,
    /// 活跃 Profile 凭证与匹配规则
    pub profile: ProfileSnapshot,
    /// 是否启用基于网关/SSID 的 Profile 自动切换
    pub auto_switch: bool,
}

/// 由全局配置与活跃 Profile 构建运行时快照
///
/// 密码字段从 `ENC:` 密文解密为明文；解密失败时使用空密码并保留解密失败标记，
/// 由前端检测后提示用户重新输入。
pub fn build_runtime_config(
    settings: &SettingsData,
    profile: &ProfileData,
    crypto: &PasswordCrypto,
) -> Result<RuntimeConfig, ConfigError> {
    // 解密密码；失败则置空并记录重新输入标记（供前端提示）
    let (password, reinput) = match crypto.decrypt_to_zeroizing(&profile.password) {
        Ok(p) => (p, false),
        Err(_) => (Zeroizing::new(String::new()), true),
    };

    let profile_snapshot = ProfileSnapshot {
        id: profile.id.clone(),
        name: profile.name.clone(),
        username: profile.username.clone(),
        password,
        auth_url: profile.auth_url.clone(),
        isp: profile.isp.clone(),
        gateway_ip: profile.gateway_ip.clone(),
        wifi_ssid: profile.wifi_ssid.clone(),
        active_task: profile.active_task.clone(),
        password_reinput_needed: reinput,
    };

    Ok(RuntimeConfig {
        browser: settings.global.browser.clone(),
        monitor: settings.global.monitor.clone(),
        pause: settings.global.pause.clone(),
        logging: settings.global.logging.clone(),
        retry: settings.global.retry_settings.clone(),
        worker: settings.global.worker.clone(),
        app: settings.global.app.clone(),
        updater: settings.global.updater.clone(),
        profile: profile_snapshot,
        auto_switch: settings.auto_switch,
    })
}
