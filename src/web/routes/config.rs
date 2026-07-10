//! 配置路由：全局设置读写、日志级别、纯净模式

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/config — 获取当前全局设置
///
/// 返回扁平结构，前端期望的格式：
/// { browser, monitor, pause, logging, retry, app_settings, credentials, active_task }
/// monitor 字段做后端→前端字段名映射
pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let settings = state.container.config.load_settings();
    let active_id = &settings.active_profile_id;
    let profile = state.container.config.load_profile(active_id).unwrap_or_default();

    Ok(data(serde_json::json!({
        "browser": settings.global.browser,
        "monitor": monitor_backend_to_frontend(&settings.global.monitor),
        "pause": settings.global.pause,
        "logging": settings.global.logging,
        "retry": settings.global.retry_settings,
        "app_settings": settings.global.app,
        "worker": settings.global.worker,
        "updater": settings.global.updater,
        "username": profile.username,
        "auth_url": profile.auth_url,
        "isp": profile.isp,
        "carrier_custom": "",
        "active_task": profile.active_task,
        "has_password": !profile.password.is_empty()
    })))
}

/// PUT /api/config — 全量更新设置
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let settings: crate::config::SettingsData = serde_json::from_value(body)?;
    state.container.config.save_settings(&settings).await?;
    state.container.config.reload().await?;
    let updated = state.container.config.load_settings();
    Ok(data(serde_json::to_value(updated)?))
}

/// PATCH /api/config — 局部更新全局设置（合并后保存）
///
/// 前端发送扁平结构 { browser, monitor, pause, logging, retry, app_settings, ... }
/// 后端 SettingsData 结构为 { global: { browser, monitor, ... }, active_profile_id, ... }
/// 需要将前端的扁平 key 映射到 global 子结构下
pub async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mut current = state.container.config.load_settings();
    let mut current_value = serde_json::to_value(&current)?;

    // 将前端的扁平字段映射到 global 子结构
    if let Some(obj) = patch.as_object() {
        let mut global_patch = serde_json::Map::new();
        let mut profile_patch = serde_json::Map::new();
        let mut other_patch = serde_json::Map::new();

        // 前端字段名 → 后端字段名映射
        let field_map: std::collections::HashMap<&str, &str> = [
            ("retry", "retry_settings"),
            ("app_settings", "app"),
        ]
        .into_iter()
        .collect();

        // 凭证字段属于 Profile 而非全局设置
        let profile_keys = ["username", "password", "auth_url", "isp", "carrier_custom", "active_task"];

        // 全局设置字段
        let global_keys = [
            "browser",
            "monitor",
            "pause",
            "logging",
            "retry",
            "app_settings",
            "retry_settings",
            "app",
            "worker",
            "updater",
        ];

        for (k, v) in obj {
            if profile_keys.contains(&k.as_str()) {
                profile_patch.insert(k.clone(), v.clone());
            } else if k == "monitor" {
                // monitor 字段需要前端→后端字段名映射
                let backend_monitor = monitor_frontend_to_backend(v);
                global_patch.insert("monitor".to_string(), backend_monitor);
            } else if global_keys.contains(&k.as_str()) {
                // 映射前端字段名到后端字段名
                let default_key = k.as_str();
                let mapped_key = field_map.get(k.as_str()).copied().unwrap_or(default_key);
                global_patch.insert(mapped_key.to_string(), v.clone());
            } else {
                other_patch.insert(k.clone(), v.clone());
            }
        }

        // 合并 global 字段
        if !global_patch.is_empty() {
            if let Some(global) = current_value.get_mut("global") {
                json_merge(global, &Value::Object(global_patch));
            }
        }

        // 合并其他字段（如 active_profile_id, active_task 等）
        if !other_patch.is_empty() {
            json_merge(&mut current_value, &Value::Object(other_patch));
        }

        // 保存凭证到活跃 Profile
        if !profile_patch.is_empty() {
            let active_id = current_value
                .get("active_profile_id")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            if let Ok(mut profile) = state.container.config.load_profile(&active_id) {
                if let Some(username) = profile_patch.get("username").and_then(|v| v.as_str()) {
                    profile.username = username.to_string();
                }
                if let Some(auth_url) = profile_patch.get("auth_url").and_then(|v| v.as_str()) {
                    profile.auth_url = auth_url.to_string();
                }
                if let Some(isp) = profile_patch.get("isp").and_then(|v| v.as_str()) {
                    profile.isp = isp.to_string();
                }
                if let Some(active_task) = profile_patch.get("active_task").and_then(|v| v.as_str()) {
                    profile.active_task = active_task.to_string();
                }
                if let Some(password) = profile_patch.get("password") {
                    let pwd_str = password.as_str().unwrap_or("");
                    profile.password = state
                        .container
                        .profiles
                        .save_password(Some(pwd_str), &profile.password);
                }
                state.container.config.save_profile(&profile).await?;
            }
        }
    }

    current = serde_json::from_value(current_value)?;
    state.container.config.save_settings(&current).await?;
    state.container.config.reload().await?;
    let settings = state.container.config.load_settings();
    let active_id = &settings.active_profile_id;
    let profile = state.container.config.load_profile(active_id).unwrap_or_default();
    Ok(data(serde_json::json!({
        "browser": settings.global.browser,
        "monitor": monitor_backend_to_frontend(&settings.global.monitor),
        "pause": settings.global.pause,
        "logging": settings.global.logging,
        "retry": settings.global.retry_settings,
        "app_settings": settings.global.app,
        "worker": settings.global.worker,
        "updater": settings.global.updater,
        "username": profile.username,
        "auth_url": profile.auth_url,
        "isp": profile.isp,
        "carrier_custom": "",
        "active_task": profile.active_task,
        "has_password": !profile.password.is_empty()
    })))
}

