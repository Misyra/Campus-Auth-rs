//! 配置系统：ConfigService + ProfileService + 加密 + 迁移
//!
//! 本模块负责 `settings.json` 与 `profiles/*.json` 的读写、原子写入、AES-256-GCM 密码加解密、
//! schema 版本迁移，以及通过 `Arc<ArcSwap<RuntimeConfig>>` 暴露无锁运行时配置快照。

pub mod crypto;
pub mod migration;
pub mod profiles;
pub mod runtime;
pub mod schema;
pub mod service;

/// 当前代码支持的最高 schema 版本
pub const CURRENT_CONFIG_VERSION: u32 = 6;
/// 配置目录（相对于 base_path）
pub const CONFIG_DIR: &str = "config";
/// 主配置文件名
pub const SETTINGS_FILE: &str = "settings.json";
/// Profile 文件目录（相对于 config_dir）
pub const PROFILES_DIR: &str = "profiles";
/// 损坏文件备份前缀
pub const CORRUPT_PREFIX: &str = "settings.corrupt.";
/// 迁移备份目录前缀
pub const BACKUP_PREFIX: &str = ".backup.v5.";
/// Profile 安全删除目录
pub const TRASH_DIR: &str = ".trash";
// 重新导出公共类型，供其他模块直接 `use crate::config::Xxx`
pub use crypto::PasswordCrypto;
pub use profiles::ProfileService;
pub use runtime::{build_runtime_config, ConfigReloadSignal, ProfileSnapshot, RuntimeConfig};
pub use schema::{
    AppSettings, BrowserSettings, GlobalConfig, LoggingSettings, MonitorSettings, PauseSettings,
    ProfileData, RetrySettings, SettingsData, StartupAction, UpdaterSettings, WorkerSettings,
};
pub use service::ConfigError;
pub use service::ConfigService;

/// Web 层消费的配置服务抽象（M1 细粒度 state：config 域）
///
/// handler 通过 `State<Arc<dyn ConfigApi>>` 提取依赖（或经 AppState 直字段
/// `state.config` 触达），不再访问 `state.container`，测试可注入内存实现
/// （见 `web/routes/config.rs` 模块测试）。
#[async_trait::async_trait]
pub trait ConfigApi: Send + Sync {
    /// 加载 settings.json（异步，内部走 spawn_blocking）。
    async fn load_settings_async(&self) -> SettingsData;
    /// 原子写入 settings.json。
    async fn save_settings(&self, data: &SettingsData) -> Result<(), ConfigError>;
    /// 加载单个 Profile。
    fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError>;
    /// 保存 Profile。
    async fn save_profile(&self, profile: &ProfileData) -> Result<(), ConfigError>;
    /// 重载配置并广播变更信号。
    async fn reload(&self) -> Result<(), ConfigError>;
    /// 密文是否可解密（用于脱敏展示判断）。
    fn can_decrypt_password(&self, ciphertext: &str) -> bool;
    /// 凭据解密是否曾失败（初始化向导提示）。
    fn has_decryption_error(&self) -> bool;
    /// 返回项目根目录。
    fn base_path(&self) -> std::path::PathBuf;
    /// 返回当前运行时配置快照（无锁读，Arc 共享免深拷贝）。
    fn runtime_snapshot(&self) -> std::sync::Arc<RuntimeConfig>;
}

#[async_trait::async_trait]
impl ConfigApi for ConfigService {
    async fn load_settings_async(&self) -> SettingsData {
        ConfigService::load_settings_async(self).await
    }

    async fn save_settings(&self, data: &SettingsData) -> Result<(), ConfigError> {
        ConfigService::save_settings(self, data).await
    }

    fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
        ConfigService::load_profile(self, id)
    }

    async fn save_profile(&self, profile: &ProfileData) -> Result<(), ConfigError> {
        ConfigService::save_profile(self, profile).await
    }

    async fn reload(&self) -> Result<(), ConfigError> {
        ConfigService::reload(self).await
    }

    fn can_decrypt_password(&self, ciphertext: &str) -> bool {
        ConfigService::can_decrypt_password(self, ciphertext)
    }

    fn has_decryption_error(&self) -> bool {
        ConfigService::has_decryption_error(self)
    }

    fn base_path(&self) -> std::path::PathBuf {
        ConfigService::base_path(self)
    }

    fn runtime_snapshot(&self) -> std::sync::Arc<RuntimeConfig> {
        ConfigService::runtime_snapshot(self)
    }
}
