//! Profile 路由：认证档案 CRUD + 切换 + 自动检测
//!
//! M1 细粒度 state（profiles 域）：handler 声明 `State<Arc<dyn ProfileApi>>` /
//! `State<Arc<dyn ConfigApi>>` 依赖（经 AppState 的 FromRef 委派提取），
//! 不再触达 `state.container`，测试可注入内存实现（见模块测试）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::config::{ConfigApi, ProfileApi};
use crate::web::error::{ApiError, data};

#[derive(Deserialize)]
pub struct ProfileCreateBody {
    pub id: String,
    pub name: String,
    pub username: String,
    pub password: Zeroizing<String>,
}

#[derive(Deserialize)]
pub struct ProfileUpdateBody {
    pub name: Option<String>,
    pub username: Option<String>,
    pub password: Option<Zeroizing<String>>,
    pub auth_url: Option<String>,
    pub isp: Option<String>,
    pub gateway_ip: Option<String>,
    pub wifi_ssid: Option<String>,
    pub active_task: Option<String>,
}

#[derive(Deserialize)]
pub struct SwitchBody {
    pub profile_id: String,
}

#[derive(Deserialize)]
pub struct AutoSwitchBody {
    pub enabled: bool,
}

/// GET /api/profiles — 列出全部 Profile
pub async fn list_profiles(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let profiles = profiles.list_profiles();
    let settings = config.load_settings_async().await;
    let mut map = serde_json::Map::new();
    // ProfileSummary 仅含展示字段（无密码），列表接口天然不泄露密文
    for p in profiles {
        map.insert(p.id.clone(), serde_json::to_value(&p)?);
    }
    Ok(data(serde_json::json!({
        "profiles": Value::Object(map),
        "active_profile": settings.active_profile_id,
        "auto_switch": settings.auto_switch,
    })))
}

/// GET /api/profiles/{id} — 获取单个 Profile
pub async fn get_profile(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = config.load_profile(&id)?;
    // 避免将密码（密文）泄露给前端
    profile.password = String::new();
    Ok(data(
        serde_json::json!({ "settings": serde_json::to_value(profile)? }),
    ))
}

/// POST /api/profiles/{id} — 创建 Profile
pub async fn create_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
    Json(body): Json<ProfileCreateBody>,
) -> Result<Json<Value>, ApiError> {
    // 路径 id 优先于 body id
    let target_id = if id.is_empty() { body.id.clone() } else { id };
    // 查找或构造 ProfileData
    let existing = config.load_profile(&target_id).ok();
    let mut profile = existing.unwrap_or_default();
    profile.id = target_id.clone();
    profile.name = body.name;
    // 空密码表示“不设置独立密码”，必须保持为空；若把空串加密成 ENC:，
    // 后续 has_password 会误判为已有密码，而运行时解密后仍为空。
    profile.password = if body.password.is_empty() {
        String::new()
    } else {
        config
            .encrypt_password(&body.password)
            .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?
    };
    profile.username = body.username;
    profiles.create_profile(&target_id, profile).await?;
    Ok(data(Value::String("ok".into())))
}

/// PUT /api/profiles/{id} — 更新 Profile
pub async fn update_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
    Json(body): Json<ProfileUpdateBody>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = config.load_profile(&id)?;
    if let Some(name) = body.name {
        profile.name = name;
    }
    if let Some(p) = body.password {
        // GET /api/profiles/{id} 会出于安全考虑把密码清空，前端编辑后保存时
        // 因此会回传 password=""。空串的既有契约是“未修改，保留原密码”，
        // 不能先加密成合法 ENC: 再交给 ProfileService，否则会把原密码覆盖为空。
        if !p.is_empty() {
            // 非空新密码仍需显式传播加密失败，不能返回 ok 但实际未更新。
            profile.password = config
                .encrypt_password(&p)
                .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?;
        }
    }
    if let Some(username) = body.username {
        profile.username = username;
    }
    if let Some(auth_url) = body.auth_url {
        let trimmed = auth_url.trim().to_string();
        if !trimmed.is_empty() {
            let parsed = trimmed.parse::<url::Url>().map_err(|_| {
                crate::web::error::ApiError::BadRequest(format!("认证地址格式非法: {trimmed}"))
            })?;
            match parsed.scheme() {
                "http" | "https" => {}
                _ => {
                    return Err(crate::web::error::ApiError::BadRequest(format!(
                        "认证地址仅支持 http/https，当前为: {}",
                        parsed.scheme()
                    )));
                }
            }
            let host = parsed.host_str().ok_or_else(|| {
                crate::web::error::ApiError::BadRequest(format!("认证地址缺少主机名: {trimmed}"))
            })?;
            if host.is_empty() {
                return Err(crate::web::error::ApiError::BadRequest(format!(
                    "认证地址缺少主机名: {trimmed}"
                )));
            }
        }
        profile.auth_url = trimmed;
    }
    if let Some(isp) = body.isp {
        profile.isp = isp;
    }
    if let Some(gateway_ip) = body.gateway_ip {
        profile.gateway_ip = gateway_ip;
    }
    if let Some(wifi_ssid) = body.wifi_ssid {
        profile.wifi_ssid = wifi_ssid;
    }
    if let Some(active_task) = body.active_task {
        profile.active_task = active_task;
    }
    profiles.update_profile(&id, profile).await?;
    Ok(data(Value::String("ok".into())))
}

