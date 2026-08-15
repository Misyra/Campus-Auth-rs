//! ConfigService：配置读写、原子写入、mtime 缓存、ArcSwap 无锁快照
//!
//! 持有 [`RuntimeConfig`] 的 `Arc<ArcSwap<>>` 无锁快照，所有写操作通过
//! `tokio::sync::Mutex` 串行化。配置变更通过 mpsc 通道发送 [`ConfigReloadSignal`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use arc_swap::ArcSwap;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc::Sender;

use crate::config::crypto::PasswordCrypto;
use crate::config::migration::run_migrations;
use crate::config::runtime::{build_runtime_config, ConfigReloadSignal, RuntimeConfig};
use crate::config::schema::{ProfileData, SettingsData};
use crate::utils::recover_lock;

/// 配置错误类型
#[derive(Debug, Error)]
pub enum ConfigError {
    /// 文件 IO 错误
    #[error("文件 IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// JSON 解析/序列化错误
    #[error("JSON 错误: {0}")]
    JsonParse(#[from] serde_json::Error),
    /// 配置文件不存在
    #[error("配置文件不存在: {path}")]
    ConfigNotFound {
        /// 缺失的文件路径
        path: String,
    },
    /// 配置解析失败，已备份
    #[error("配置解析失败: {path}{}", backup_path.as_ref().map(|p| format!("（已备份至 {p}）")).unwrap_or_default())]
    ConfigParseError {
        /// 出错的文件路径
        path: String,
        /// 备份文件路径（如有）
        backup_path: Option<String>,
    },
    /// 配置写入失败
    #[error("配置写入失败: {reason}")]
    ConfigWriteError {
        /// 失败原因
        reason: String,
    },
    /// 密码解密失败
    #[error("密码解密失败 (profile: {profile_id})")]
    DecryptFailed {
        /// 关联的 Profile ID
        profile_id: String,
    },
    /// Profile 不存在
    #[error("Profile 不存在: {id}")]
    ProfileNotFound {
        /// 缺失的 Profile ID
        id: String,
    },
    /// Profile ID 冲突
    #[error("Profile ID 冲突: {id}")]
    ProfileIdConflict {
        /// 冲突的 Profile ID
        id: String,
    },
    /// 不允许删除 default Profile
    #[error("不允许删除 default Profile")]
    CannotDeleteDefault,
    /// 迁移失败
    #[error("配置迁移失败 (v{from}→v{to}): {reason}")]
    MigrationFailed {
        /// 源版本
        from: u32,
        /// 目标版本
        to: u32,
        /// 失败原因
        reason: String,
    },
    /// 配置版本高于代码支持（版本降级保护）
    #[error("配置版本 {version} 高于支持的最高版本 {max}")]
    VersionTooHigh {
        /// 配置文件中的版本号
        version: u32,
        /// 代码支持的最高版本号
        max: u32,
    },
}

/// settings.json 内存缓存 + mtime
#[derive(Default)]
struct SettingsCache {
    /// 缓存的配置数据
    data: Option<SettingsData>,
    /// 上次读取时的文件修改时间
    mtime: Option<SystemTime>,
    /// 磁盘文件损坏且无缓存可用（隔离态：拒绝保存，防止默认值覆盖用户配置）
    poisoned: bool,
}

/// 单个 Profile 的内存缓存 + mtime
struct ProfileCache {
    /// 缓存的 Profile 数据
    data: ProfileData,
    /// 上次读取时的文件修改时间
    mtime: SystemTime,
}

/// 配置服务：读写 settings.json 与 profiles/*.json，管理无锁 RuntimeConfig 快照
pub struct ConfigService {
    /// 项目根目录（exe 所在目录）
    base_path: PathBuf,
    /// 配置目录（base_path / "config"）
    config_dir: PathBuf,
    /// settings.json 路径
    settings_path: PathBuf,
    /// profiles 目录
    profiles_dir: PathBuf,
    /// 运行时配置无锁快照
    runtime: Arc<ArcSwap<RuntimeConfig>>,
    /// 串行化所有 save() 调用
    save_mutex: tokio::sync::Mutex<()>,
    /// settings.json 内存缓存
    settings_cache: Mutex<SettingsCache>,
    /// Profile 文件内存缓存
    profile_cache: Mutex<HashMap<String, ProfileCache>>,
    /// 密码加解密器
    crypto: PasswordCrypto,
    /// 配置变更通知通道（仅发信号，不传内容）
    reload_tx: Sender<ConfigReloadSignal>,
}

impl ConfigService {
    /// 创建实例：确保目录、加载/迁移配置、构建 RuntimeConfig
    pub async fn new(
        base_path: PathBuf,
        reload_tx: Sender<ConfigReloadSignal>,
    ) -> Result<Self, ConfigError> {
        // 同步文件 I/O（建目录/清理 tmp/读配置/迁移/解密）移入 spawn_blocking，
        // 避免在 async 构造函数内直接阻塞 tokio worker 线程。
        tokio::task::spawn_blocking(move || Self::new_sync(base_path, reload_tx))
            .await
            .map_err(|e| {
                ConfigError::Io(std::io::Error::other(format!("配置初始化任务失败: {e}")))
            })?
    }

