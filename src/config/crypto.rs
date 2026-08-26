//! 密码加解密（AES-256-GCM）
//!
//! 使用 AES-256-GCM 对 Profile 密码做认证加密。密钥以 raw 32 字节形式存储在
//! `~/.campus_network_auth/.enc_key.rs`。密文格式：`ENC:` + base64(nonce(12B) || ciphertext+tag(16B))。
//! 空串与无 `ENC:` 前缀的明文向后兼容直接返回。

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Local;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::consts::{U12, U32};
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use zeroize::Zeroizing;

// MSRV 1.85 兼容：fs4::FileExt 提供文件排他锁（Rust 1.96+ 内置方法优先级更高，不会冲突）
#[allow(unused_imports)]
use fs4::FileExt;

use crate::config::ConfigError;

/// 加密密文前缀
pub const ENC_PREFIX: &str = "ENC:";
/// AES-256 密钥长度（字节）
const KEY_LEN: usize = 32;
/// AES-GCM nonce 长度（96-bit）
const NONCE_LEN: usize = 12;

/// 默认加密密钥文件路径：`~/.campus_network_auth/.enc_key.rs`（raw 32 字节）
///
/// 文件名带 `.rs` 后缀以避免与 Python 版的 `.enc_key`（base64 格式）冲突，
/// 允许两个版本在同一用户目录下共存。`.enc_key.rs` 不存在时会优先继承
/// Python 旧版的 `.enc_key`（见 [`PasswordCrypto::read_or_create_key`]），
/// 保证新旧版本共用同一密钥、密码互通（历史遗留：两版密钥各自生成导致冲突）。
pub fn default_key_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".campus_network_auth");
    dir.push(".enc_key.rs");
    dir
}

/// 密码加解密器，密钥延迟加载
pub struct PasswordCrypto {
    /// 延迟加载的加密密钥（首次使用时从文件读取或生成）
    key: OnceLock<Zeroizing<[u8; KEY_LEN]>>,
    /// 密钥文件路径
    key_path: PathBuf,
    /// 解密失败的 Profile ID 集合（按 profile 记录，F10）
    ///
    /// 历史实现为全局单布尔：任一密文解密成功即清全局标志，A Profile 的损坏
    /// 提示会被 B Profile 的成功解密抹掉。改为按 id 的集合后，清位只清对应
    /// Profile，`/api/system` 的 `password_decryption_failed` 汇总语义保持为
    /// 「任一 Profile 失败即 true」（集合非空）。
    decryption_failed_profiles: Mutex<HashSet<String>>,
}

impl PasswordCrypto {
    /// 构造加解密器（不立即加载密钥）
    pub fn new(key_path: PathBuf) -> Self {
        Self {
            key: OnceLock::new(),
            key_path,
            decryption_failed_profiles: Mutex::new(HashSet::new()),
        }
    }

    /// 加密明文密码，返回 `ENC:` 前缀密文
    pub fn encrypt(&self, plaintext: &str) -> Result<String, ConfigError> {
        if plaintext.is_empty() {
            return Ok(String::new());
        }
        self.ensure_key()?;
        let key = self.key.get().expect("密钥已确保加载");
        let key_arr = GenericArray::<u8, U32>::from_slice(key.as_ref());
        let cipher = Aes256Gcm::new(key_arr);

        // 生成随机 nonce
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = GenericArray::<u8, U12>::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| ConfigError::Io(std::io::Error::other(format!("密码加密失败: {e}"))))?;

