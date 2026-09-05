//! 运行时配置快照（RuntimeConfig）
//!
//! [`RuntimeConfig`] 是从 [`SettingsData`] 全局配置与当前活跃 [`ProfileData`]
//! 合并而成的不可变快照。各服务通过 `ConfigService::runtime().load()` 无锁读取。
//! 配置变更时构建全新的 `RuntimeConfig` 并原子替换。

use zeroize::Zeroizing;

use crate::config::ConfigError;
use crate::config::crypto::PasswordCrypto;
use crate::config::schema::{
    AppSettings, BrowserSettings, LoggingSettings, MonitorSettings, PauseSettings, ProfileData,
    RetrySettings, SettingsData, UpdaterSettings, WorkerSettings,
};

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
///
/// 手动实现 `Debug`（G25）：`Zeroizing<String>` 的派生 Debug 会输出明文密码，
/// 而 `RuntimeConfig` 的 Debug 输出会进入日志与错误信息——密码字段一律以
/// `[REDACTED]` 占位，其余字段保持与 derive 等价的格式。
#[derive(Clone)]
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
    /// 重定向触发地址：非空即重定向模式（直连预检/监测 auth 探测跳过，Worker 首导航用它）
    pub trigger_url: String,
    /// 运营商
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 活跃任务 ID
    pub active_task: String,
}

impl std::fmt::Debug for ProfileSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 密码字段脱敏：绝不输出明文（G25）
        f.debug_struct("ProfileSnapshot")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("auth_url", &self.auth_url)
            .field("trigger_url", &self.trigger_url)
            .field("isp", &self.isp)
            .field("gateway_ip", &self.gateway_ip)
            .field("wifi_ssid", &self.wifi_ssid)
            .field("active_task", &self.active_task)
            .finish()
    }
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
/// 密码字段从 `ENC:` 密文解密为明文；解密失败时使用空密码并按 Profile 记录
/// 解密失败标志（由 `/api/system` 的 `password_decryption_failed` 汇总暴露，
/// 前端据此提示用户重新输入）。
pub fn build_runtime_config(
    settings: &SettingsData,
    profile: &ProfileData,
    crypto: &PasswordCrypto,
) -> Result<RuntimeConfig, ConfigError> {
    // 解密密码；失败置空并按 Profile 登记解密失败（F10：标志按 profile id 记录，
    // 其他 Profile 的成功解密不会抹掉该提示）
    let password = crypto
        .decrypt_to_zeroizing(&profile.id, &profile.password)
        .unwrap_or_else(|_| Zeroizing::new(String::new()));

    let profile_snapshot = ProfileSnapshot {
        id: profile.id.clone(),
        name: profile.name.clone(),
        username: profile.username.clone(),
        password,
        auth_url: profile.auth_url.clone(),
        trigger_url: profile.trigger_url.clone(),
        isp: profile.isp.clone(),
        gateway_ip: profile.gateway_ip.clone(),
        wifi_ssid: profile.wifi_ssid.clone(),
        active_task: profile.active_task.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个含敏感密码的 ProfileSnapshot
    fn snapshot_with_password(pw: &str) -> ProfileSnapshot {
        ProfileSnapshot {
            id: "default".to_string(),
            name: "默认".to_string(),
            username: "user@example.com".to_string(),
            password: Zeroizing::new(pw.to_string()),
            auth_url: "http://10.0.0.1/login".to_string(),
            trigger_url: String::new(),
            isp: "移动".to_string(),
            gateway_ip: "10.0.0.1".to_string(),
            wifi_ssid: "Campus".to_string(),
            active_task: String::new(),
        }
    }

    // ============ Debug 脱敏（G25） ============

    #[test]
    fn test_debug_output_redacts_password() {
        // Debug 输出不得包含密码明文
        let s = snapshot_with_password("SUPER_SECRET_PASSWORD_123");
        let dbg = format!("{s:?}");
        assert!(
            !dbg.contains("SUPER_SECRET_PASSWORD_123"),
            "Debug 泄密: {dbg}"
        );
        assert!(
            dbg.contains("[REDACTED]"),
            "密码字段应以 [REDACTED] 占位: {dbg}"
        );
    }

    #[test]
    fn test_debug_output_keeps_other_fields() {
        // 其余字段保持与 derive 等价的可见性（排障所需）
        let s = snapshot_with_password("pw");
        let dbg = format!("{s:?}");
        assert!(dbg.contains("ProfileSnapshot"));
        assert!(dbg.contains("user@example.com"));
        assert!(dbg.contains("http://10.0.0.1/login"));
        assert!(dbg.contains("id: \"default\""));
    }

    #[test]
    fn test_runtime_config_debug_redacts_nested_password() {
        // RuntimeConfig 的派生 Debug 嵌套输出 ProfileSnapshot 时同样脱敏
        let snapshot = snapshot_with_password("NESTED_SECRET");
        let rc = RuntimeConfig {
            browser: BrowserSettings::default(),
            monitor: MonitorSettings::default(),
            pause: PauseSettings::default(),
            logging: LoggingSettings::default(),
            retry: RetrySettings::default(),
            worker: WorkerSettings::default(),
            app: AppSettings::default(),
            updater: UpdaterSettings::default(),
            profile: snapshot,
            auto_switch: true,
        };
        let dbg = format!("{rc:?}");
        assert!(!dbg.contains("NESTED_SECRET"), "嵌套 Debug 泄密: {dbg}");
        assert!(dbg.contains("[REDACTED]"));
    }
}