    /// `new` 的同步实现：全部磁盘 I/O 与解密在此线程完成。
    fn new_sync(
        base_path: PathBuf,
        reload_tx: Sender<ConfigReloadSignal>,
    ) -> Result<Self, ConfigError> {
        let config_dir = base_path.join(crate::config::CONFIG_DIR);
        let settings_path = config_dir.join(crate::config::SETTINGS_FILE);
        let profiles_dir = config_dir.join(crate::config::PROFILES_DIR);
        let key_path = crate::config::crypto::default_key_path();
        let crypto = PasswordCrypto::new(key_path.clone());

        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&profiles_dir)?;
        if let Some(p) = key_path.parent() {
            let _ = std::fs::create_dir_all(p);
        }

        // 清理上次崩溃残留的 .tmp 文件
        if let Ok(entries) = std::fs::read_dir(&config_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "tmp").unwrap_or(false) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }

        // 读取并（必要时）迁移 settings
        let settings = Self::load_or_init_settings(&settings_path, &config_dir)?;

        // 确保 default profile 存在
        let default_path = profiles_dir.join("default.json");
        if !default_path.exists() {
            let default = ProfileData::default();
            let json = serde_json::to_string_pretty(&default)?;
            std::fs::write(&default_path, json)?;
        }

        // 解析活跃 Profile（缺失则 fallback 到 default）
        let active_id = if settings.active_profile_id.is_empty() {
            "default".to_string()
        } else {
            settings.active_profile_id.clone()
        };
        let active_profile = match Self::read_profile_file(&profiles_dir.join(format!("{active_id}.json"))) {
            Ok(p) => p,
            Err(_) => Self::read_profile_file(&default_path)
                .unwrap_or_else(|_| ProfileData::default()),
        };

        let runtime = build_runtime_config(&settings, &active_profile, &crypto)?;

        let settings_mtime = std::fs::metadata(&settings_path)
            .ok()
            .and_then(|m| m.modified().ok());