/// DELETE /api/profiles/{id} — 删除 Profile
pub async fn delete_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    profiles.delete_profile(&id).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/profiles/switch — 切换活跃 Profile
pub async fn switch_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    Json(body): Json<SwitchBody>,
) -> Result<Json<Value>, ApiError> {
    profiles.switch_profile(&body.profile_id).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/profiles/detect — 检测当前网络环境并自动匹配 Profile
///
/// 复用 ProfileService.detect_matching_profile 做网关 IP / WiFi SSID 匹配，
/// 支持 AND/OR 匹配逻辑。
pub async fn detect_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
) -> Result<Json<Value>, ApiError> {
    let detector = crate::network::detect::create_detector();
    let gateways = detector
        .default_gateways()
        .await
        .map_err(|e| ApiError::Internal(format!("网关检测失败: {e}")))?;
    let ssid = detector
        .current_ssid()
        .await
        .map_err(|e| ApiError::Internal(format!("SSID 检测失败: {e}")))?;
    let gateway_ip = gateways.first().map(|g| g.to_string()).unwrap_or_default();
    // 复用匹配逻辑（gateway_ip + wifi_ssid AND/OR）
    let matched = profiles.detect_matching_profile(&gateway_ip, ssid.as_deref().unwrap_or(""));
    // 按 id 查询 profile 名称，供前端优先展示（matched 为 None 时为 Null）
    let matched_profile_name = match &matched {
        Some(id) => profiles.get_profile(id).ok().map(|p| Value::String(p.name)),
        None => None,
    };
    Ok(data(serde_json::json!({
        "gateway_ip": if gateway_ip.is_empty() { Value::Null } else { Value::String(gateway_ip) },
        "ssid": ssid,
        "matched_profile_id": matched.map(Value::String).unwrap_or(Value::Null),
        "matched_profile_name": matched_profile_name.unwrap_or(Value::Null),
    })))
}