/// POST /api/config/reload — 重新加载配置
pub async fn reload_settings(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    state.container.config.reload().await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/config/defaults — 返回配置默认值（扁平结构，与 GET /api/config 格式对齐）
pub async fn get_config_defaults(
    State(_state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let defaults = crate::config::SettingsData::default();
    let g = &defaults.global;
    Ok(data(serde_json::json!({
        "browser": g.browser,
        "monitor": monitor_backend_to_frontend(&g.monitor),
        "pause": g.pause,
        "logging": g.logging,
        "retry": g.retry_settings,
        "app_settings": g.app,
        "worker": g.worker,
        "updater": g.updater,
        "username": "",
        "auth_url": "",
        "isp": "",
        "carrier_custom": "",
        "active_task": "",
        "has_password": false
    })))
}

/// GET /api/config/log-levels — 返回当前日志级别
pub async fn get_log_levels(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let settings = state.container.config.load_settings();
    Ok(data(serde_json::json!({ "level": settings.global.logging.level })))
}

#[derive(Deserialize)]
pub struct SetLogLevelBody {
    pub level: String,
}

/// PUT /api/config/log-level — 设置日志级别
pub async fn set_log_level(
    State(state): State<AppState>,
    Json(body): Json<SetLogLevelBody>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = state.container.config.load_settings();
    settings.global.logging.level = body.level.clone();
    state.container.config.save_settings(&settings).await?;
    Ok(data(body.level))
}

/// GET /api/config/default-stealth-script — 默认反检测脚本
pub async fn get_default_stealth_script(
    State(_state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(data(serde_json::json!({
        "script": r#"// Campus-Auth 默认反检测脚本
// 隐藏 webdriver 属性、伪造 plugins/mimeTypes、覆盖 navigator 检测点

(() => {
    // 隐藏 navigator.webdriver
    Object.defineProperty(navigator, 'webdriver', { get: () => false });

    // 伪造 chrome.runtime（防止 "not found" 检测）
    window.chrome = {
        runtime: {},
        loadTimes: () => {},
        csi: () => {},
        app: {},
    };

    // 伪造 plugins（空数组会触发反自动化检测）
    Object.defineProperty(navigator, 'plugins', {
        get: () => [1, 2, 3, 4, 5],
    });

    // 伪造 mimeTypes
    Object.defineProperty(navigator, 'mimeTypes', {
        get: () => [1, 2, 3, 4, 5],
    });

    // 覆盖 permissions.query（防止指纹）
    const origQuery = window.navigator.permissions.query;
    window.navigator.permissions.query = (parameters) => (
        parameters.name === 'notifications'
            ? Promise.resolve({ state: Notification.permission })
            : origQuery(parameters)
    );

    // 覆盖 Headless 检测 API
    Object.defineProperty(navigator, 'languages', { get: () => ['zh-CN', 'zh', 'en'] });
    Object.defineProperty(navigator, 'platform', { get: () => 'Win32' });
})();"#
    })))
}

/// GET /api/pure-mode — 获取纯净模式状态
pub async fn get_pure_mode(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let settings = state.container.config.load_settings();
    Ok(data(serde_json::json!({ "enabled": settings.global.browser.pure_mode })))
}

/// POST /api/pure-mode — 切换纯净模式（toggle，无需请求体）
///
/// 前端不发送请求体，后端读取当前值取反后保存。
pub async fn set_pure_mode(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let mut settings = state.container.config.load_settings();
    let new_enabled = !settings.global.browser.pure_mode;
    settings.global.browser.pure_mode = new_enabled;
    state.container.config.save_settings(&settings).await?;
    Ok(data(serde_json::json!({ "enabled": new_enabled, "message": "纯净模式已切换" })))
}

/// 后端 MonitorSettings → 前端 MonitorConfig 字段映射
///
/// 后端字段：tcp_enabled/tcp_targets/http_enabled/http_targets/url_enabled/url_targets/url_expected_responses/...
/// 前端字段：enable_tcp_check/ping_targets/enable_http_check/test_urls/url_check_urls/...
/// url_check_urls 格式："url|expected_response"（合并 url_targets + url_expected_responses）
fn monitor_backend_to_frontend(m: &crate::config::MonitorSettings) -> Value {
    // 合并 url_targets + url_expected_responses → url_check_urls ("url|expected" 格式)
    let url_check_urls: Vec<String> = m
        .url_targets
        .iter()
        .map(|url| {
            match m.url_expected_responses.get(url) {
                Some(expected) => format!("{}|{}", url, expected),
                None => url.clone(),
            }
        })
        .collect();

    serde_json::json!({
        "check_interval_seconds": m.check_interval,
        "network_check_timeout": m.tcp_timeout,
        "ping_targets": m.tcp_targets,
        "enable_tcp_check": m.tcp_enabled,
        "enable_http_check": m.http_enabled,
        "enable_local_check": false,
        "test_urls": m.http_targets,
        "check_auth_url": false,
        "auth_url_targets": [],
        "url_check_urls": url_check_urls,
        "script_timeout": 60,
        "bind_interface_name": m.bind_interface_name,
    })
}

/// 前端 MonitorConfig → 后端 MonitorSettings 字段映射
///
/// 拆分 url_check_urls ("url|expected" 格式) → url_targets + url_expected_responses
fn monitor_frontend_to_backend(v: &Value) -> Value {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return v.clone(),
    };

    // 拆分 url_check_urls → url_targets + url_expected_responses
    let mut url_targets: Vec<String> = Vec::new();
    let mut url_expected_responses: serde_json::Map<String, Value> = serde_json::Map::new();
    if let Some(urls) = obj.get("url_check_urls").and_then(|x| x.as_array()) {
        for entry in urls {
            if let Some(s) = entry.as_str() {
                if let Some((url, expected)) = s.split_once('|') {
                    url_targets.push(url.trim().to_string());
                    url_expected_responses
                        .insert(url.trim().to_string(), Value::String(expected.trim().to_string()));
                } else {
                    url_targets.push(s.trim().to_string());
                }
            }
        }
    }

    serde_json::json!({
        "enabled": obj.get("enable_tcp_check").or_else(|| obj.get("enable_http_check")).and_then(|v| v.as_bool()).unwrap_or(true),
        "check_interval": obj.get("check_interval_seconds").and_then(|v| v.as_u64()).unwrap_or(300),
        "tcp_targets": obj.get("ping_targets").cloned().unwrap_or(serde_json::json!([])),
        "http_targets": obj.get("test_urls").cloned().unwrap_or(serde_json::json!([])),
        "url_targets": serde_json::json!(url_targets),
        "url_expected_responses": Value::Object(url_expected_responses),
        "tcp_enabled": obj.get("enable_tcp_check").and_then(|v| v.as_bool()).unwrap_or(false),
        "http_enabled": obj.get("enable_http_check").and_then(|v| v.as_bool()).unwrap_or(false),
        "url_enabled": obj.get("url_check_urls").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false),
        "profile_check_interval": 180,
        "tcp_timeout": obj.get("network_check_timeout").and_then(|v| v.as_u64()).unwrap_or(5),
        "http_timeout": 10,
        "url_timeout": 10,
        "auth_url_timeout": 5,
        "bind_interface_name": obj.get("bind_interface_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

/// 浅合并：将 patch 中的所有 key 递归覆盖到 target
fn json_merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    json_merge(t.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        (t, p) => *t = p.clone(),
    }
}