        let service = Self {
            base_path,
            config_dir,
            settings_path,
            profiles_dir,
            runtime: Arc::new(ArcSwap::new(Arc::new(runtime))),
            save_mutex: tokio::sync::Mutex::new(()),
            settings_cache: Mutex::new(SettingsCache {
                data: Some(settings.clone()),
                mtime: settings_mtime,
                poisoned: false,
            }),
            profile_cache: Mutex::new(HashMap::new()),
            crypto,
            reload_tx,
        };
        Ok(service)
    }

    /// 获取 RuntimeConfig 无锁快照引用
    pub fn runtime(&self) -> &Arc<ArcSwap<RuntimeConfig>> {
        &self.runtime
    }

    /// 为指定 Profile 构建运行时快照（含解密后的密码）
    ///
    /// 用于多 Profile 场景下的定时浏览器任务使用各自独立的账号凭据。
    /// 找不到指定 Profile 时透传 [`ConfigError::ProfileNotFound`]。
    pub fn runtime_config_for_profile(&self, id: &str) -> Result<RuntimeConfig, ConfigError> {
        let profile = self.load_profile(id)?;
        let settings = self.load_settings();
        build_runtime_config(&settings, &profile, &self.crypto)
    }

    /// 返回项目根目录（exe 所在目录）
    pub fn base_path(&self) -> PathBuf {
        self.base_path.clone()
    }

    /// 加密明文密码（委托 [`PasswordCrypto`]）
    pub fn encrypt_password(&self, raw: &str) -> Result<String, ConfigError> {
        self.crypto.encrypt(raw)
    }

    /// 凭据解密是否曾失败（用于初始化向导提示）
    pub fn has_decryption_error(&self) -> bool {
        self.crypto.has_decryption_error()
    }

    /// 加载 settings.json（mtime 缓存）
    pub fn load_settings(&self) -> SettingsData {
        let mtime = std::fs::metadata(&self.settings_path)
            .ok()
            .and_then(|m| m.modified().ok());
        {
            let cache = self.settings_cache.lock().unwrap_or_else(recover_lock);
            if let (Some(data), Some(mt)) = (&cache.data, &cache.mtime) {
                if Some(*mt) == mtime {
                    return data.clone();
                }
            }
        }
        match Self::read_settings_from_disk(&self.settings_path) {
            Ok(s) => {
                let mut c = self.settings_cache.lock().unwrap_or_else(recover_lock);
                c.data = Some(s.clone());
                c.mtime = mtime;
                // 文件恢复可解析（可能被外部修复），解除隔离
                c.poisoned = false;
                s
            }
            Err(e) => {
                // 损坏时绝不静默返回默认值：调用方随后的"读→改→存"会用
                // 默认值原子覆盖用户的原始配置，造成配置数据丢失。
                // 有缓存则沿用旧快照；无缓存则进入隔离态并拒绝后续保存。
                let mut c = self.settings_cache.lock().unwrap_or_else(recover_lock);
                if let Some(prev) = c.data.clone() {
                    tracing::warn!("settings.json 解析失败，沿用内存缓存: {e}");
                    return prev;
                }
                tracing::error!("settings.json 解析失败且无缓存可用，进入隔离态（保存将被拒绝）: {e}");
                c.poisoned = true;
                SettingsData::default()
            }
        }
    }

    /// 原子写入 settings.json
    pub async fn save_settings(&self, data: &SettingsData) -> Result<(), ConfigError> {
        // 隔离态拒绝保存：防止基于降级默认值的修改覆盖损坏前的用户配置
        if self.settings_cache.lock().unwrap_or_else(recover_lock).poisoned {
            return Err(ConfigError::ConfigWriteError {
                reason: "settings.json 损坏（无可用缓存），已拒绝保存以保护原配置；请修复或恢复备份后重启".into(),
            });
        }
        let _guard = self.save_mutex.lock().await;
        Self::atomic_write_json(&self.settings_path, data).await?;
        let mtime = std::fs::metadata(&self.settings_path)
            .ok()
            .and_then(|m| m.modified().ok());
        let mut c = self.settings_cache.lock().unwrap_or_else(recover_lock);
        c.data = Some(data.clone());
        c.mtime = mtime;
        Ok(())
    }

    /// 加载单个 Profile（mtime 缓存）
    pub fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
        let path = self.profiles_dir.join(format!("{id}.json"));
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        {
            let cache = self.profile_cache.lock().unwrap_or_else(recover_lock);
            if let Some(pc) = cache.get(id) {
                if Some(pc.mtime) == mtime {
                    return Ok(pc.data.clone());
                }
            }
        }
        let p = Self::read_profile_file(&path)?;
        let mut cache = self.profile_cache.lock().unwrap_or_else(recover_lock);
        cache.insert(
            id.to_string(),
            ProfileCache {
                data: p.clone(),
                mtime: mtime.unwrap_or_else(SystemTime::now),
            },
        );
        Ok(p)
    }

    /// 检查给定密文是否能被当前密钥成功解密（用于前端 has_password 判断）
    pub fn can_decrypt_password(&self, ciphertext: &str) -> bool {
        self.crypto.decrypt_to_zeroizing(ciphertext).is_ok()
    }

    /// 加载所有 Profile 文件（解析失败跳过并记 WARN）
    pub fn load_all_profiles(&self) -> Vec<ProfileData> {
        let mut result = Vec::new();
        let entries = match std::fs::read_dir(&self.profiles_dir) {
            Ok(e) => e,
            Err(_) => return result,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                match Self::read_profile_file(&path) {
                    Ok(p) => result.push(p),
                    Err(e) => tracing::warn!("跳过解析失败的 Profile 文件 {:?}: {e}", path),
                }
            }
        }
        result
    }

    /// 原子写入 Profile 文件
    pub async fn save_profile(&self, profile: &ProfileData) -> Result<(), ConfigError> {
        let _guard = self.save_mutex.lock().await;
        let path = self.profiles_dir.join(format!("{}.json", profile.id));
        Self::atomic_write_json(&path, profile).await?;
        let mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or_else(SystemTime::now);
        let mut cache = self.profile_cache.lock().unwrap_or_else(recover_lock);
        cache.insert(
            profile.id.clone(),
            ProfileCache {
                data: profile.clone(),
                mtime,
            },
        );
        Ok(())
    }

    /// 安全删除 Profile（移至 .trash/，不允许删除 default）
    pub async fn delete_profile(&self, id: &str) -> Result<(), ConfigError> {
        if id == "default" {
            return Err(ConfigError::CannotDeleteDefault);
        }
        let _guard = self.save_mutex.lock().await;
        let path = self.profiles_dir.join(format!("{id}.json"));
        if !path.exists() {
            return Err(ConfigError::ProfileNotFound { id: id.to_string() });
        }
        let trash_dir = self.config_dir.join(crate::config::TRASH_DIR);
        std::fs::create_dir_all(&trash_dir)?;
        let trash_path = trash_dir.join(format!("{id}.json.{}", timestamp()));
        std::fs::rename(&path, &trash_path)?;
        self.profile_cache.lock().unwrap_or_else(recover_lock).remove(id);
        Ok(())
    }

    /// 重新读取配置 + 构建 RuntimeConfig + 原子替换 + 发通知（默认 `GlobalChanged`）
    pub async fn reload(&self) -> Result<(), ConfigError> {
        self.reload_with_signal(ConfigReloadSignal::GlobalChanged)
            .await
    }

    /// 重新读取配置并发送指定变更信号
    ///
    /// 供仅改了某个子集的调用方使用（如切换 Profile），避免调度器对无关变更做全量任务重载。
    pub async fn reload_with_signal(&self, signal: ConfigReloadSignal) -> Result<(), ConfigError> {
        self.reload_inner(signal).await
    }

    /// reload 的内部实现：与写路径（save_settings / save_profile / delete_profile）互斥
    async fn reload_inner(&self, signal: ConfigReloadSignal) -> Result<(), ConfigError> {
        // settings 与 profile 是两步独立读取，若并发写插入其间会产生混合快照
        // （新全局配置 + 旧 Profile，或反之）。持锁读完即释放，不阻塞后续写（历史遗留 M8）。
        let _guard = self.save_mutex.lock().await;
        // 保存旧快照，用于比较非热更字段
        let old = self.runtime.load().as_ref().clone();
        // 强制绕过 mtime 缓存
        self.settings_cache.lock().unwrap_or_else(recover_lock).data = None;
        let settings = self.load_settings();
        let active_id = settings.active_profile_id.clone();
        let active_profile = self
            .load_profile(&active_id)
            .unwrap_or_else(|_| ProfileData::default());
        self.build_and_swap_runtime(&settings, &active_profile).await?;
        // 非热更字段变更提示（如端口、运行模式等）
        Self::log_non_hot_reload_changes(&old, self.runtime.load().as_ref());
        let _ = self.reload_tx.send(signal).await;
        Ok(())
    }

    /// 比较新旧运行时配置，对非热更字段的变更记录 WARN 提示需重启
    ///
    /// 这些字段在程序启动期即被消费（监听端口、运行模式、Worker 空闲超时），
    /// 运行期无法热替换，需重启生效。
    fn log_non_hot_reload_changes(old: &RuntimeConfig, new: &RuntimeConfig) {
        let mut changed = Vec::new();
        if old.app.port != new.app.port {
            changed.push("app.port");
        }
        if old.worker.idle_timeout_seconds != new.worker.idle_timeout_seconds {
            changed.push("worker.idle_timeout_seconds");
        }
        if old.app.runtime_mode != new.app.runtime_mode {
            changed.push("app.runtime_mode");
        }
        if !changed.is_empty() {
            tracing::warn!(
                "以下配置项不支持热更新，需重启程序后生效: {}",
                changed.join(", ")
            );
        }
    }

    /// 构建 RuntimeConfig 并原子替换
    pub async fn build_and_swap_runtime(
        &self,
        settings: &SettingsData,
        active_profile: &ProfileData,
    ) -> Result<(), ConfigError> {
        let rc = build_runtime_config(settings, active_profile, &self.crypto)?;
        self.runtime.store(Arc::new(rc));
        Ok(())
    }

    /// 读取并（必要时）迁移 settings.json，返回 SettingsData
    fn load_or_init_settings(
        settings_path: &Path,
        config_dir: &Path,
    ) -> Result<SettingsData, ConfigError> {
        if !settings_path.exists() {
            return Ok(SettingsData::default());
        }
        let raw = std::fs::read_to_string(settings_path)?;
        let mut value: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                let backup = config_dir.join(format!(
                    "{}{}.json",
                    crate::config::CORRUPT_PREFIX,
                    timestamp()
                ));
                let _ = std::fs::rename(settings_path, &backup);
                return Ok(SettingsData::default());
            }
        };

        let version = value
            .get("config_version")
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32;
        if version > crate::config::CURRENT_CONFIG_VERSION {
            return Err(ConfigError::VersionTooHigh {
                version,
                max: crate::config::CURRENT_CONFIG_VERSION,
            });
        }
        if version < crate::config::CURRENT_CONFIG_VERSION {
            run_migrations(config_dir, &mut value).map_err(|e| ConfigError::MigrationFailed {
                from: version,
                to: crate::config::CURRENT_CONFIG_VERSION,
                reason: e.to_string(),
            })?;
            // commit point：写回迁移后的 settings（同步 fsync，防止掉电导致版本回退重跑迁移）
            let json = serde_json::to_string_pretty(&value)?;
            Self::atomic_write_json_sync(settings_path, &json)?;
        }

        serde_json::from_value(value).map_err(|_| ConfigError::ConfigParseError {
            path: settings_path.to_string_lossy().to_string(),
            backup_path: None,
        })
    }

    /// 从磁盘直接读取 settings.json（不做迁移，假设为当前版本）
    fn read_settings_from_disk(path: &Path) -> Result<SettingsData, ConfigError> {
        if !path.exists() {
            return Ok(SettingsData::default());
        }
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).map_err(|_| {
            ConfigError::ConfigParseError {
                path: path.to_string_lossy().to_string(),
                backup_path: None,
            }
        })
    }

    /// 从磁盘读取单个 Profile 文件
    fn read_profile_file(path: &Path) -> Result<ProfileData, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::ProfileNotFound {
                id: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
            });
        }
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).map_err(|_| ConfigError::ConfigParseError {
            path: path.to_string_lossy().to_string(),
            backup_path: None,
        })
    }

    /// 同步原子写入字符串到文件（tmp + sync_all + rename + 父目录 fsync）
    ///
    /// 供同步上下文中需要持久化保证的写回使用（如配置迁移写回），
    /// 与 [`Self::atomic_write_json`] 保证一致。
    fn atomic_write_json_sync(path: &Path, content: &str) -> Result<(), ConfigError> {
        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file =
                std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        // 父目录 fsync（确保目录项持久化）
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    /// 原子写入 JSON 文件（tmp + sync_all + rename + 父目录 fsync）
    async fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), ConfigError> {
        let json = serde_json::to_vec_pretty(value)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .await?;
            use tokio::io::AsyncWriteExt;
            file.write_all(&json).await?;
            file.sync_all().await?;
            // macOS 下 sync_all 仅等价于 fsync，需 F_FULLFSYNC 才能刷写至物理介质
            #[cfg(target_os = "macos")]
            {
                use std::os::unix::io::AsRawFd;
                let fd = file.as_raw_fd();
                let rc = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC, 0) };
                if rc != 0 {
                    tracing::warn!("F_FULLFSYNC 失败（rc={rc}），已退化为 sync_all");
                }
            }
        }
        tokio::fs::rename(&tmp, path).await?;
        // 父目录 fsync（确保目录项持久化）
        if let Some(parent) = path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                let _ = dir.sync_all().await;
            }
        }
        Ok(())
    }
}

