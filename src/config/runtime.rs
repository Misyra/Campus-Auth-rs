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
    /// 用户显式填写的登录网址；非空且 `trigger_url` 为空时直接使用
    pub auth_url: String,
    /// 重定向触发地址：非空即重定向模式；登录网址留空时由运行时补入默认值
    pub trigger_url: String,
    /// 运营商
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 活跃任务 ID
    pub active_task: String,
    /// 登录执行渠道（browser=浏览器自动化默认；http=直连请求）
    pub login_channel: crate::config::LoginChannel,
    /// 直连渠道使用的直连任务 ID（空 = 未绑定）
    ///
    /// 直连请求的全部参数（方法 / 地址 / 认证地址 / 请求头 / 请求体 / 判定关键字 /
    /// 凭据变换脚本 / 证书策略）都在该任务里（`tasks/http/<id>.json`），凭据仍取本
    /// 快照的 `username` / `password`。
    ///
    /// 与 `active_task` 的差别：浏览器渠道有内置默认任务可回退，直连**没有**
    /// （门户地址无法内置），故空值不是"用默认"，而是"直连登录不可用"。
    pub active_http_task: String,
}

impl ProfileSnapshot {
    /// 是否使用浏览器重定向登录。
    ///
    /// `trigger_url` 非空兼容旧版显式“重定向模式”；两个地址都为空则是新版
    /// “登录网址留空即自动跟随重定向”的默认行为。直连 HTTP 渠道不使用浏览器
    /// 触发地址，因此始终返回 `false`。
    pub fn uses_redirect_login(&self) -> bool {
        self.login_channel == crate::config::LoginChannel::Browser
            && (!self.trigger_url.trim().is_empty() || self.auth_url.trim().is_empty())
    }

    /// 浏览器首导航的有效地址。
    ///
    /// 旧版同时保存认证地址与触发地址时仍以触发地址为准，避免升级后改变既有
    /// 方案行为；新版填写固定登录网址时，前端会清空 `trigger_url`。
    pub fn effective_browser_login_url(&self) -> &str {
        let trigger = self.trigger_url.trim();
        if !trigger.is_empty() {
            trigger
        } else {
            let auth = self.auth_url.trim();
            if auth.is_empty() {
                crate::config::DEFAULT_TRIGGER_URL
            } else {
                auth
            }
        }
    }
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

    // 新交互不再要求用户先打开“重定向模式”：固定登录网址留空就是自动跟随
    // 网关跳转。默认值仅进入运行时，持久化仍保留空串，方便 UI 明确展示“未填写”。
    let trigger_url = if profile.login_channel == crate::config::LoginChannel::Browser
        && profile.auth_url.trim().is_empty()
        && profile.trigger_url.trim().is_empty()
    {
        crate::config::DEFAULT_TRIGGER_URL.to_string()
    } else {
        profile.trigger_url.clone()
    };

    let profile_snapshot = ProfileSnapshot {
        id: profile.id.clone(),
        name: profile.name.clone(),
        username: profile.username.clone(),
        password,
        auth_url: profile.auth_url.clone(),
        trigger_url,
        isp: profile.isp.clone(),
        gateway_ip: profile.gateway_ip.clone(),
        wifi_ssid: profile.wifi_ssid.clone(),
        active_task: profile.active_task.clone(),
        login_channel: profile.login_channel,
        active_http_task: profile.active_http_task.clone(),
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
            login_channel: crate::config::LoginChannel::default(),
            active_http_task: String::new(),
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

    #[test]
    fn blank_login_url_uses_default_redirect_trigger_at_runtime() {
        let mut profile = ProfileData::default();
        profile.auth_url.clear();
        profile.trigger_url.clear();
        let temp = tempfile::tempdir().unwrap();
        let crypto = PasswordCrypto::new(temp.path().join("key"));
        let runtime = build_runtime_config(&SettingsData::default(), &profile, &crypto).unwrap();

        assert!(runtime.profile.uses_redirect_login());
        assert_eq!(
            runtime.profile.effective_browser_login_url(),
            crate::config::DEFAULT_TRIGGER_URL
        );
    }

    #[test]
    fn explicit_login_url_stays_direct_and_legacy_trigger_keeps_precedence() {
        let mut direct = snapshot_with_password("pw");
        direct.auth_url = "https://portal.example/login".into();
        direct.trigger_url.clear();
        assert!(!direct.uses_redirect_login());
        assert_eq!(
            direct.effective_browser_login_url(),
            "https://portal.example/login"
        );

        direct.trigger_url = "http://trigger.example/".into();
        assert!(direct.uses_redirect_login());
        assert_eq!(
            direct.effective_browser_login_url(),
            "http://trigger.example/"
        );

        direct.login_channel = crate::config::LoginChannel::Http;
        direct.auth_url.clear();
        assert!(
            !direct.uses_redirect_login(),
            "直连 HTTP 渠道不得被浏览器重定向设置接管"
        );
    }
}
