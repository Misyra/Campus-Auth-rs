//! Profile 路由：认证档案 CRUD + 切换 + 自动检测

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

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
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let profiles = state.container.profiles.list_profiles();
    let settings = state.container.config.load_settings();
    let mut map = serde_json::Map::new();
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
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = state.container.config.load_profile(&id)?;
    // 避免将密码（密文）泄露给前端
    profile.password = String::new();
    Ok(data(serde_json::json!({ "settings": serde_json::to_value(profile)? })))
}

/// POST /api/profiles/{id} — 创建 Profile
pub async fn create_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ProfileCreateBody>,
) -> Result<Json<Value>, ApiError> {
    // 路径 id 优先于 body id
    let target_id = if id.is_empty() { body.id.clone() } else { id };
    // 查找或构造 ProfileData
    let existing = state.container.config.load_profile(&target_id).ok();
    let mut profile = existing.unwrap_or_default();
    profile.id = target_id.clone();
    profile.name = body.name;
    profile.password = state
        .container
        .config
        .encrypt_password(&body.password)
        .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?;
    profile.username = body.username;
    state
        .container
        .profiles
        .create_profile(&target_id, profile)
        .await?;
    Ok(data(Value::String("ok".into())))
}

/// PUT /api/profiles/{id} — 更新 Profile
pub async fn update_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ProfileUpdateBody>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = state.container.config.load_profile(&id)?;
    if let Some(name) = body.name {
        profile.name = name;
    }
    if let Some(p) = body.password {
        // 加密失败需显式报错，不能静默跳过（否则返回 ok 但密码实际未更新）
        profile.password = state
            .container
            .config
            .encrypt_password(&p)
            .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?;
    }
    if let Some(username) = body.username {
        profile.username = username;
    }
    if let Some(auth_url) = body.auth_url {
        profile.auth_url = auth_url;
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
    state
        .container
        .profiles
        .update_profile(&id, profile)
        .await?;
    Ok(data(Value::String("ok".into())))
}

/// DELETE /api/profiles/{id} — 删除 Profile
pub async fn delete_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.profiles.delete_profile(&id).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/profiles/switch — 切换活跃 Profile
pub async fn switch_profile(
    State(state): State<AppState>,
    Json(body): Json<SwitchBody>,
) -> Result<Json<Value>, ApiError> {
    state.container.profiles.switch_profile(&body.profile_id).await?;
    Ok(data(Value::String("ok".into())))
}

/// POST /api/profiles/detect — 检测当前网络环境并自动匹配 Profile
///
/// 复用 ProfileService.detect_matching_profile 做网关 IP / WiFi SSID 匹配，
/// 支持 AND/OR 匹配逻辑。
pub async fn detect_profile(
    State(state): State<AppState>,
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
    // 复用 ProfileService 的匹配逻辑（gateway_ip + wifi_ssid AND/OR）
    let matched = state.container.profiles.detect_matching_profile(
        &gateway_ip,
        ssid.as_deref().unwrap_or(""),
    );
    Ok(data(serde_json::json!({
        "gateway_ip": if gateway_ip.is_empty() { Value::Null } else { Value::String(gateway_ip) },
        "ssid": ssid,
        "matched_profile_id": matched.map(Value::String).unwrap_or(Value::Null),
    })))
}

/// POST /api/profiles/auto-switch — 设置自动切换开关
pub async fn auto_switch(
    State(state): State<AppState>,
    Json(body): Json<AutoSwitchBody>,
) -> Result<Json<Value>, ApiError> {
    state
        .container
        .profiles
        .set_auto_switch(body.enabled)
        .await?;
    let settings = state.container.config.load_settings();
    Ok(data(serde_json::json!({
        "active_profile": settings.active_profile_id,
        "message": "自动切换已更新",
    })))
}
