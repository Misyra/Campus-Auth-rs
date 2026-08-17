//! ProfileService：Profile CRUD + 匹配 + 切换
//!
//! 业务逻辑层，所有文件 IO 通过注入的 [`ConfigService`] 执行。密码字段遵循
//! `save_password` 语义：空值/明文/密文三种输入的区别处理。

use std::sync::Arc;

use crate::config::schema::ProfileData;
use crate::config::service::ConfigService;
use crate::config::{ConfigError, ConfigReloadSignal};

/// Profile 摘要（不含密码），用于列表展示
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileSummary {
    /// Profile ID
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 登录用户名
    pub username: String,
    /// 运营商
    pub isp: String,
    /// 活跃任务 ID
    pub active_task: String,
}

/// Profile 业务层：CRUD + 匹配 + 切换
pub struct ProfileService {
    /// 注入的配置服务，所有文件 IO 通过它执行
    config: Arc<ConfigService>,
}

impl ProfileService {
    /// 构造 Profile 服务
    pub fn new(config: Arc<ConfigService>) -> Self {
        Self { config }
    }

    /// 列出所有 Profile（不含密码）
    pub fn list_profiles(&self) -> Vec<ProfileSummary> {
        self.config
            .load_all_profiles()
            .into_iter()
            .map(|p| ProfileSummary {
                id: p.id,
                name: p.name,
                username: p.username,
                isp: p.isp,
                active_task: p.active_task,
            })
            .collect()
    }

    /// 获取单个 Profile（含密码，调用方需谨慎处理）
    pub fn get_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
        self.config.load_profile(id)
    }

    /// 创建 Profile（id 已存在则冲突；传入 id 会被规范化为 slug）
    pub async fn create_profile(&self, id: &str, data: ProfileData) -> Result<(), ConfigError> {
        // 将传入 id 规范化为合法 slug（小写、仅保留 [a-z0-9-_]、空白转连字符）
        let slug = slugify_id(id);
        if slug.is_empty() {
            return Err(ConfigError::ProfileIdConflict { id: id.to_string() });
        }
        // 已存在则视为冲突
        if self.config.load_all_profiles().iter().any(|p| p.id == slug) {
            return Err(ConfigError::ProfileIdConflict { id: slug });
        }
        let mut profile = data;
        profile.id = slug;
        self.config.save_profile(&profile).await
    }

    /// 更新 Profile（密码字段走 `save_password` 语义）
    pub async fn update_profile(&self, id: &str, data: ProfileData) -> Result<(), ConfigError> {
        // 读取既有密码，供 save_password 在空串/未修改时保留
        let existing_pw = self
            .config
            .load_profile(id)
            .map(|p| p.password)
            .unwrap_or_default();
        let mut profile = data;
        profile.id = id.to_string();
        // 密码字段走 save_password 语义：空串保留原密码、ENC: 透传、明文加密
        profile.password = self.save_password(
            if profile.password.is_empty() {
                None
            } else {
                Some(profile.password.as_str())
            },
            &existing_pw,
        );
        self.config.save_profile(&profile).await
    }

    /// 删除 Profile（不允许删除 default）
    pub async fn delete_profile(&self, id: &str) -> Result<(), ConfigError> {
        if id == "default" {
            return Err(ConfigError::CannotDeleteDefault);
        }
        self.config.delete_profile(id).await
    }

    /// 切换活跃 Profile：更新 settings.json 的 active_profile_id 并触发 reload
    pub async fn switch_profile(&self, id: &str) -> Result<(), ConfigError> {
        // 校验目标 Profile 存在
        let exists = self.config.load_all_profiles().iter().any(|p| p.id == id);
        if !exists {
            return Err(ConfigError::ProfileNotFound { id: id.to_string() });
        }
        let mut settings = self.config.load_settings();
        settings.active_profile_id = id.to_string();
        self.config.save_settings(&settings).await?;
        // 仅切换 Profile 不影响定时任务表，发送 ProfileSwitched 信号避免调度器全量重载任务
        self.config
            .reload_with_signal(ConfigReloadSignal::ProfileSwitched {
                id: id.to_string(),
            })
            .await?;
        Ok(())
    }

    /// 设置自动切换开关
    pub async fn set_auto_switch(&self, enabled: bool) -> Result<(), ConfigError> {
        let mut settings = self.config.load_settings();
        settings.auto_switch = enabled;
        self.config.save_settings(&settings).await?;
        // 触发 reload，使 ArcSwap<RuntimeConfig> 权威源同步更新，
        // 避免其他服务读取到旧快照造成读取来源双轨（历史遗留 F14）
        self.config.reload().await?;
        Ok(())
    }

    /// 根据网关 IP 与 WiFi SSID 匹配 Profile
    ///
    /// 匹配规则：gateway_ip 与 wifi_ssid 均设置时取 AND；仅设置一个时满足即可；
    /// 两者均空则跳过（仅手动切换）。
    ///
    /// 多个 Profile 命中时，按 (匹配强度降序, id 升序) 稳定选取：优先选择同时约束
    /// gateway 与 ssid 的更具体 Profile；强度相同时按 id 字典序取最小，避免
    /// `load_all_profiles` 顺序不确定导致的匹配抖动（历史遗留 F15）。
    pub fn detect_matching_profile(&self, gateway_ip: &str, ssid: &str) -> Option<String> {
        // (匹配强度, id)：强度 = 生效的约束条件数（1 或 2）
        let mut candidates: Vec<(u8, String)> = Vec::new();
        for p in self.config.load_all_profiles() {
            let has_gw = !p.gateway_ip.is_empty();
            let has_ssid = !p.wifi_ssid.is_empty();
            if !has_gw && !has_ssid {
                continue;
            }
            let gw_ok = !has_gw || p.gateway_ip == gateway_ip;
            let ssid_ok = !has_ssid || p.wifi_ssid == ssid;
            if gw_ok && ssid_ok {
                let strength = u8::from(has_gw) + u8::from(has_ssid);
                candidates.push((strength, p.id));
            }
        }
        // 强度降序、id 升序：稳定且优先最具体的匹配
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        candidates.into_iter().next().map(|(_, id)| id)
    }

    /// 处理前端密码提交语义
    ///
    /// - `None` / 空串 → 不修改，返回 existing
    /// - 以 `ENC:` 开头 → 已是密文，原样返回
    /// - 其他明文 → 加密后返回 `ENC:...`
    pub fn save_password(&self, raw: Option<&str>, existing: &str) -> String {
        match raw {
            None => existing.to_string(),
            Some("") => existing.to_string(),
            Some(s) if s.starts_with("ENC:") => s.to_string(),
            Some(s) => match self.config.encrypt_password(s) {
                Ok(encrypted) => encrypted,
                Err(e) => {
                    tracing::warn!("密码加密失败，保留原密码: {e}");
                    existing.to_string()
                }
            },
        }
    }
}

