//! AI 任务生成：LLM 配置存储、提示词组装、OpenAI 兼容调用与生成编排
//!
//! 由登录页捕获产物（`captures/latest/`）+ 任务 schema 提示词驱动视觉模型
//! 生成浏览器任务 JSON，经任务强校验回喂自纠后返回前端预览入库。

pub mod error;
pub mod generate;
pub mod llm;
pub mod prompt;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::crypto::{PasswordCrypto, default_key_path};
use crate::utils::io::atomic_write_json;

pub use error::AiError;

/// LLM 配置文件名（位于 `<base>/config/` 下，与 settings.json 同级）
const LLM_CONFIG_FILE: &str = "llm.json";

/// LLM 服务配置（AI 任务生成），落盘 `<base>/config/llm.json`
///
/// 独立文件而非并入 settings.json：低频工具配置，避免牵动 RuntimeConfig
/// 快照语义与 schema 迁移；API key 以 AES-256-GCM 密文（`ENC:` 前缀）存储，
/// 密钥与校园网密码共用（`~/.campus_network_auth/.enc_key.rs`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    /// 当前服务商标识；API key 按服务商隔离，切换时不会把旧服务商凭据发给新地址
    #[serde(default)]
    pub provider: String,
    /// OpenAI 兼容 API 根地址（如 `https://open.bigmodel.cn/api/paas/v4`），
    /// 实际请求拼接 `/chat/completions`；允许 http 回环/私网（自定义兼容服务场景）
    #[serde(default)]
    pub base_url: String,
    /// 视觉模型名（如 `glm-4v-flash`）
    #[serde(default)]
    pub model: String,
    /// 旧版单 key 字段，仅用于读取迁移；新写入统一进入 `api_keys_enc`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key_enc: String,
    /// 按服务商隔离的 API key 密文（`ENC:` 前缀）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub api_keys_enc: BTreeMap<String, String>,
    /// 单次生成的 max_tokens 上限；缺省 16384，`null` 表示不携带
    /// 该字段（交由服务商默认，避免硬编码上限截断长任务 JSON 或超出模型限额）
    #[serde(
        default = "default_max_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<u32>,
}

/// max_tokens 缺省值：为较长的任务 JSON 预留空间
fn default_max_tokens() -> Option<u32> {
    Some(16_384)
}

fn default_provider() -> String {
    "custom".to_string()
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: String::new(),
            model: String::new(),
            api_key_enc: String::new(),
            api_keys_enc: BTreeMap::new(),
            max_tokens: default_max_tokens(),
        }
    }
}

impl LlmSettings {
    /// 是否已完成基础配置（base_url + model，key 可为空——部分本地网关免鉴权）
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }

    /// 当前服务商对应的加密 API key；自定义服务按 origin 隔离
    pub fn active_api_key_enc(&self) -> &str {
        self.api_keys_enc
            .get(&self.key_slot())
            .map(String::as_str)
            .or({
                if self.api_key_enc.is_empty() {
                    None
                } else {
                    Some(self.api_key_enc.as_str())
                }
            })
            .unwrap_or_default()
    }

    /// 当前服务商是否已经保存 API key
    pub fn has_active_api_key(&self) -> bool {
        !self.active_api_key_enc().is_empty()
    }

    /// 设置或清除当前服务商的 API key 密文
    pub fn set_active_api_key_enc(&mut self, value: String) {
        let slot = self.key_slot();
        if value.is_empty() {
            self.api_keys_enc.remove(&slot);
        } else {
            self.api_keys_enc.insert(slot, value);
        }
        self.api_key_enc.clear();
    }

    /// 已保存 key 的内置服务商列表（仅返回标识，不暴露密文）
    pub fn configured_providers(&self) -> Vec<String> {
        ["opencode", "glm", "deepseek"]
            .into_iter()
            .filter(|provider| self.api_keys_enc.contains_key(*provider))
            .map(str::to_string)
            .collect()
    }

    fn key_slot(&self) -> String {
        if self.provider != "custom" {
            return self.provider.clone();
        }
        url::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| {
                let host = url.host_str()?;
                let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
                Some(format!("custom:{}://{host}{port}", url.scheme()))
            })
            .unwrap_or_else(|| "custom".to_string())
    }

    fn normalize_after_load(&mut self) {
        if self.provider.trim().is_empty() {
            self.provider = infer_provider(&self.base_url).to_string();
        }
        if !self.api_key_enc.is_empty() {
            let slot = self.key_slot();
            self.api_keys_enc
                .entry(slot)
                .or_insert_with(|| self.api_key_enc.clone());
            self.api_key_enc.clear();
        }
    }
}