/// 生成紧凑时间戳字符串（用于备份/垃圾文件名）
fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::GlobalConfig;

    // ============ ConfigError Display 测试 ============

    #[test]
    fn test_config_parse_error_display_without_backup() {
        // 测试 ConfigParseError 不含 backup_path 时的 Display 输出
        let err = ConfigError::ConfigParseError {
            path: "config/settings.json".to_string(),
            backup_path: None,
        };
        let display = format!("{err}");
        assert!(display.contains("config/settings.json"));
        assert!(!display.contains("已备份"));
    }

    #[test]
    fn test_config_parse_error_display_with_backup() {
        // 测试 ConfigParseError 含 backup_path 时的 Display 输出
        let err = ConfigError::ConfigParseError {
            path: "config/settings.json".to_string(),
            backup_path: Some("config/.trash/backup.json".to_string()),
        };
        let display = format!("{err}");
        assert!(display.contains("config/settings.json"));
        assert!(display.contains("已备份"));
        assert!(display.contains("config/.trash/backup.json"));
    }

    #[test]
    fn test_config_not_found_display() {
        let err = ConfigError::ConfigNotFound {
            path: "/missing/path".to_string(),
        };
        assert!(format!("{err}").contains("/missing/path"));
    }

    #[test]
    fn test_cannot_delete_default_display() {
        let err = ConfigError::CannotDeleteDefault;
        assert!(format!("{err}").contains("default"));
    }

    #[test]
    fn test_version_too_high_display() {
        let err = ConfigError::VersionTooHigh {
            version: 10,
            max: 6,
        };
        let display = format!("{err}");
        assert!(display.contains("10"));
        assert!(display.contains("6"));
    }

    // ============ settings.json 读写往返测试 ============

    #[test]
    fn test_settings_data_serde_roundtrip() {
        // 测试 SettingsData 序列化后可完整反序列化回来
        let settings = SettingsData {
            config_version: 6,
            active_profile_id: "test-profile".to_string(),
            auto_switch: false,
            global: GlobalConfig::default(),
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let back: SettingsData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.config_version, 6);
        assert_eq!(back.active_profile_id, "test-profile");
        assert!(!back.auto_switch);
    }

    #[test]
    fn test_settings_data_default_values() {
        // 测试 SettingsData 默认值的正确性
        let settings = SettingsData::default();
        assert_eq!(settings.config_version, crate::config::CURRENT_CONFIG_VERSION);
        assert_eq!(settings.active_profile_id, "default");
        assert!(settings.auto_switch);
    }

    #[test]
    fn test_settings_data_partial_json_fills_defaults() {
        // 测试缺失字段自动填充默认值
        let json = r#"{"active_profile_id": "custom"}"#;
        let settings: SettingsData = serde_json::from_str(json).unwrap();
        assert_eq!(settings.active_profile_id, "custom");
        assert_eq!(settings.config_version, crate::config::CURRENT_CONFIG_VERSION);
        assert!(settings.auto_switch);
    }

    // ============ 配置迁移 v5→v6 测试 ============

    #[test]
    fn test_migration_v5_to_v6_renames_fields() {
        // 测试 v5→v6 迁移能正确重命名字段并拆分 profiles
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();

        let v5_json = r#"{
            "config_version": 5,
            "active_profile_id": "default",
            "profiles": {
                "default": {
                    "name": "默认",
                    "username": "test",
                    "password": "",
                    "carrier": "移动",
                    "match_gateway_ip": "10.0.0.1",
                    "match_ssid": "MyWiFi"
                }
            },
            "global": {
                "monitor": {
                    "check_interval_seconds": 60,
                    "enable_tcp_check": true,
                    "enable_http_check": false,
                    "ping_targets": ["8.8.8.8:53"],
                    "test_urls": ["https://example.com"],
                    "url_check_urls": ["https://captive.apple.com"]
                },
                "logging": {
                    "log_retention_days": 7
                },
                "app": {
                    "app_port": 50721,
                    "auto_open_browser": true
                }
            }
        }"#;

        let mut value: Value = serde_json::from_str(v5_json).unwrap();
        let new_version = crate::config::migration::run_migrations(config_dir, &mut value).unwrap();
        assert_eq!(new_version, 6);

        // 验证全局字段重命名发生在正确的子段内，且旧字段名已移除
        let monitor = &value["global"]["monitor"];
        assert_eq!(monitor["check_interval"].as_u64().unwrap(), 60);
        assert!(monitor["tcp_enabled"].as_bool().unwrap());
        assert!(!monitor["http_enabled"].as_bool().unwrap());
        assert_eq!(monitor["tcp_targets"][0].as_str().unwrap(), "8.8.8.8:53");
        assert_eq!(monitor["http_targets"][0].as_str().unwrap(), "https://example.com");
        assert_eq!(monitor["url_targets"][0].as_str().unwrap(), "https://captive.apple.com");
        assert!(monitor.get("check_interval_seconds").is_none());
        assert!(monitor.get("enable_tcp_check").is_none());
        assert!(monitor.get("ping_targets").is_none());

        let logging = &value["global"]["logging"];
        assert_eq!(logging["retention_days"].as_u64().unwrap(), 7);
        assert!(logging.get("log_retention_days").is_none());

        let app = &value["global"]["app"];
        assert_eq!(app["port"].as_u64().unwrap(), 50721);
        assert!(app["auto_start_browser"].as_bool().unwrap());
        assert!(app.get("app_port").is_none());
        assert!(app.get("auto_open_browser").is_none());

        // 验证 profiles 已拆分为独立文件
        let profile_path = config_dir.join("profiles").join("default.json");
        assert!(profile_path.exists());
        let profile_content = std::fs::read_to_string(&profile_path).unwrap();
        let profile: Value = serde_json::from_str(&profile_content).unwrap();
        assert_eq!(profile["isp"].as_str().unwrap(), "移动");
        assert_eq!(profile["gateway_ip"].as_str().unwrap(), "10.0.0.1");
        assert_eq!(profile["wifi_ssid"].as_str().unwrap(), "MyWiFi");
        assert!(profile.get("carrier").is_none());
        assert!(profile.get("match_gateway_ip").is_none());

        // 验证 settings 中 profiles 已移除，版本已更新
        assert!(value.get("profiles").is_none());
        assert_eq!(value["config_version"].as_u64().unwrap(), 6);
    }

    #[test]
    fn test_migration_idempotent_on_v6() {
        // 测试已是 v6 的配置不触发迁移
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path();

        let v6_json = r#"{"config_version": 6, "active_profile_id": "default"}"#;
        let mut value: Value = serde_json::from_str(v6_json).unwrap();
        let new_version = crate::config::migration::run_migrations(config_dir, &mut value).unwrap();
        assert_eq!(new_version, 6);
    }
}