/// 将任意字符串规范化为 Profile ID slug
///
/// 规则：转小写；空白与下划线折叠为单个连字符；仅保留 ASCII 字母数字与连字符；
/// 丢弃其余字符（含中文与标点）；首尾连字符去除。结果为空说明原始字符串无可保留字符。
fn slugify_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if (ch.is_whitespace() || ch == '_' || ch == '-') && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        // 其他字符直接丢弃
    }
    out.truncate(64);
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建一个用于测试的最小 ConfigService（使用临时目录）
    async fn make_config_service() -> (tempfile::TempDir, Arc<ConfigService>) {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let service = ConfigService::new(tmp.path().to_path_buf(), tx).await.unwrap();
        (tmp, service)
    }

    // ============ save_password 三种输入 ============

    #[tokio::test]
    async fn test_save_password_none_keeps_existing() {
        // None 输入不修改，返回现有密码
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.save_password(None, "existing_encrypted");
        assert_eq!(result, "existing_encrypted");
    }

    #[tokio::test]
    async fn test_save_password_empty_keeps_existing() {
        // 空串输入不修改，返回现有密码
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.save_password(Some(""), "existing_encrypted");
        assert_eq!(result, "existing_encrypted");
    }

    #[tokio::test]
    async fn test_save_password_enc_prefix_passthrough() {
        // 已是 ENC: 前缀的密文应原样返回
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let enc = "ENC:base64encodeddata";
        let result = svc.save_password(Some(enc), "old_password");
        assert_eq!(result, enc);
    }

    #[tokio::test]
    async fn test_save_password_plaintext_gets_encrypted() {
        // 明文密码应被加密，结果以 ENC: 开头
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.save_password(Some("my_password"), "");
        assert!(result.starts_with("ENC:"));
        assert_ne!(result, "my_password");
    }

    // ============ Profile CRUD ============

    #[tokio::test]
    async fn test_create_and_get_profile() {
        // 创建 Profile 后能正确读取
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());

        let data = ProfileData {
            username: "test_user".to_string(),
            isp: "移动".to_string(),
            ..Default::default()
        };

        svc.create_profile("test1", data).await.unwrap();

        let loaded = svc.get_profile("test1").unwrap();
        assert_eq!(loaded.username, "test_user");
        assert_eq!(loaded.isp, "移动");
    }

    #[tokio::test]
    async fn test_create_profile_conflict_on_duplicate_id() {
        // 重复 ID 创建应返回冲突错误
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);

        svc.create_profile("dup", ProfileData::default()).await.unwrap();
        let result = svc.create_profile("dup", ProfileData::default()).await;
        assert!(matches!(result, Err(ConfigError::ProfileIdConflict { id }) if id == "dup"));
    }

    #[tokio::test]
    async fn test_create_profile_rejects_empty_id() {
        // 空 ID 应被拒绝
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.create_profile("", ProfileData::default()).await;
        assert!(matches!(result, Err(ConfigError::ProfileIdConflict { id }) if id.is_empty()));
    }

    #[tokio::test]
    async fn test_create_profile_slugifies_id() {
        // 含空白与标点的 ID 应被规范化为 slug
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());

        let data = ProfileData {
            username: "u1".to_string(),
            ..Default::default()
        };
        svc.create_profile("My Profile!", data).await.unwrap();

        let loaded = svc.get_profile("my-profile").unwrap();
        assert_eq!(loaded.id, "my-profile");
    }

    #[tokio::test]
    async fn test_create_profile_slug_conflict() {
        // 规范化后与已有 ID 冲突应返回冲突错误
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());
        svc.create_profile("My-Profile", ProfileData::default()).await.unwrap();
        let result = svc.create_profile("my_profile", ProfileData::default()).await;
        assert!(matches!(result, Err(ConfigError::ProfileIdConflict { id }) if id == "my-profile"));
    }

    #[test]
    fn test_slugify_id_rules() {
        // 验证 slug 规范化规则
        assert_eq!(slugify_id("My Profile!"), "my-profile");
        assert_eq!(slugify_id("  Hello__World  "), "hello-world");
        assert_eq!(slugify_id("foo--bar"), "foo-bar");
        assert_eq!(slugify_id("!!!"), "");
        assert_eq!(slugify_id(""), "");
        assert_eq!(slugify_id("保留中文ABC"), "abc");
    }

    #[tokio::test]
    async fn test_delete_profile_not_found() {
        // 删除不存在的 Profile 应返回 NotFound
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.delete_profile("nonexistent").await;
        assert!(matches!(result, Err(ConfigError::ProfileNotFound { .. })));
    }

    #[tokio::test]
    async fn test_delete_default_profile_rejected() {
        // 不允许删除 default Profile
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);
        let result = svc.delete_profile("default").await;
        assert!(matches!(result, Err(ConfigError::CannotDeleteDefault)));
    }

    #[tokio::test]
    async fn test_list_profiles() {
        // 列出所有 Profile
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());

        // 创建一个新 Profile
        let data = ProfileData {
            id: "custom".to_string(),
            name: "自定义".to_string(),
            ..Default::default()
        };
        svc.create_profile("custom", data).await.unwrap();

        let profiles = svc.list_profiles();
        let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"default"));
        assert!(ids.contains(&"custom"));
    }

    // ============ detect_matching_profile ============

    #[tokio::test]
    async fn test_detect_matching_profile_by_gateway() {
        // 仅设置 gateway_ip 匹配
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());

        let data = ProfileData {
            id: "gw-profile".to_string(),
            gateway_ip: "10.0.0.1".to_string(),
            ..Default::default()
        };
        svc.create_profile("gw-profile", data).await.unwrap();

        let result = svc.detect_matching_profile("10.0.0.1", "");
        assert_eq!(result, Some("gw-profile".to_string()));
    }

    #[tokio::test]
    async fn test_detect_matching_profile_by_ssid() {
        // 仅设置 wifi_ssid 匹配
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config.clone());

        let data = ProfileData {
            id: "wifi-profile".to_string(),
            wifi_ssid: "CampusWiFi".to_string(),
            ..Default::default()
        };
        svc.create_profile("wifi-profile", data).await.unwrap();

        let result = svc.detect_matching_profile("", "CampusWiFi");
        assert_eq!(result, Some("wifi-profile".to_string()));
    }

    #[tokio::test]
    async fn test_detect_matching_profile_no_match() {
        // 无匹配时返回 None
        let (_tmp, config) = make_config_service().await;
        let svc = ProfileService::new(config);

        let result = svc.detect_matching_profile("192.168.1.1", "UnknownSSID");
        assert!(result.is_none());
    }
}