/// 根据预设域名识别服务商；未知地址归为自定义服务
pub fn infer_provider(base_url: &str) -> &'static str {
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if host == "api.deepseek.com" {
        "deepseek"
    } else if host == "open.bigmodel.cn" {
        "glm"
    } else if host == "opencode.ai" {
        "opencode"
    } else {
        "custom"
    }
}

/// 校验前端传入的服务商标识，避免任意字符串膨胀 key 映射
pub fn validate_provider(raw: &str) -> Result<String, AiError> {
    let provider = raw.trim().to_ascii_lowercase();
    if matches!(
        provider.as_str(),
        "opencode" | "glm" | "deepseek" | "custom"
    ) {
        Ok(provider)
    } else {
        Err(AiError::ProviderInvalid)
    }
}

/// 校验内置服务商与域名匹配，避免通过篡改请求把其 Key 发送到其他主机。
pub fn validate_provider_base_url(provider: &str, base_url: &str) -> Result<(), AiError> {
    if provider == "custom" || infer_provider(base_url) == provider {
        Ok(())
    } else {
        Err(AiError::ProviderBaseUrlMismatch)
    }
}

/// LLM 配置文件路径 `<base>/config/llm.json`
pub fn llm_config_path(base: &Path) -> PathBuf {
    crate::utils::paths::config_dir(base).join(LLM_CONFIG_FILE)
}

/// 加载 LLM 配置；文件缺失/损坏时返回默认值（不视为错误，首次使用前未配置属正常态）
pub fn load_llm_settings(base: &Path) -> LlmSettings {
    let path = llm_config_path(base);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(|mut settings: LlmSettings| {
                settings.normalize_after_load();
                settings
            })
            .unwrap_or_else(|e| {
                tracing::warn!("LLM 配置解析失败（{}），按未配置处理: {e}", path.display());
                LlmSettings::default()
            }),
        Err(_) => LlmSettings::default(),
    }
}

/// 原子写入 LLM 配置（调用方负责先校验 base_url、加密 api_key）
pub fn save_llm_settings(base: &Path, settings: &LlmSettings) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::utils::paths::config_dir(base))?;
    let mut normalized = settings.clone();
    normalized.normalize_after_load();
    atomic_write_json(&llm_config_path(base), &normalized)
}

/// 加密 API key 明文（与校园网密码共用同一密钥文件）
pub fn encrypt_api_key(raw: &str) -> Result<String, crate::config::ConfigError> {
    PasswordCrypto::new(default_key_path()).encrypt(raw)
}

/// 解密 API key（`Zeroizing` 保证 drop 清零）；明文向后兼容直通
pub fn decrypt_api_key(
    ciphertext: &str,
) -> Result<zeroize::Zeroizing<String>, crate::config::ConfigError> {
    PasswordCrypto::new(default_key_path()).decrypt_to_zeroizing("llm", ciphertext)
}

/// 校验并规范化 LLM base URL，返回去掉尾部 `/` 的规范形式
///
/// 校验规则（AI 生成是用户主动配置的低频出站请求，语义同 updater 的更新源而非
/// 任意 Web 资源拉取，因此不做 SSRF 严格钉扎，但必须守住协议边界）：
/// - 仅允许 `http` / `https`（http 放行任意 host：本地 Ollama / LM Studio 走回环/私网）
/// - 拒绝带 userinfo 的 URL（`https://key@host` 形式会把凭据写进配置明文区）
/// - host 非空
pub fn validate_base_url(raw: &str) -> Result<String, AiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AiError::BaseUrlEmpty);
    }
    let url = url::Url::parse(trimmed)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AiError::BaseUrlUnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AiError::BaseUrlUserInfoForbidden);
    }
    if url.host_str().map(str::is_empty).unwrap_or(true) {
        return Err(AiError::BaseUrlMissingHost);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AiError::BaseUrlQueryOrFragmentForbidden);
    }
    let mut normalized = url.as_str().to_string();
    // Url::as_str 保留 path 与结尾斜杠语义；统一去掉结尾 "/"（拼接 /chat/completions 前再处理）
    while normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

