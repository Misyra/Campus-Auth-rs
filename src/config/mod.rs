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
/// 加密密钥文件名（相对于 AUTH_DATA_DIR）
pub const ENC_KEY_FILE: &str = ".enc_key.rs";
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
