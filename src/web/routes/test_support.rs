//! 路由单测共享脚手架（仅 `#[cfg(test)]` 生效）
//!
//! 复用说明：各路由 `mod tests` 按需 `use super::super::test_support::{...}`，
//! 不再每文件重复实现内存 `ConfigApi`（11 方法）与 `RuntimeConfig` 构造。

use std::sync::Arc;

use serde_json::Value;

use crate::config::{
    AppSettings, BrowserSettings, ConfigApi, ConfigError, LoggingSettings, MonitorSettings,
    PauseSettings, ProfileData, ProfileSnapshot, RetrySettings, RuntimeConfig, SettingsData,
    UpdaterSettings, WorkerSettings,
};

/// 构造测试用 RuntimeConfig（类型未派生 Default，字段逐个填充默认值）
pub fn test_runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        browser: BrowserSettings::default(),
        monitor: MonitorSettings::default(),
        pause: PauseSettings::default(),
        logging: LoggingSettings::default(),
        retry: RetrySettings::default(),
        worker: WorkerSettings::default(),
        app: AppSettings::default(),
        updater: UpdaterSettings::default(),
        profile: ProfileSnapshot {
            id: "default".into(),
            name: String::new(),
            username: String::new(),
            password: zeroize::Zeroizing::new(String::new()),
            auth_url: String::new(),
            trigger_url: String::new(),
            isp: String::new(),
            gateway_ip: String::new(),
            wifi_ssid: String::new(),
            active_task: String::new(),
        },
        auto_switch: false,
    }
}

/// 内存 ConfigApi：settings + RuntimeConfig 均可预置，无需磁盘与完整容器
pub struct MockConfigInner {
    pub settings: SettingsData,
    pub runtime: RuntimeConfig,
    pub base_path: std::path::PathBuf,
    pub save_calls: usize,
    /// 打开后 modify_settings_tx 返回内层校验失败（不断言落盘）
    pub modify_fails: bool,
}

impl Default for MockConfigInner {
    fn default() -> Self {
        Self {
            settings: SettingsData::default(),
            runtime: test_runtime_config(),
            base_path: std::path::PathBuf::new(),
            save_calls: 0,
            modify_fails: false,
        }
    }
}

pub struct MockConfigApi(pub Arc<std::sync::Mutex<MockConfigInner>>);

impl MockConfigApi {
    pub fn mocked() -> (Arc<dyn ConfigApi>, Arc<std::sync::Mutex<MockConfigInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockConfigInner::default()));
        (Arc::new(Self(inner.clone())), inner)
    }
}

#[async_trait::async_trait]
impl ConfigApi for MockConfigApi {
    async fn load_settings_async(&self) -> SettingsData {
        self.0.lock().unwrap().settings.clone()
    }

    async fn save_settings(&self, data: &SettingsData) -> Result<(), ConfigError> {
        let mut inner = self.0.lock().unwrap();
        inner.settings = data.clone();
        inner.save_calls += 1;
        Ok(())
    }

    async fn modify_settings_tx(
        &self,
        f: Box<dyn FnOnce(SettingsData) -> Result<SettingsData, String> + Send>,
    ) -> Result<Result<(), String>, ConfigError> {
        let mut inner = self.0.lock().unwrap();
        if inner.modify_fails {
            return Ok(Err("mock 校验失败".to_string()));
        }
        match f(inner.settings.clone()) {
            Ok(new_settings) => {
                inner.settings = new_settings;
                inner.save_calls += 1;
                Ok(Ok(()))
            }
            Err(msg) => Ok(Err(msg)),
        }
    }

    fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
        let _ = id;
        Ok(ProfileData::default())
    }

    async fn save_profile(&self, _profile: &ProfileData) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn reload(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    fn can_decrypt_password(&self, _ciphertext: &str) -> bool {
        true
    }

    fn has_decryption_error(&self) -> bool {
        false
    }

    fn base_path(&self) -> std::path::PathBuf {
        self.0.lock().unwrap().base_path.clone()
    }

    fn runtime_snapshot(&self) -> Arc<RuntimeConfig> {
        Arc::new(self.0.lock().unwrap().runtime.clone())
    }

    fn encrypt_password(&self, raw: &str) -> Result<String, ConfigError> {
        Ok(format!("ENC:mock:{raw}"))
    }
}

/// 读取 oneshot 响应体并解析为 JSON（`data` 包裹层保留，调用方按需解）
pub async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