/// 页面捕获产物根目录（与 Python 侧 `_capture_dir` 对齐：worker 工程目录下
/// `captures/latest`，每次捕获覆盖）
pub fn capture_dir(base_path: &Path) -> PathBuf {
    crate::utils::paths::worker_project_dir(base_path)
        .join("captures")
        .join("latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- validate_base_url ----

    #[test]
    fn test_validate_base_url_accepts_https() {
        assert_eq!(
            validate_base_url("https://open.bigmodel.cn/api/paas/v4/").unwrap(),
            "https://open.bigmodel.cn/api/paas/v4"
        );
    }

    #[test]
    fn test_validate_base_url_accepts_loopback_for_local_models() {
        // 本地模型（Ollama/LM Studio）走回环 http，属合法场景
        assert_eq!(
            validate_base_url("http://127.0.0.1:11434/v1").unwrap(),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            validate_base_url("http://localhost:1234").unwrap(),
            "http://localhost:1234"
        );
    }

    #[test]
    fn test_validate_base_url_rejects_bad_scheme() {
        assert!(validate_base_url("ftp://example.com").is_err());
        assert!(validate_base_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_validate_base_url_rejects_userinfo() {
        assert!(validate_base_url("https://sk-key@api.example.com/v1").is_err());
    }

    #[test]
    fn test_validate_base_url_rejects_empty_and_garbage() {
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("   ").is_err());
        assert!(validate_base_url("not a url").is_err());
    }

    // ---- 配置存取 ----

    #[test]
    fn test_llm_settings_roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // 未配置：缺文件返回默认值，is_configured 为 false
        let loaded = load_llm_settings(base);
        assert!(!loaded.is_configured());

        // 写入后往返一致
        let s = LlmSettings {
            provider: "glm".into(),
            base_url: "https://api.example.com/v1".into(),
            model: "glm-4v-flash".into(),
            api_key_enc: "ENC:abc".into(),
            api_keys_enc: BTreeMap::new(),
            max_tokens: None,
        };
        save_llm_settings(base, &s).unwrap();
        let loaded = load_llm_settings(base);
        assert!(loaded.is_configured());
        assert_eq!(loaded.base_url, s.base_url);
        assert_eq!(loaded.model, s.model);
        assert_eq!(loaded.active_api_key_enc(), s.api_key_enc);

        // 损坏内容：按未配置处理不 panic
        std::fs::write(llm_config_path(base), b"{broken").unwrap();
        assert!(!load_llm_settings(base).is_configured());
    }

    #[test]
    fn test_api_key_encrypt_decrypt_roundtrip() {
        let enc = encrypt_api_key("sk-test-12345").unwrap();
        assert!(enc.starts_with("ENC:"));
        let plain = decrypt_api_key(&enc).unwrap();
        assert_eq!(&*plain, "sk-test-12345");
    }

    #[test]
    fn test_api_key_decrypt_plaintext_passthrough() {
        // 明文向后兼容直通（手工编辑 llm.json 的兜底）
        assert_eq!(&*decrypt_api_key("raw-key").unwrap(), "raw-key");
    }

    #[test]
    fn test_provider_keys_are_isolated_and_legacy_key_is_migrated() {
        let mut settings = LlmSettings {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            api_key_enc: "ENC:legacy".into(),
            ..Default::default()
        };
        settings.normalize_after_load();
        assert_eq!(settings.active_api_key_enc(), "ENC:legacy");

        settings.provider = "glm".into();
        settings.base_url = "https://open.bigmodel.cn/api/paas/v4".into();
        assert!(!settings.has_active_api_key());
        settings.set_active_api_key_enc("ENC:glm".into());

        settings.provider = "deepseek".into();
        settings.base_url = "https://api.deepseek.com".into();
        assert_eq!(settings.active_api_key_enc(), "ENC:legacy");
    }

    #[test]
    fn test_explicit_custom_provider_is_not_reclassified() {
        let mut settings = LlmSettings {
            provider: "custom".into(),
            base_url: "https://api.deepseek.com/proxy".into(),
            ..Default::default()
        };
        settings.set_active_api_key_enc("ENC:custom".into());
        settings.normalize_after_load();
        assert_eq!(settings.provider, "custom");
        assert_eq!(settings.active_api_key_enc(), "ENC:custom");

        let mut legacy: LlmSettings = serde_json::from_value(serde_json::json!({
            "base_url": "https://api.deepseek.com",
            "model": "m",
            "api_key_enc": "ENC:legacy"
        }))
        .unwrap();
        legacy.normalize_after_load();
        assert_eq!(legacy.provider, "deepseek");
        assert_eq!(legacy.active_api_key_enc(), "ENC:legacy");
    }

    #[test]
    fn test_validate_base_url_rejects_query_and_fragment() {
        assert!(validate_base_url("https://api.example.com/v1?key=x").is_err());
        assert!(validate_base_url("https://api.example.com/v1#chat").is_err());
    }

    #[test]
    fn test_builtin_provider_must_match_base_url() {
        assert!(validate_provider_base_url("glm", "https://open.bigmodel.cn/api/paas/v4").is_ok());
        assert!(validate_provider_base_url("glm", "https://api.deepseek.com").is_err());
        assert!(validate_provider_base_url("custom", "https://example.com/v1").is_ok());
    }
}