/// POST /api/profiles/auto-switch — 设置自动切换开关
pub async fn auto_switch(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<AutoSwitchBody>,
) -> Result<Json<Value>, ApiError> {
    profiles.set_auto_switch(body.enabled).await?;
    let settings = config.load_settings_async().await;
    Ok(data(serde_json::json!({
        "active_profile": settings.active_profile_id,
        "message": "自动切换已更新",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use crate::config::{ConfigError, ProfileData, ProfileSummary, SettingsData};

    #[derive(Default)]
    struct MockInner {
        profiles: Vec<ProfileData>,
        active: String,
        auto_switch: bool,
    }

    /// 内存 ProfileApi（M1）
    struct MockProfileApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl ProfileApi for MockProfileApi {
        fn list_profiles(&self) -> Vec<ProfileSummary> {
            self.0
                .lock()
                .unwrap()
                .profiles
                .iter()
                .map(|p| ProfileSummary {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    username: p.username.clone(),
                    isp: p.isp.clone(),
                    active_task: p.active_task.clone(),
                })
                .collect()
        }

        fn get_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
            self.0
                .lock()
                .unwrap()
                .profiles
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| ConfigError::ProfileNotFound { id: id.to_string() })
        }

        async fn create_profile(&self, id: &str, data: ProfileData) -> Result<(), ConfigError> {
            let mut inner = self.0.lock().unwrap();
            if inner.profiles.iter().any(|p| p.id == id) {
                return Err(ConfigError::ProfileIdConflict { id: id.to_string() });
            }
            inner.profiles.push(data);
            Ok(())
        }

        async fn update_profile(&self, id: &str, data: ProfileData) -> Result<(), ConfigError> {
            let mut inner = self.0.lock().unwrap();
            let p = inner
                .profiles
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| ConfigError::ProfileNotFound { id: id.to_string() })?;
            *p = data;
            Ok(())
        }

        async fn delete_profile(&self, id: &str) -> Result<(), ConfigError> {
            if id == "default" {
                return Err(ConfigError::CannotDeleteDefault);
            }
            let mut inner = self.0.lock().unwrap();
            inner.profiles.retain(|p| p.id != id);
            Ok(())
        }

        async fn switch_profile(&self, id: &str) -> Result<(), ConfigError> {
            let mut inner = self.0.lock().unwrap();
            if !inner.profiles.iter().any(|p| p.id == id) {
                return Err(ConfigError::ProfileNotFound { id: id.to_string() });
            }
            inner.active = id.to_string();
            Ok(())
        }

        async fn set_auto_switch(&self, enabled: bool) -> Result<(), ConfigError> {
            self.0.lock().unwrap().auto_switch = enabled;
            Ok(())
        }

        fn detect_matching_profile(&self, _gateway_ip: &str, _ssid: &str) -> Option<String> {
            None
        }

        fn save_password(&self, raw: Option<&str>, existing: &str) -> String {
            match raw {
                Some(r) if !r.is_empty() => format!("ENC:{r}"),
                _ => existing.to_string(),
            }
        }
    }

    /// 内存 ConfigApi（profiles handler 测试所需的最小面）
    struct MockConfigApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl ConfigApi for MockConfigApi {
        async fn load_settings_async(&self) -> SettingsData {
            let inner = self.0.lock().unwrap();
            SettingsData {
                active_profile_id: inner.active.clone(),
                auto_switch: inner.auto_switch,
                ..SettingsData::default()
            }
        }

        async fn save_settings(&self, _data: &SettingsData) -> Result<(), ConfigError> {
            Ok(())
        }

        fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
            self.0
                .lock()
                .unwrap()
                .profiles
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| ConfigError::ProfileNotFound { id: id.to_string() })
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
            std::path::PathBuf::new()
        }

        fn runtime_snapshot(&self) -> std::sync::Arc<crate::config::RuntimeConfig> {
            unreachable!("profiles handler 测试不触达 runtime_snapshot")
        }

        fn encrypt_password(&self, raw: &str) -> Result<String, ConfigError> {
            Ok(format!("ENC:{raw}"))
        }
    }

    fn profile_of(id: &str) -> ProfileData {
        ProfileData {
            id: id.into(),
            name: format!("档案 {id}"),
            ..Default::default()
        }
    }

    /// 双 State 提取的测试 Router：ProfileApi + ConfigApi 组合为单一 state 类型
    #[derive(Clone)]
    struct TestState {
        profiles: Arc<dyn ProfileApi>,
        config: Arc<dyn ConfigApi>,
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn ProfileApi> {
        fn from_ref(state: &TestState) -> Self {
            state.profiles.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn ConfigApi> {
        fn from_ref(state: &TestState) -> Self {
            state.config.clone()
        }
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            profiles: vec![profile_of("default"), profile_of("dorm")],
            active: "default".into(),
            auto_switch: false,
        }));
        let state = TestState {
            profiles: Arc::new(MockProfileApi(inner.clone())),
            config: Arc::new(MockConfigApi(inner.clone())),
        };
        let app = axum::Router::new()
            .route("/api/profiles", get(list_profiles))
            .route(
                "/api/profiles/{id}",
                get(get_profile)
                    .post(create_profile)
                    .put(update_profile)
                    .delete(delete_profile),
            )
            .route("/api/profiles/switch", axum::routing::post(switch_profile))
            .route(
                "/api/profiles/auto-switch",
                axum::routing::post(auto_switch),
            )
            .with_state(state);
        (app, inner)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 列表返回 map + active/auto_switch 元数据
    #[tokio::test]
    async fn test_list_profiles_shape() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let d = &v["data"];
        assert_eq!(d["active_profile"], "default");
        assert_eq!(d["auto_switch"], false);
        assert_eq!(d["profiles"].as_object().unwrap().len(), 2);
        assert_eq!(d["profiles"]["dorm"]["name"], "档案 dorm");
    }

    /// 切换到存在的 Profile 成功；不存在的返回错误
    #[tokio::test]
    async fn test_switch_profile() {
        let (app, inner) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/switch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"profile_id": "dorm"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().active, "dorm");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/switch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"profile_id": "missing"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 删除 default 被拒绝（业务错误 → 非 200）
    #[tokio::test]
    async fn test_delete_default_rejected() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/profiles/default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().profiles.len(), 2);
    }

    /// auto-switch 落库并回显 active
    #[tokio::test]
    async fn test_auto_switch_roundtrip() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/auto-switch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(inner.lock().unwrap().auto_switch);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["active_profile"], "default");
    }

    /// Profile 编辑保存时空密码表示“未修改”，必须保留既有密文
    #[tokio::test]
    async fn test_update_profile_empty_password_preserves_existing() {
        let (app, inner) = mock_app();
        {
            let mut guard = inner.lock().unwrap();
            guard
                .profiles
                .iter_mut()
                .find(|p| p.id == "dorm")
                .unwrap()
                .password = "ENC:old-secret".into();
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"name": "改名后", "password": ""}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let guard = inner.lock().unwrap();
        let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.name, "改名后");
        assert_eq!(dorm.password, "ENC:old-secret");
    }

    /// 新建 Profile 时空密码应保持为空，不能生成“可解密但明文为空”的假密文
    #[tokio::test]
    async fn test_create_profile_empty_password_stays_empty() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/new-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "new-profile",
                            "name": "新方案",
                            "username": "student",
                            "password": ""
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let guard = inner.lock().unwrap();
        let created = guard
            .profiles
            .iter()
            .find(|p| p.id == "new-profile")
            .unwrap();
        assert!(created.password.is_empty());
    }

    /// 单个 Profile 读取不泄露密码
    #[tokio::test]
    async fn test_get_profile_masks_password() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/profiles/dorm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["settings"]["password"], "");
    }
}