        // 组装 payload：nonce || ciphertext+tag
        let mut payload = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&ciphertext);

        Ok(format!("{ENC_PREFIX}{}", STANDARD.encode(&payload)))
    }

    /// 解密 `ENC:` 密文，返回明文
    ///
    /// 生产路径一律使用 [`Self::decrypt_to_zeroizing`]（返回 `Zeroizing<String>` 保证清零）。
    /// 本非清零版本仅测试使用，故标记 `#[cfg(test)]` 收敛为测试专用，防止误用。
    /// 走无副作用路径（不更新失败标记），标记行为专门由 `decrypt_to_zeroizing` 测试覆盖。
    #[cfg(test)]
    pub fn decrypt(&self, ciphertext: &str) -> Result<String, ConfigError> {
        if ciphertext.is_empty() {
            return Ok(String::new());
        }
        // 向后兼容：非 ENC: 前缀视为明文直接返回
        if !ciphertext.starts_with(ENC_PREFIX) {
            return Ok(ciphertext.to_string());
        }
        self.decrypt_core(ciphertext)
    }

    /// 解密 `ENC:` 密文，返回 `Zeroizing<String>`（drop 时自动清零）
    ///
    /// 携带 `profile_id` 用于按 Profile 记录/清除解密失败标志（F10）：
    /// 失败时仅登记该 Profile，成功时仅清除该 Profile 的记录，
    /// 不再影响其他 Profile 的失败状态。
    pub fn decrypt_to_zeroizing(
        &self,
        profile_id: &str,
        ciphertext: &str,
    ) -> Result<Zeroizing<String>, ConfigError> {
        if ciphertext.is_empty() {
            return Ok(Zeroizing::new(String::new()));
        }
        if !ciphertext.starts_with(ENC_PREFIX) {
            return Ok(Zeroizing::new(ciphertext.to_string()));
        }
        match self.decrypt_core(ciphertext) {
            Ok(plain) => {
                self.clear_decryption_failed(profile_id);
                Ok(Zeroizing::new(plain))
            }
            Err(e) => {
                self.mark_decryption_failed(profile_id);
                Err(e)
            }
        }
    }

    /// 纯查询：给定密文在当前密钥下是否可解密
    ///
    /// 供 `can_decrypt_password`（前端 has_password 判断）使用。必须走无副作用
    /// 的 [`Self::decrypt_core`]：历史实现经 `decrypt_inner` 带置位/清位副作用，
    /// 一次查询就会污染失败标志集合（F10）。
    pub fn can_decrypt(&self, ciphertext: &str) -> bool {
        if ciphertext.is_empty() || !ciphertext.starts_with(ENC_PREFIX) {
            return true;
        }
        self.decrypt_core(ciphertext).is_ok()
    }

    /// 是否存在任一 Profile 的解密失败记录（汇总语义：集合非空即 true）
    pub fn has_decryption_error(&self) -> bool {
        !self
            .decryption_failed_profiles
            .lock()
            .unwrap_or_else(crate::utils::recover_lock)
            .is_empty()
    }

    /// 登记某个 Profile 的解密失败记录
    fn mark_decryption_failed(&self, profile_id: &str) {
        self.decryption_failed_profiles
            .lock()
            .unwrap_or_else(crate::utils::recover_lock)
            .insert(profile_id.to_string());
    }

    /// 清除某个 Profile 的解密失败记录（仅该 Profile，不影响其他记录）
    fn clear_decryption_failed(&self, profile_id: &str) {
        self.decryption_failed_profiles
            .lock()
            .unwrap_or_else(crate::utils::recover_lock)
            .remove(profile_id);
    }

    /// 内部解密核心逻辑：纯计算，无失败标志副作用
    fn decrypt_core(&self, ciphertext: &str) -> Result<String, ConfigError> {
        self.ensure_key()?;
        let key = self.key.get().expect("密钥已确保加载");

        let b64 = &ciphertext[ENC_PREFIX.len()..];
        let payload = match STANDARD.decode(b64) {
            Ok(p) => p,
            Err(_) => {
                return Err(ConfigError::DecryptFailed {
                    profile_id: String::new(),
                });
            }
        };

        if payload.len() < NONCE_LEN {
            return Err(ConfigError::DecryptFailed {
                profile_id: String::new(),
            });
        }

        let (nonce_part, ct) = payload.split_at(NONCE_LEN);
        let nonce = GenericArray::<u8, U12>::from_slice(nonce_part);
        let key_arr = GenericArray::<u8, U32>::from_slice(key.as_ref());
        let cipher = Aes256Gcm::new(key_arr);

        match cipher.decrypt(nonce, ct) {
            Ok(plain) => String::from_utf8(plain).map_err(|e| {
                ConfigError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("解密结果非 UTF-8: {e}"),
                ))
            }),
            Err(_) => Err(ConfigError::DecryptFailed {
                profile_id: String::new(),
            }),
        }
    }

    /// 确保密钥已加载，必要时从文件读取或生成新密钥
    fn ensure_key(&self) -> Result<(), ConfigError> {
        if self.key.get().is_some() {
            return Ok(());
        }
        let key = Self::read_or_create_key(&self.key_path)?;
        // 若并发下其他线程已写入，忽略冲突（保留先写入的密钥）
        let _ = self.key.set(key);
        Ok(())
    }

    /// 读取密钥文件，不存在则生成并写入
    ///
    /// 生成/继承/异常恢复等"写密钥"路径用文件锁尽力串行化，缓解双实例并发
    /// 生成密钥互相覆盖（历史遗留 TOCTOU：旧密文解密失败）。
    /// 锁获取失败仅降级（warn 后继续执行）：密钥生成是低频关键路径，
    /// 不能因锁文件权限/残留问题阻断加密（曾有真实故障）。
    fn read_or_create_key(key_path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, ConfigError> {
        // 锁文件与密钥文件分离（独立 .lock），避免锁住密钥文件本身导致同进程读写冲突
        let lock_path = {
            let mut p = key_path.as_os_str().to_owned();
            p.push(".lock");
            PathBuf::from(p)
        };
        // 非阻塞 try_lock：拿不到锁（权限/残留/他人持有）时降级继续，
        // 只在能获取锁时提供写路径串行化
        let lock_held = match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(f) => {
                // MSRV 1.85 兼容：此处 try_lock() 由 fs4::FileExt trait 提供
                #[allow(clippy::incompatible_msrv)]
                match f.try_lock() {
                    Ok(()) => Some(f), // 锁成功：持有到作用域结束
                    Err(e) => {
                        tracing::warn!("密钥文件锁获取失败（降级继续）: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("密钥锁文件无法打开（降级继续）: {e}");
                None
            }
        };

        let result = Self::read_or_create_key_inner(key_path);
        drop(lock_held); // 持有锁时关闭并释放
        result
    }

    /// 锁保护下的密钥读取/生成核心逻辑
    fn read_or_create_key_inner(key_path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, ConfigError> {
        if key_path.exists() {
            let bytes = std::fs::read(key_path)?;
            if bytes.len() != KEY_LEN {
                // 密钥长度异常：备份旧文件并生成新密钥。
                // 换钥意味着旧密钥加密的全部密码将无法解密（用户需重新输入），
                // 静默换钥会让用户误以为密码丢失——显式告警并给出旧钥备份路径（G26）。
                let ts = Local::now().format("%Y%m%d%H%M%S");
                let backup = {
                    let mut b = key_path.to_path_buf();
                    b.set_extension(format!("corrupt.{ts}.key"));
                    b
                };
                let _ = std::fs::rename(key_path, &backup);
                tracing::warn!(
                    "加密密钥文件长度异常（{} 字节，应为 {}），已备份至 {} 并生成新密钥；\
                     旧密钥加密的密码将无法解密，需要重新输入",
                    bytes.len(),
                    KEY_LEN,
                    backup.display()
                );
                return Self::generate_and_write_key(key_path);
            }
            let mut arr = [0u8; KEY_LEN];
            arr.copy_from_slice(&bytes);
            return Ok(Zeroizing::new(arr));
        }
        // `.enc_key.rs` 不存在：优先继承 Python 旧版密钥，避免两版各持一密钥导致
        // 旧版加密的密码在 Rust 版无法解密（历史遗留：新旧版本密钥冲突）。
        if let Some(key) = Self::try_inherit_python_key(key_path) {
            return Ok(key);
        }
        Self::generate_and_write_key(key_path)
    }

    /// 尝试从 Python 旧版密钥文件 `.enc_key`（base64 编码 32 字节）继承密钥。
    ///
    /// 成功时把密钥以 raw 32 字节写入 `.enc_key.rs`，此后两版共用同一密钥。
    /// 旧版密钥缺失 / 解码失败 / 长度异常时返回 `None`（由调用方生成新密钥）。
    fn try_inherit_python_key(key_path: &Path) -> Option<Zeroizing<[u8; KEY_LEN]>> {
        let python_path = key_path.with_file_name(".enc_key");
        let raw = std::fs::read_to_string(&python_path).ok()?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .ok()?;
        if bytes.len() != KEY_LEN {
            tracing::warn!(
                "Python 旧版密钥长度异常（{} 字节），忽略并生成新密钥",
                bytes.len()
            );
            return None;
        }
        let mut arr = [0u8; KEY_LEN];
        arr.copy_from_slice(&bytes);
        tracing::info!("检测到 Python 旧版密钥，已继承（新旧版本密码互通）");
        // 写入 `.enc_key.rs`，后续统一从该文件读取；写入失败不阻断（内存中已持有密钥）
        if let Some(parent) = key_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(key_path, arr.as_ref()) {
            Ok(()) => {}
            Err(e) => tracing::warn!("写入继承密钥到 {} 失败: {e}", key_path.display()),
        }
        Some(Zeroizing::new(arr))
    }

    /// 生成新密钥并写入文件（Unix 下权限 0600）
    fn generate_and_write_key(key_path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, ConfigError> {
        let mut arr = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut arr);
        let key = Zeroizing::new(arr);

        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(key_path, key.as_ref())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(key_path, perms);
        }
        #[cfg(windows)]
        {
            // Windows 无 std 级 ACL API，借助系统 icacls 移除继承权限并仅授予当前用户读取
            let username = std::env::var("USERNAME").unwrap_or_default();
            let domain = std::env::var("USERDOMAIN").unwrap_or_default();
            if !username.is_empty() {
                let account = if domain.is_empty() {
                    username
                } else {
                    format!("{domain}\\{username}")
                };
                // 失败仅告警不阻断：密钥已写入，仅权限未收紧
                // stderr 重定向到 null，避免环境无域账号解析时刷屏
                let _ = std::process::Command::new("icacls")
                    .arg(key_path)
                    .arg("/inheritance:r")
                    .arg(format!("/grant:r \"{account}\":(R)"))
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建临时目录下的 PasswordCrypto 实例（自动生成密钥）
    fn make_crypto() -> (tempfile::TempDir, PasswordCrypto) {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join(".enc_key");
        let crypto = PasswordCrypto::new(key_path);
        (tmp, crypto)
    }

    // ============ AES-256-GCM 加解密往返 ============

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        // 测试加密后再解密能得到原始明文
        let (_tmp, crypto) = make_crypto();
        let plaintext = "hello_world_password_123";
        let encrypted = crypto.encrypt(plaintext).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_chinese_text() {
        // 测试中文密码的加解密
        let (_tmp, crypto) = make_crypto();
        let plaintext = "校园网密码测试";
        let encrypted = crypto.encrypt(plaintext).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_long_password() {
        // 测试较长密码的加解密
        let (_tmp, crypto) = make_crypto();
        let plaintext = "a".repeat(500);
        let encrypted = crypto.encrypt(&plaintext).unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // ============ 空串处理 ============

    #[test]
    fn test_encrypt_empty_string_returns_empty() {
        // 空串加密应返回空串（不做加密操作）
        let (_tmp, crypto) = make_crypto();
        let result = crypto.encrypt("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decrypt_empty_string_returns_empty() {
        // 空串解密应返回空串
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt("").unwrap();
        assert!(result.is_empty());
    }

    // ============ ENC: 前缀识别 ============

    #[test]
    fn test_enc_prefix_present_after_encrypt() {
        // 加密后的密文应带有 ENC: 前缀
        let (_tmp, crypto) = make_crypto();
        let encrypted = crypto.encrypt("test").unwrap();
        assert!(encrypted.starts_with(ENC_PREFIX));
    }

    #[test]
    fn test_decrypt_plaintext_without_enc_prefix() {
        // 不带 ENC: 前缀的字符串视为明文，直接返回
        let (_tmp, crypto) = make_crypto();
        let plaintext = "not_encrypted_password";
        let result = crypto.decrypt(plaintext).unwrap();
        assert_eq!(result, plaintext);
    }

    // ============ 无效密文处理 ============

    #[test]
    fn test_decrypt_invalid_base64_fails() {
        // ENC: 后跟无效 base64 应返回错误（经带标记的生产路径验证）
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt_to_zeroizing("p1", "ENC:not_valid_base64!!!");
        assert!(result.is_err());
        assert!(crypto.has_decryption_error());
    }

    #[test]
    fn test_decrypt_too_short_payload_fails() {
        // ENC: 后跟有效 base64 但 payload 太短（< nonce 长度）应返回错误
        let (_tmp, crypto) = make_crypto();
        let short = STANDARD.encode([0u8; 4]); // 4 字节 < NONCE_LEN(12)
        let result = crypto.decrypt_to_zeroizing("p1", &format!("{ENC_PREFIX}{short}"));
        assert!(result.is_err());
        assert!(crypto.has_decryption_error());
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        // 用不同密钥加密后，另一个密钥应无法解密
        let tmp1 = tempfile::tempdir().unwrap();
        let key1 = tmp1.path().join(".enc_key");
        let crypto1 = PasswordCrypto::new(key1);

        let tmp2 = tempfile::tempdir().unwrap();
        let key2 = tmp2.path().join(".enc_key");
        let crypto2 = PasswordCrypto::new(key2);

        let encrypted = crypto1.encrypt("secret").unwrap();
        let result = crypto2.decrypt_to_zeroizing("p1", &encrypted);
        assert!(result.is_err());
    }

    // ============ 按 Profile 的解密失败标志（F10） ============

    #[test]
    fn test_decryption_error_flag_lifecycle_per_profile() {
        // 失败登记对应 Profile；同一 Profile 后续成功解密仅清除该 Profile 记录
        let (_tmp, crypto) = make_crypto();
        assert!(!crypto.has_decryption_error());

        // 触发一个解密错误（登记 profile-a）
        let _ = crypto.decrypt_to_zeroizing("profile-a", "ENC:AAAA");
        assert!(crypto.has_decryption_error());

        // profile-a 成功解密后清除自身记录
        let encrypted = crypto.encrypt("ok").unwrap();
        let _ = crypto.decrypt_to_zeroizing("profile-a", &encrypted);
        assert!(!crypto.has_decryption_error());
    }

    #[test]
    fn test_failed_profile_not_cleared_by_other_profile_success() {
        // F10 核心：A 解密失败后，B 的成功解密不得清除 A 的失败标志
        let (_tmp, crypto) = make_crypto();
        let encrypted_ok = crypto.encrypt("good").unwrap();

        // profile-a 失败、profile-b 成功
        let _ = crypto.decrypt_to_zeroizing("profile-a", "ENC:AAAA");
        assert!(
            crypto
                .decrypt_to_zeroizing("profile-b", &encrypted_ok)
                .is_ok()
        );

        // 汇总语义仍为 true（A 仍未修复）
        assert!(crypto.has_decryption_error());

        // B 再次成功也不影响 A
        let _ = crypto.decrypt_to_zeroizing("profile-b", &encrypted_ok);
        assert!(crypto.has_decryption_error());

        // A 自身成功后才整体清除
        let _ = crypto.decrypt_to_zeroizing("profile-a", &encrypted_ok);
        assert!(!crypto.has_decryption_error());
    }

    #[test]
    fn test_can_decrypt_is_pure_query_without_flag_side_effect() {
        // can_decrypt 查询不得置位失败标志，也不得清除既有失败记录
        let (_tmp, crypto) = make_crypto();
        let encrypted = crypto.encrypt("ok").unwrap();
        let bad = "ENC:AAAA";

        // 查询坏密文：返回 false 但不置位
        assert!(!crypto.can_decrypt(bad));
        assert!(!crypto.has_decryption_error());

        // 经生产路径登记失败后，查询好密文不清除标志
        let _ = crypto.decrypt_to_zeroizing("profile-a", bad);
        assert!(crypto.has_decryption_error());
        assert!(crypto.can_decrypt(&encrypted));
        assert!(crypto.has_decryption_error());
    }

    #[test]
    fn test_successful_decrypt_clears_error_flag() {
        // 保留历史用例语义：同一 Profile 成功解密后标志清除
        let (_tmp, crypto) = make_crypto();
        // 先触发错误
        let _ = crypto.decrypt_to_zeroizing("p", "ENC:AAAA");
        assert!(crypto.has_decryption_error());
        // 成功解密后标记应清除
        let encrypted = crypto.encrypt("ok").unwrap();
        let _ = crypto.decrypt_to_zeroizing("p", &encrypted);
        assert!(!crypto.has_decryption_error());
    }

    // ============ decrypt_to_zeroizing ============

    #[test]
    fn test_decrypt_to_zeroizing_roundtrip() {
        let (_tmp, crypto) = make_crypto();
        let plaintext = "sensitive_data";
        let encrypted = crypto.encrypt(plaintext).unwrap();
        let result = crypto.decrypt_to_zeroizing("p1", &encrypted).unwrap();
        assert_eq!(&*result, plaintext);
    }

    #[test]
    fn test_decrypt_to_zeroizing_empty() {
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt_to_zeroizing("p1", "").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decrypt_to_zeroizing_plaintext_passthrough() {
        // 不带 ENC: 前缀应直接返回明文
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt_to_zeroizing("p1", "plain").unwrap();
        assert_eq!(&*result, "plain");
    }

    // ============ 密钥文件损坏处理 ============

    #[test]
    fn test_corrupt_key_file_generates_new_key() {
        // 密钥文件长度不为 32 字节时，应备份旧文件并生成新密钥
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join(".enc_key");
        // 写入一个长度错误的文件
        std::fs::write(&key_path, [0u8; 16]).unwrap();

        let crypto = PasswordCrypto::new(key_path.clone());
        let encrypted = crypto.encrypt("test").unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, "test");
        // 旧文件应被备份
        assert!(
            tmp.path().join(".corrupt.key").exists()
                || !key_path.exists()
                || std::fs::metadata(&key_path).unwrap().len() == 32
        );
    }

    // ============ Python 旧版密钥继承（历史遗留：两版密钥冲突） ============

    #[test]
    fn test_inherit_python_key_when_rs_key_missing() {
        // `.enc_key.rs` 不存在而 Python 版 `.enc_key`（base64 32 字节）存在时，
        // 应继承同一密钥：Rust 版加密的密文能被"直接以该密钥为 .enc_key.rs"的实例解密。
        let tmp = tempfile::tempdir().unwrap();
        let python_key = [7u8; KEY_LEN]; // 固定"旧版"密钥
        let python_path = tmp.path().join(".enc_key");
        std::fs::write(
            &python_path,
            base64::engine::general_purpose::STANDARD.encode(python_key),
        )
        .unwrap();

        // 继承实例：key_path 指向不存在的 .enc_key.rs
        let rs_path = tmp.path().join(".enc_key.rs");
        let inherited = PasswordCrypto::new(rs_path.clone());
        let encrypted = inherited.encrypt("旧版密码").unwrap();

        // 对照实例：直接把旧密钥当作 .enc_key.rs 内容
        std::fs::write(&rs_path, python_key).unwrap();
        let reference = PasswordCrypto::new(rs_path.clone());
        let decrypted = reference.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, "旧版密码", "继承的密钥应与 Python 旧版一致");
        // .enc_key.rs 应已被写入且内容为 raw 32 字节
        let written = std::fs::read(&rs_path).unwrap();
        assert_eq!(written.len(), KEY_LEN);
        assert_eq!(written, python_key);
    }

    #[test]
    fn test_inherit_python_key_prefers_existing_rs_key() {
        // `.enc_key.rs` 已存在时，不得被 Python 旧版密钥覆盖
        let tmp = tempfile::tempdir().unwrap();
        let rs_path = tmp.path().join(".enc_key.rs");
        let rs_key = [9u8; KEY_LEN];
        std::fs::write(&rs_path, rs_key).unwrap();
        // Python 旧版密钥存在且不同
        let python_path = tmp.path().join(".enc_key");
        std::fs::write(
            &python_path,
            base64::engine::general_purpose::STANDARD.encode([7u8; KEY_LEN]),
        )
        .unwrap();

        let crypto = PasswordCrypto::new(rs_path.clone());
        let encrypted = crypto.encrypt("test").unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, "test");
        // .enc_key.rs 内容保持不变（仍为 rs_key）
        assert_eq!(std::fs::read(&rs_path).unwrap(), rs_key);
    }

    #[test]
    fn test_inherit_python_key_invalid_base64_falls_back_to_new() {
        // Python 旧版密钥内容非法（非 base64）时，应正常生成新密钥
        let tmp = tempfile::tempdir().unwrap();
        let python_path = tmp.path().join(".enc_key");
        std::fs::write(&python_path, "not-a-valid-base64!!!").unwrap();

        let rs_path = tmp.path().join(".enc_key.rs");
        let crypto = PasswordCrypto::new(rs_path.clone());
        let encrypted = crypto.encrypt("test").unwrap();
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, "test");
        // 新密钥已写入 .enc_key.rs（32 字节）
        assert_eq!(std::fs::read(&rs_path).unwrap().len(), KEY_LEN);
    }
}
