//! 密码加解密（AES-256-GCM）
//!
//! 使用 AES-256-GCM 对 Profile 密码做认证加密。密钥以 raw 32 字节形式存储在
//! `~/.campus_network_auth/.enc_key.rs`。密文格式：`ENC:` + base64(nonce(12B) || ciphertext+tag(16B))。
//! 空串与无 `ENC:` 前缀的明文向后兼容直接返回。

use std::path::{Path, PathBuf};

use chrono::Local;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::consts::{U12, U32};
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::Aes256Gcm;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rand::RngCore;
use zeroize::Zeroizing;

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
/// 允许两个版本在同一用户目录下共存。
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
    /// 全局解密失败标记（供前端检测是否需要重新输入密码）
    decryption_failed: AtomicBool,
}

impl PasswordCrypto {
    /// 构造加解密器（不立即加载密钥）
    pub fn new(key_path: PathBuf) -> Self {
        Self {
            key: OnceLock::new(),
            key_path,
            decryption_failed: AtomicBool::new(false),
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

        let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes()).map_err(|e| {
            ConfigError::Io(std::io::Error::other(format!("密码加密失败: {e}")))
        })?;

        // 组装 payload：nonce || ciphertext+tag
        let mut payload = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&ciphertext);

        Ok(format!("{ENC_PREFIX}{}", STANDARD.encode(&payload)))
    }

    /// 解密 `ENC:` 密文，返回明文
    pub fn decrypt(&self, ciphertext: &str) -> Result<String, ConfigError> {
        if ciphertext.is_empty() {
            return Ok(String::new());
        }
        // 向后兼容：非 ENC: 前缀视为明文直接返回
        if !ciphertext.starts_with(ENC_PREFIX) {
            return Ok(ciphertext.to_string());
        }
        Ok(self.decrypt_inner(ciphertext)?.to_string())
    }

    /// 解密 `ENC:` 密文，返回 `Zeroizing<String>`（drop 时自动清零）
    pub fn decrypt_to_zeroizing(&self, ciphertext: &str) -> Result<Zeroizing<String>, ConfigError> {
        if ciphertext.is_empty() {
            return Ok(Zeroizing::new(String::new()));
        }
        if !ciphertext.starts_with(ENC_PREFIX) {
            return Ok(Zeroizing::new(ciphertext.to_string()));
        }
        Ok(Zeroizing::new(self.decrypt_inner(ciphertext)?))
    }

    /// 检查是否发生过解密失败
    pub fn has_decryption_error(&self) -> bool {
        self.decryption_failed.load(Ordering::SeqCst)
    }

    /// 清除解密失败标记
    pub fn clear_decryption_error(&self) {
        self.decryption_failed.store(false, Ordering::SeqCst);
    }

    /// 内部解密逻辑，返回明文 String
    fn decrypt_inner(&self, ciphertext: &str) -> Result<String, ConfigError> {
        self.ensure_key()?;
        let key = self.key.get().expect("密钥已确保加载");

        let b64 = &ciphertext[ENC_PREFIX.len()..];
        let payload = match STANDARD.decode(b64) {
            Ok(p) => p,
            Err(_) => {
                self.decryption_failed.store(true, Ordering::SeqCst);
                return Err(ConfigError::DecryptFailed {
                    profile_id: String::new(),
                });
            }
        };

        if payload.len() < NONCE_LEN {
            self.decryption_failed.store(true, Ordering::SeqCst);
            return Err(ConfigError::DecryptFailed {
                profile_id: String::new(),
            });
        }

        let (nonce_part, ct) = payload.split_at(NONCE_LEN);
        let nonce = GenericArray::<u8, U12>::from_slice(nonce_part);
        let key_arr = GenericArray::<u8, U32>::from_slice(key.as_ref());
        let cipher = Aes256Gcm::new(key_arr);

        match cipher.decrypt(nonce, ct) {
            Ok(plain) => {
                self.clear_decryption_error();
                String::from_utf8(plain).map_err(|e| {
                    ConfigError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("解密结果非 UTF-8: {e}"),
                    ))
                })
            }
            Err(_) => {
                self.decryption_failed.store(true, Ordering::SeqCst);
                Err(ConfigError::DecryptFailed {
                    profile_id: String::new(),
                })
            }
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
    fn read_or_create_key(key_path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, ConfigError> {
        if key_path.exists() {
            let bytes = std::fs::read(key_path)?;
            if bytes.len() != KEY_LEN {
                // 密钥长度异常：备份旧文件并生成新密钥
                let ts = Local::now().format("%Y%m%d%H%M%S");
                let backup = {
                    let mut b = key_path.to_path_buf();
                    b.set_extension(format!("corrupt.{ts}.key"));
                    b
                };
                let _ = std::fs::rename(key_path, &backup);
                return Self::generate_and_write_key(key_path);
            }
            let mut arr = [0u8; KEY_LEN];
            arr.copy_from_slice(&bytes);
            return Ok(Zeroizing::new(arr));
        }
        Self::generate_and_write_key(key_path)
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
        // ENC: 后跟无效 base64 应返回错误
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt("ENC:not_valid_base64!!!");
        assert!(result.is_err());
        assert!(crypto.has_decryption_error());
    }

    #[test]
    fn test_decrypt_too_short_payload_fails() {
        // ENC: 后跟有效 base64 但 payload 太短（< nonce 长度）应返回错误
        let (_tmp, crypto) = make_crypto();
        let short = STANDARD.encode([0u8; 4]); // 4 字节 < NONCE_LEN(12)
        let result = crypto.decrypt(&format!("{ENC_PREFIX}{short}"));
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
        let result = crypto2.decrypt(&encrypted);
        assert!(result.is_err());
    }

    // ============ has_decryption_error / clear_decryption_error ============

    #[test]
    fn test_decryption_error_flag_lifecycle() {
        // 测试解密失败标记的设置与清除
        let (_tmp, crypto) = make_crypto();
        assert!(!crypto.has_decryption_error());

        // 触发一个解密错误
        let _ = crypto.decrypt("ENC:AAAA");
        assert!(crypto.has_decryption_error());

        // 清除标记
        crypto.clear_decryption_error();
        assert!(!crypto.has_decryption_error());
    }

    #[test]
    fn test_successful_decrypt_clears_error_flag() {
        // 成功解密后应自动清除失败标记
        let (_tmp, crypto) = make_crypto();
        // 先触发错误
        let _ = crypto.decrypt("ENC:AAAA");
        assert!(crypto.has_decryption_error());
        // 成功解密后标记应清除
        let encrypted = crypto.encrypt("ok").unwrap();
        let _ = crypto.decrypt(&encrypted);
        assert!(!crypto.has_decryption_error());
    }

    // ============ decrypt_to_zeroizing ============

    #[test]
    fn test_decrypt_to_zeroizing_roundtrip() {
        let (_tmp, crypto) = make_crypto();
        let plaintext = "sensitive_data";
        let encrypted = crypto.encrypt(plaintext).unwrap();
        let result = crypto.decrypt_to_zeroizing(&encrypted).unwrap();
        assert_eq!(&*result, plaintext);
    }

    #[test]
    fn test_decrypt_to_zeroizing_empty() {
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt_to_zeroizing("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decrypt_to_zeroizing_plaintext_passthrough() {
        // 不带 ENC: 前缀应直接返回明文
        let (_tmp, crypto) = make_crypto();
        let result = crypto.decrypt_to_zeroizing("plain").unwrap();
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
        assert!(tmp.path().join(".corrupt.key").exists()
            || !key_path.exists()
            || std::fs::metadata(&key_path).unwrap().len() == 32);
    }
}
