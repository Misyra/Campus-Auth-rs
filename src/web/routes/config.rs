//! 配置路由：全局设置读写、日志级别、纯净模式
//!
//! M1 细粒度 state（config 域）：handler 声明 `State<Arc<dyn ConfigApi>>` 依赖，
//! 不再触达 `state.container`（patch_settings 凭据保存经
//! `State<Arc<dyn ProfileApi>>` 提取）。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::Value;

use crate::config::{ConfigApi, ProfileApi};
use crate::web::error::{ApiError, data};

/// GET /api/config — 获取当前全局设置
///
/// 返回扁平结构，前端期望的格式：
/// { browser, monitor, pause, logging, retry, app_settings, credentials, active_task }
/// monitor 字段做后端→前端字段名映射
pub async fn get_settings(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    let profile = match config.load_profile(&settings.active_profile_id) {
        Ok(p) => p,
        Err(e) => {
            // 活跃 Profile 加载失败时回退空凭据（与原 unwrap_or_default 同语义），warn 留痕
            tracing::warn!(
                profile_id = %settings.active_profile_id,
                "活跃 Profile 加载失败，返回空凭据: {e}"
            );
            crate::config::ProfileData::default()
        }
    };
    let has_password = effective_has_password(config.as_ref(), &profile);
    Ok(data(settings_flat_response(
        &settings,
        &profile,
        has_password,
    )))
}

/// PATCH /api/config — 局部更新全局设置（合并后保存）
///
/// 前端发送扁平结构 { browser, monitor, pause, logging, retry, app_settings, ... }
/// 后端 SettingsData 结构为 { global: { browser, monitor, ... }, active_profile_id, ... }
/// 扁平 key → global 子结构 / 活跃 Profile 的映射由
/// [`apply_flat_settings_patch`] 统一实现（与 PUT 共用）
pub async fn patch_settings(
    State(config): State<Arc<dyn ConfigApi>>,
    State(profiles): State<Arc<dyn ProfileApi>>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    apply_flat_settings_patch(&config, &profiles, &patch).await?;
    reload_and_flat_response(&config).await
}

/// 将前端扁平 patch 应用并保存（PATCH /api/config）
///
/// 凭证字段（username/password/auth_url/isp/active_task）直接写入活跃 Profile；
/// 全局设置经 [`ConfigApi::modify_settings_tx`] 的提交事务落盘——「读取→合并→
/// 校验→持久化」在同一 `settings_lock` 临界区内完成。历史实现锁外读取合并
/// 整份设置再 `save_settings`（仅锁最终写入），两个并发修改不同字段的请求
/// 会相互覆盖（丢更新）。
async fn apply_flat_settings_patch(
    config: &Arc<dyn ConfigApi>,
    profiles: &Arc<dyn ProfileApi>,
    patch: &Value,
) -> Result<(), ApiError> {
    let Some(obj) = patch.as_object() else {
        // 非对象 patch：无字段可合并（与旧实现一致，不落盘直接成功）
        return Ok(());
    };

    let mut global_patch = serde_json::Map::new();
    let mut profile_patch = serde_json::Map::new();
    let mut other_patch = serde_json::Map::new();
    // 记录变更字段名（仅字段名，绝不记录值：payload 可能含密码/密钥）
    let mut changed_fields: Vec<String> = Vec::new();

    // 前端字段名 → 后端字段名映射
    let field_map: std::collections::HashMap<&str, &str> =
        [("retry", "retry_settings"), ("app_settings", "app")]
            .into_iter()
            .collect();

    // 凭证字段属于 Profile 而非全局设置
    let profile_keys = [
        "username",
        "password",
        "auth_url",
        "trigger_url",
        "isp",
        "carrier_custom",
        "active_task",
    ];

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
        if k == "carrier_custom" {
            // 纯前端展示字段（自定义运营商输入框），后端无对应存储；
            // 实际运营商名已由 `isp` 字段承载。显式忽略，避免落入 other_patch 污染 settings.json。
            continue;
        }
        changed_fields.push(k.clone());
        if profile_keys.contains(&k.as_str()) {
            profile_patch.insert(k.clone(), v.clone());
        } else if k == "monitor" {
            // monitor 字段通过 Option<T> DTO 区分“缺省”和显式值；未知字段、
            // 类型错误在进入合并前拒绝，避免缺字段被硬编码默认值覆盖。
            let backend_monitor = monitor_frontend_to_backend(v)?;
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

    // 端口范围硬校验（对齐 v5 Pydantic ge=1 le=65535 口径）：serde 的 u16 只保证
    // 类型，port=0 落盘后重启将永远无法按配置端口监听（Linux 非 root/Docker 直接
    // 绑定失败起不来），必须在保存前拦下；<1024 特权端口不强制拒绝（Windows 无
    // 特权概念、Docker 可能绑 80）
    if let Some(port) = global_patch.get("app").and_then(|app| app.get("port")) {
        let valid = port.as_u64().is_some_and(|p| (1..=65535).contains(&p));
        if !valid {
            return Err(ApiError::BadRequest("端口范围必须在 1-65535 之间".into()));
        }
    }

    // 先在内存中构造待提交 Profile；若同一请求还包含全局字段，必须等全局合并校验
    // 成功后由 ConfigService 双域事务一起落盘，禁止先写凭证形成半提交。
    let profile_to_save = if !profile_patch.is_empty() {
        let active_id = match obj.get("active_profile_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => config.load_settings_async().await.active_profile_id,
        };
        // Profile 加载失败必须显式报错：旧实现 if let Ok 静默丢弃整个
        // profile_patch 仍返回成功，用户以为密码已保存实际未生效。
        let mut profile = config.load_profile(&active_id).map_err(|e| {
            ApiError::BadRequest(format!(
                "Profile {active_id} 加载失败（{e}），凭证修改未生效，请重试"
            ))
        })?;
        if let Some(username) = profile_patch.get("username").and_then(|v| v.as_str()) {
            profile.username = username.to_string();
        }
        if let Some(auth_url) = profile_patch.get("auth_url").and_then(|v| v.as_str()) {
            let trimmed = auth_url.trim();
            if !trimmed.is_empty() {
                validate_auth_url(trimmed)?;
            }
            profile.auth_url = trimmed.to_string();
        }
        if let Some(trigger_url) = profile_patch.get("trigger_url").and_then(|v| v.as_str()) {
            let trimmed = trigger_url.trim();
            if !trimmed.is_empty() {
                validate_trigger_url(trimmed)?;
            }
            profile.trigger_url = trimmed.to_string();
        }
        if let Some(isp) = profile_patch.get("isp").and_then(|v| v.as_str()) {
            profile.isp = isp.to_string();
        }
        if let Some(active_task) = profile_patch.get("active_task").and_then(|v| v.as_str()) {
            profile.active_task = active_task.to_string();
        }
        if let Some(password) = profile_patch.get("password") {
            // 全局设置页使用三态契约：null 保留、空串清除、非空字符串加密更新。
            // Profile 编辑接口仍沿用其既有的“空串保留”语义，避免改变旧客户端行为。
            match password {
                Value::Null => {}
                Value::String(pwd_str) if pwd_str.is_empty() => profile.password.clear(),
                Value::String(pwd_str) => {
                    profile.password = profiles.save_password(Some(pwd_str), &profile.password);
                }
                _ => return Err(ApiError::BadRequest("password 必须是字符串或 null".into())),
            }
        }
        Some(profile)
    } else {
        None
    };

    // 全局设置合并：提交事务（持锁读-改-写，闭包失败不落盘）
    if !global_patch.is_empty() || !other_patch.is_empty() {
        // 空否以前置 Map 判定为准：Value::Object 包裹后 as_object() 恒为 Some，
        // 此处不再 unwrap（此前写法正确但制造 panic 观感）
        let global_empty = global_patch.is_empty();
        let other_empty = other_patch.is_empty();
        let global_patch = Value::Object(global_patch);
        let other_patch = Value::Object(other_patch);
        let merge = Box::new(move |settings| {
            let mut current_value =
                serde_json::to_value(&settings).map_err(|e| format!("设置序列化失败: {e}"))?;
            // 合并 global 字段
            if !global_empty {
                if let Some(global) = current_value.get_mut("global") {
                    json_merge(global, &global_patch);
                }
            }
            // 合并其他字段（如 active_profile_id 等）
            if !other_empty {
                json_merge(&mut current_value, &other_patch);
            }
            serde_json::from_value(current_value).map_err(|e| format!("设置合并后校验失败: {e}"))
        });
        let result = match profile_to_save {
            Some(profile) => config.modify_settings_and_profile_tx(profile, merge).await,
            None => config.modify_settings_tx(merge).await,
        };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => return Err(ApiError::BadRequest(msg)),
            Err(e) => return Err(e.into()),
        }
    } else if let Some(profile) = profile_to_save {
        config.save_profile(&profile).await?;
    }

    // 保存成功后统一记录变更字段名列表（严禁记录字段值，尤其密码/密钥）
    if !changed_fields.is_empty() {
        tracing::info!(fields = %changed_fields.join(","), "设置已保存");
    }

    Ok(())
}

/// reload → 回读设置与活跃 Profile → 构造扁平响应（PUT / PATCH 共用）
///
/// 与 GET /api/config 响应字节保持一致（现有前端契约护航）。
/// 设置已在 [`apply_flat_settings_patch`] 的提交事务内落盘，此处只负责
/// 发布（reload 触发 RuntimeConfig 替换 + 配置版本广播）与回读。
async fn reload_and_flat_response(config: &Arc<dyn ConfigApi>) -> Result<Json<Value>, ApiError> {
    config.reload().await?;
    let settings = config.load_settings_async().await;
    let profile = config
        .load_profile(&settings.active_profile_id)
        .unwrap_or_default();
    let has_password = effective_has_password(config.as_ref(), &profile);
    Ok(data(settings_flat_response(
        &settings,
        &profile,
        has_password,
    )))
}

/// 构造设置扁平响应（GET / PATCH /api/config 共用）
///
/// 字段顺序与历史响应完全一致；monitor 字段做后端→前端字段名映射
fn settings_flat_response(
    settings: &crate::config::SettingsData,
    profile: &crate::config::ProfileData,
    has_password: bool,
) -> Value {
    serde_json::json!({
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
        "trigger_url": profile.trigger_url,
        "isp": profile.isp,
        "carrier_custom": "",
        "active_task": profile.active_task,
        "has_password": has_password
    })
}

/// 计算 has_password：必须反映「密码可用」（能解密），而非仅「字段非空」
///
/// 否则密钥变更/格式不兼容时，前端误认为已保存 → 不重新输入 →
/// 登录报缺少 password；刚保存显示成功、刷新又提示需重输，体验割裂。
fn effective_has_password(config: &dyn ConfigApi, profile: &crate::config::ProfileData) -> bool {
    if profile.password.is_empty() {
        false
    } else {
        config.can_decrypt_password(&profile.password)
    }
}

/// POST /api/config/reload — 重新加载配置
pub async fn reload_settings(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    config.reload().await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/config/log-levels — 返回当前日志级别
pub async fn get_log_levels(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    Ok(data(
        serde_json::json!({ "level": settings.global.logging.level }),
    ))
}

#[derive(Deserialize)]
pub struct SetLogLevelBody {
    pub level: String,
}

/// PUT /api/config/log-level — 设置日志级别
pub async fn set_log_level(
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<SetLogLevelBody>,
) -> Result<Json<Value>, ApiError> {
    let level = match body.level.trim().to_ascii_uppercase().as_str() {
        "TRACE" => "TRACE",
        "DEBUG" => "DEBUG",
        "INFO" => "INFO",
        "WARN" | "WARNING" => "WARN",
        "ERROR" => "ERROR",
        _ => {
            return Err(ApiError::BadRequest(
                "日志级别仅支持 TRACE、DEBUG、INFO、WARN、ERROR".into(),
            ));
        }
    }
    .to_string();
    // 持锁读-改-写：锁外的 load→改→save 会丢并发的其他字段更新
    let persisted_level = level.clone();
    match config
        .modify_settings_tx(Box::new(move |mut s| {
            s.global.logging.level = persisted_level;
            Ok(s)
        }))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(ApiError::BadRequest(msg)),
        Err(e) => return Err(e.into()),
    }
    // 热更新运行时日志级别（tracing filter），而非仅落盘下次启动生效
    crate::logging::reload_log_level(&level);
    tracing::info!(level = %level, "日志级别已更新");
    Ok(data(level))
}

/// GET /api/config/default-stealth-script — 默认反检测脚本
pub async fn get_default_stealth_script() -> Result<Json<Value>, ApiError> {
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
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    Ok(data(
        serde_json::json!({ "enabled": settings.global.browser.pure_mode }),
    ))
}

/// POST /api/pure-mode — 切换纯净模式（toggle，无需请求体）
///
/// 前端不发送请求体，后端读取当前值取反后保存。
pub async fn set_pure_mode(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // 持锁读-改-写：并发请求各自读到相同旧值取反会互相覆盖（两次 toggle 终值不变）
    match config
        .modify_settings_tx(Box::new(|mut s| {
            s.global.browser.pure_mode = !s.global.browser.pure_mode;
            Ok(s)
        }))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(ApiError::BadRequest(msg)),
        Err(e) => return Err(e.into()),
    }
    // 回读终值（并发 toggle 下以落盘结果为准）
    let new_enabled = config.load_settings_async().await.global.browser.pure_mode;
    Ok(data(
        serde_json::json!({ "enabled": new_enabled, "message": "纯净模式已切换" }),
    ))
}

/// 后端 MonitorSettings → 前端 MonitorConfig 字段映射
///
/// 后端字段：tcp_enabled/tcp_targets/http_enabled/http_targets/url_enabled/url_targets/url_expected_responses/...
/// 前端字段：enable_tcp_check/ping_targets/enable_http_check/test_urls/enable_url_check/url_check_urls/...
/// url_check_urls 格式："url|expected_response"（合并 url_targets + url_expected_responses）
fn monitor_backend_to_frontend(m: &crate::config::MonitorSettings) -> Value {
    // 合并 url_targets + url_expected_responses → url_check_urls ("url|expected" 格式)
    let url_check_urls: Vec<String> = m
        .url_targets
        .iter()
        .map(|url| match m.url_expected_responses.get(url) {
            Some(expected) => format!("{}|{}", url, expected),
            None => url.clone(),
        })
        .collect();

    serde_json::json!({
        "check_interval_seconds": m.check_interval,
        "network_check_timeout": m.tcp_timeout,
        "ping_targets": m.tcp_targets,
        "enable_tcp_check": m.tcp_enabled,
        "enable_http_check": m.http_enabled,
        "test_urls": m.http_targets,
        "enable_url_check": m.url_enabled,
        "check_auth_url": m.check_auth_url,
        "auth_url_targets": [],
        "url_check_urls": url_check_urls,
        "enable_local_check": m.local_check_enabled,
        "disable_proxy": m.disable_proxy,
        "script_timeout": 60,
        "post_login_delay": m.post_login_delay,
    })
}

/// 前端 MonitorConfig 的类型化局部更新 DTO
///
/// 所有字段均为 `Option<T>`：缺省/null 表示不修改，`false`/`0`/空数组仍是显式值。
/// `deny_unknown_fields` 阻止误传后端字段名后被静默忽略。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorPatch {
    check_interval_seconds: Option<u64>,
    ping_targets: Option<Vec<String>>,
    test_urls: Option<Vec<String>>,
    url_check_urls: Option<Vec<String>>,
    enable_tcp_check: Option<bool>,
    enable_http_check: Option<bool>,
    enable_url_check: Option<bool>,
    enable_local_check: Option<bool>,
    disable_proxy: Option<bool>,
    network_check_timeout: Option<u64>,
    post_login_delay: Option<u64>,
    check_auth_url: Option<bool>,
    // GET 响应的往返保真字段：前端会原样回传，后端有意不存储。
    auth_url_targets: Option<Vec<String>>,
    script_timeout: Option<u64>,
}

impl MonitorPatch {
    /// 转换为后端 MonitorSettings 的局部 JSON patch，仅输出实际提供的字段
    fn into_backend_patch(self) -> Value {
        let mut backend = serde_json::Map::new();
        if let Some(value) = self.check_interval_seconds {
            backend.insert("check_interval".into(), Value::from(value));
        }
        if let Some(value) = self.ping_targets {
            backend.insert("tcp_targets".into(), serde_json::json!(value));
        }
        if let Some(value) = self.test_urls {
            backend.insert("http_targets".into(), serde_json::json!(value));
        }

        // 拆分 url_check_urls → url_targets + url_expected_responses。旧客户端省略
        // enable_url_check 时，仅在实际携带列表的情况下沿用“非空即启用”兼容语义。
        let derived_url_enabled = self.url_check_urls.as_ref().map(|items| !items.is_empty());
        if let Some(urls) = self.url_check_urls {
            let mut targets = Vec::with_capacity(urls.len());
            let mut expected = serde_json::Map::new();
            for raw in urls {
                if let Some((url, response)) = raw.split_once('|') {
                    let url = url.trim().to_string();
                    targets.push(url.clone());
                    expected.insert(url, Value::String(response.trim().to_string()));
                } else {
                    targets.push(raw.trim().to_string());
                }
            }
            backend.insert("url_targets".into(), serde_json::json!(targets));
            backend.insert("url_expected_responses".into(), Value::Object(expected));
        }
        if let Some(value) = self.enable_url_check.or(derived_url_enabled) {
            backend.insert("url_enabled".into(), Value::from(value));
        }
        if let Some(value) = self.enable_tcp_check {
            backend.insert("tcp_enabled".into(), Value::from(value));
        }
        if let Some(value) = self.enable_http_check {
            backend.insert("http_enabled".into(), Value::from(value));
        }
        if let Some(value) = self.enable_local_check {
            backend.insert("local_check_enabled".into(), Value::from(value));
        }
        if let Some(value) = self.disable_proxy {
            backend.insert("disable_proxy".into(), Value::from(value));
        }
        if let Some(value) = self.network_check_timeout {
            backend.insert("tcp_timeout".into(), Value::from(value));
        }
        if let Some(value) = self.post_login_delay {
            backend.insert("post_login_delay".into(), Value::from(value));
        }
        if let Some(value) = self.check_auth_url {
            backend.insert("check_auth_url".into(), Value::from(value));
        }

        // 显式消费保真字段，表明它们经过类型校验但不进入后端配置。
        let _ = (self.auth_url_targets, self.script_timeout);
        Value::Object(backend)
    }
}

/// 前端 MonitorConfig → 后端 MonitorSettings 局部 patch
fn monitor_frontend_to_backend(value: &Value) -> Result<Value, ApiError> {
    let patch: MonitorPatch = serde_json::from_value(value.clone())
        .map_err(|error| ApiError::BadRequest(format!("monitor 配置无效: {error}")))?;
    Ok(patch.into_backend_patch())
}

/// 校验认证地址：仅 http/https，已通过 DNS 钉扎防护的内网/保留地址需前置拒收
fn validate_auth_url(url: &str) -> Result<(), ApiError> {
    validate_http_url(url, "认证地址")
}

/// 校验重定向触发地址：与认证地址同口径（仅 http/https + 主机名），标签区分报错文案
fn validate_trigger_url(url: &str) -> Result<(), ApiError> {
    validate_http_url(url, "重定向触发地址")
}

/// http/https URL 通用校验（认证地址与触发地址共用，G2 单点语义）
fn validate_http_url(url: &str, label: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| ApiError::BadRequest(format!("{label}格式非法: {url}")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(ApiError::BadRequest(format!(
                "{label}仅支持 http/https，当前为: {}",
                parsed.scheme()
            )));
        }
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest(format!("{label}缺少主机名: {url}")))?;
    if host.is_empty() {
        return Err(ApiError::BadRequest(format!("{label}缺少主机名: {url}")));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    // ============ monitor 前后端字段映射（往返一致性） ============

    fn sample_monitor() -> crate::config::MonitorSettings {
        let mut url_expected = std::collections::HashMap::new();
        url_expected.insert("http://a.com".to_string(), "OK".to_string());
        crate::config::MonitorSettings {
            check_interval: 120,
            tcp_targets: vec!["8.8.8.8:53".into()],
            http_targets: vec!["http://b.com".into()],
            url_targets: vec!["http://a.com".into()],
            url_expected_responses: url_expected,
            tcp_enabled: true,
            http_enabled: false,
            url_enabled: true,
            local_check_enabled: false,
            disable_proxy: true,
            profile_check_interval: 300,
            tcp_timeout: 5,
            http_timeout: 5,
            url_timeout: 5,
            auth_url_timeout: 5,
            check_auth_url: false,
            post_login_delay: 5,
        }
    }

    #[test]
    fn monitor_backend_to_frontend_maps_url_check_urls() {
        let front = monitor_backend_to_frontend(&sample_monitor());
        // url_targets + 期望响应 → "url|expected" 合并
        assert_eq!(
            front["url_check_urls"],
            serde_json::json!(["http://a.com|OK"])
        );
        assert_eq!(front["check_interval_seconds"], 120);
        assert_eq!(front["ping_targets"], serde_json::json!(["8.8.8.8:53"]));
        assert_eq!(front["enable_tcp_check"], serde_json::json!(true));
        assert_eq!(front["enable_url_check"], serde_json::json!(true));
    }

    #[test]
    fn monitor_backend_to_frontend_handles_url_without_expected() {
        // url 无期望响应时，仅保留 url 本身
        let m = crate::config::MonitorSettings {
            url_expected_responses: Default::default(),
            ..sample_monitor()
        };
        let front = monitor_backend_to_frontend(&m);
        assert_eq!(front["url_check_urls"], serde_json::json!(["http://a.com"]));
    }

    #[test]
    fn monitor_frontend_to_backend_splits_url_check_urls() {
        let front = serde_json::json!({
            "enable_tcp_check": true,
            "check_interval_seconds": 60,
            "ping_targets": ["1.1.1.1:53"],
            "test_urls": ["http://c.com"],
            "enable_url_check": true,
            "url_check_urls": [" http://a.com | OK ", "http://d.com"],
            "network_check_timeout": 8,
            "post_login_delay": 3,
        });
        let back = monitor_frontend_to_backend(&front).unwrap();
        assert_eq!(
            back["url_targets"],
            serde_json::json!(["http://a.com", "http://d.com"])
        );
        assert_eq!(
            back["url_expected_responses"]["http://a.com"],
            serde_json::json!("OK")
        );
        assert!(back["url_expected_responses"].get("http://d.com").is_none());
        assert_eq!(back["tcp_enabled"], serde_json::json!(true));
        assert_eq!(back["url_enabled"], serde_json::json!(true));
        assert_eq!(back["check_interval"], serde_json::json!(60));
    }

    #[test]
    fn monitor_url_targets_do_not_implicitly_enable_probe() {
        let front = serde_json::json!({
            "enable_url_check": false,
            "url_check_urls": ["http://a.com|OK"]
        });
        let back = monitor_frontend_to_backend(&front).unwrap();
        assert_eq!(back["url_enabled"], serde_json::json!(false));
        assert_eq!(back["url_targets"], serde_json::json!(["http://a.com"]));
    }

    #[test]
    fn monitor_roundtrip_preserves_check_auth_url() {
        // 开关须真实往返：GET 给出后端值，PATCH 能写回（不再是被忽略的保真字段）
        let mut original = sample_monitor();
        assert!(!original.check_auth_url, "默认应关闭");
        assert_eq!(
            monitor_backend_to_frontend(&original)["check_auth_url"],
            serde_json::json!(false)
        );
        original.check_auth_url = true;
        let front = monitor_backend_to_frontend(&original);
        assert_eq!(front["check_auth_url"], serde_json::json!(true));
        let back = monitor_frontend_to_backend(&front).unwrap();
        assert_eq!(back["check_auth_url"], serde_json::json!(true));
    }

    #[test]
    fn monitor_frontend_to_backend_rejects_invalid_shape_and_unknown_fields() {
        assert!(monitor_frontend_to_backend(&serde_json::json!(42)).is_err());
        assert!(monitor_frontend_to_backend(&serde_json::json!({ "http_targets": [] })).is_err());
    }

    #[test]
    fn monitor_partial_patch_only_emits_supplied_fields() {
        let back = monitor_frontend_to_backend(&serde_json::json!({
            "enable_tcp_check": true
        }))
        .unwrap();
        assert_eq!(back, serde_json::json!({ "tcp_enabled": true }));
    }

    #[test]
    fn monitor_roundtrip_preserves_url_expected() {
        // backend → frontend → backend 应保持 url_targets 与期望响应
        let original = sample_monitor();
        let front = monitor_backend_to_frontend(&original);
        let back = monitor_frontend_to_backend(&front).unwrap();
        assert_eq!(back["url_targets"], serde_json::json!(["http://a.com"]));
        assert_eq!(
            back["url_expected_responses"]["http://a.com"],
            serde_json::json!("OK")
        );
    }

    // ============ json_merge ============

    #[test]
    fn json_merge_overrides_and_removes_keys() {
        let mut target = serde_json::json!({"a": 1, "b": {"x": 1, "y": 2}, "c": 3});
        let patch = serde_json::json!({"a": 99, "b": {"y": 20}, "c": null});
        json_merge(&mut target, &patch);
        assert_eq!(target["a"], 99);
        assert_eq!(target["b"]["x"], 1); // 未覆盖的子 key 保留
        assert_eq!(target["b"]["y"], 20);
        assert!(target.get("c").is_none()); // null 删除
    }

    #[test]
    fn json_merge_null_patch_removes_nested_key() {
        let mut target = serde_json::json!({"b": {"x": 1, "y": 2}});
        json_merge(&mut target, &serde_json::json!({"b": {"x": null}}));
        assert!(target["b"].get("x").is_none());
        assert_eq!(target["b"]["y"], 2);
    }

    #[test]
    fn json_merge_scalar_replaces_object() {
        let mut target = serde_json::json!({"a": {"nested": true}});
        json_merge(&mut target, &serde_json::json!({"a": 5}));
        assert_eq!(target["a"], 5);
    }

    // ============ handler 级单测（内存 MockConfigApi，M1） ============

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use crate::config::{ConfigError, ProfileData};

    #[derive(Default)]
    struct MockInner {
        settings: crate::config::SettingsData,
        profile: ProfileData,
        save_calls: usize,
        reload_calls: usize,
        /// 打开后 load_profile 返回错误，用于验证凭证写入失败路径（G16）
        profile_load_fails: bool,
    }

    /// 内存 ConfigApi：无需磁盘与完整 ServiceContainer
    struct MockConfigApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl ConfigApi for MockConfigApi {
        async fn load_settings_async(&self) -> crate::config::SettingsData {
            self.0.lock().unwrap().settings.clone()
        }

        async fn save_settings(
            &self,
            data: &crate::config::SettingsData,
        ) -> Result<(), ConfigError> {
            let mut inner = self.0.lock().unwrap();
            inner.settings = data.clone();
            inner.save_calls += 1;
            Ok(())
        }

        async fn modify_settings_tx(
            &self,
            f: Box<
                dyn FnOnce(
                        crate::config::SettingsData,
                    ) -> Result<crate::config::SettingsData, String>
                    + Send,
            >,
        ) -> Result<Result<(), String>, ConfigError> {
            let mut inner = self.0.lock().unwrap();
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
            let inner = self.0.lock().unwrap();
            if inner.profile_load_fails {
                return Err(ConfigError::ProfileNotFound { id: id.to_string() });
            }
            Ok(inner.profile.clone())
        }

        async fn save_profile(&self, profile: &ProfileData) -> Result<(), ConfigError> {
            self.0.lock().unwrap().profile = profile.clone();
            Ok(())
        }

        async fn reload(&self) -> Result<(), ConfigError> {
            self.0.lock().unwrap().reload_calls += 1;
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
            std::sync::Arc::new(test_runtime_config())
        }

        fn encrypt_password(&self, raw: &str) -> Result<String, ConfigError> {
            Ok(format!("ENC:mock:{raw}"))
        }
    }

    /// 构造测试用 RuntimeConfig（类型未派生 Default，字段逐个填充默认值）
    fn test_runtime_config() -> crate::config::RuntimeConfig {
        use crate::config::{
            AppSettings, BrowserSettings, LoggingSettings, MonitorSettings, PauseSettings,
            ProfileSnapshot, RetrySettings, RuntimeConfig, UpdaterSettings, WorkerSettings,
        };
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

    /// 内存 ProfileApi：patch/put 凭证路径仅消费 save_password
    struct MockProfileApi;

    #[async_trait::async_trait]
    impl ProfileApi for MockProfileApi {
        fn list_profiles(&self) -> Vec<crate::config::ProfileSummary> {
            Vec::new()
        }
        fn get_profile(&self, _id: &str) -> Result<ProfileData, ConfigError> {
            Err(ConfigError::ProfileNotFound { id: "mock".into() })
        }
        async fn create_profile(&self, _id: &str, _data: ProfileData) -> Result<(), ConfigError> {
            Ok(())
        }
        async fn update_profile(&self, _id: &str, _data: ProfileData) -> Result<(), ConfigError> {
            Ok(())
        }
        async fn delete_profile(&self, _id: &str) -> Result<(), ConfigError> {
            Ok(())
        }
        async fn switch_profile(&self, _id: &str) -> Result<(), ConfigError> {
            Ok(())
        }
        async fn set_auto_switch(&self, _enabled: bool) -> Result<(), ConfigError> {
            Ok(())
        }
        fn detect_matching_profile(&self, _gateway_ip: &str, _ssid: &str) -> Option<String> {
            None
        }
        fn save_password(&self, raw: Option<&str>, existing: &str) -> String {
            match raw {
                None | Some("") => existing.to_string(),
                Some(s) => format!("ENC:mock:{s}"),
            }
        }
    }

    /// 双域 state：ConfigApi + ProfileApi 各自经 FromRef 委派提取
    ///
    /// patch_settings 声明双 State 依赖（凭证写入活跃 Profile）
    #[derive(Clone)]
    struct PatchTestState {
        config: Arc<dyn ConfigApi>,
        profiles: Arc<dyn ProfileApi>,
    }

    impl axum::extract::FromRef<PatchTestState> for Arc<dyn ConfigApi> {
        fn from_ref(state: &PatchTestState) -> Self {
            state.config.clone()
        }
    }

    impl axum::extract::FromRef<PatchTestState> for Arc<dyn ProfileApi> {
        fn from_ref(state: &PatchTestState) -> Self {
            state.profiles.clone()
        }
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            settings: crate::config::SettingsData::default(),
            profile: ProfileData::default(),
            save_calls: 0,
            reload_calls: 0,
            profile_load_fails: false,
        }));
        let state = PatchTestState {
            config: Arc::new(MockConfigApi(inner.clone())),
            profiles: Arc::new(MockProfileApi),
        };
        let app = axum::Router::new()
            .route("/api/config", get(get_settings).patch(patch_settings))
            .route("/api/config/log-levels", get(get_log_levels))
            .route("/api/config/log-level", axum::routing::put(set_log_level))
            .route("/api/pure-mode", get(get_pure_mode).post(set_pure_mode))
            .with_state(state);
        (app, inner)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// GET 返回扁平结构与 has_password 计算字段（密码为空 → false）
    #[tokio::test]
    async fn test_get_settings_flat_shape() {
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.settings.active_profile_id = "default".into();
            g.profile.username = "user1".into();
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let d = &v["data"];
        assert_eq!(d["username"], "user1");
        assert_eq!(d["has_password"], false);
        // 扁平结构包含各域
        for key in [
            "browser",
            "monitor",
            "pause",
            "logging",
            "retry",
            "app_settings",
        ] {
            assert!(d.get(key).is_some(), "缺少字段 {key}");
        }
    }

    /// 日志级别读写往返
    #[tokio::test]
    async fn test_log_level_roundtrip() {
        let (app, inner) = mock_app();
        // 设置
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config/log-level")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"level": "debug"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().settings.global.logging.level, "DEBUG");
        // 读取
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/config/log-levels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(resp).await;
        assert_eq!(v["data"]["level"], "DEBUG");
    }

    /// 纯净模式 toggle 翻转
    #[tokio::test]
    async fn test_pure_mode_toggles() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pure-mode")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["enabled"], false);
        assert!(!inner.lock().unwrap().settings.global.browser.pure_mode);
    }

    // ============ updater 段：自动更新设置往返（channel/auto_check_enabled） ============

    /// updater 部分 patch 深合并：只改 channel 不得清空其他 updater 字段；
    /// 响应与 GET 同形回显新字段
    #[tokio::test]
    async fn test_patch_updater_merges_partially() {
        let (app, inner) = mock_app();
        // 预置非默认值：自定义代理与间隔
        {
            let mut g = inner.lock().unwrap();
            g.settings.global.updater.proxy_url = "http://192.168.1.5:7890".into();
            g.settings.global.updater.check_interval_hours = 168;
            g.settings.global.updater.use_proxy = true;
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "updater": { "channel": "prerelease" } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let u = &g.settings.global.updater;
        // 仅 channel 变更，其余字段保持原值（深合并而非整体替换）
        assert_eq!(u.channel, crate::config::UpdateChannel::Prerelease);
        assert_eq!(u.proxy_url, "http://192.168.1.5:7890");
        assert_eq!(u.check_interval_hours, 168);
        assert!(u.use_proxy);
        assert!(u.auto_check_enabled, "未指定的开关保持默认值");
    }

    /// 非法通道值在合并反序列化时被拒：返回 400 且不落盘
    #[tokio::test]
    async fn test_patch_updater_rejects_invalid_channel() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "updater": { "channel": "nightly" } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let g = inner.lock().unwrap();
        assert_eq!(g.save_calls, 0, "校验失败不得落盘");
    }

    /// auto_check_enabled 开关写入与回读
    #[tokio::test]
    async fn test_patch_updater_toggles_auto_check() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "updater": { "auto_check_enabled": false } })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let g = inner.lock().unwrap();
        assert!(!g.settings.global.updater.auto_check_enabled);
        // 响应回显（GET 同形）
        assert_eq!(v["data"]["updater"]["auto_check_enabled"], false);
        assert_eq!(v["data"]["updater"]["channel"], "stable");
    }

    // ============ patch_settings 扁平映射（双 state 提取，M1） ============

    /// 凭证字段路由到 Profile、密码走 save_password 语义、全局字段落 settings
    #[tokio::test]
    async fn test_patch_settings_routes_credentials_and_global() {
        let raw_password =
            std::env::var("TEST_PASSWORD").unwrap_or_else(|_| "test-password".into());
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "username": "alice",
                            "password": raw_password,
                            "isp": "cmcc",
                            "carrier_custom": "自定义显示",
                            "pause": { "enabled": true },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let d = &v["data"];
        // 凭证已写入 Profile，密码经 save_password 加密语义
        let g = inner.lock().unwrap();
        assert_eq!(g.profile.username, "alice");
        assert_eq!(g.profile.isp, "cmcc");
        assert_eq!(g.profile.password, format!("ENC:mock:{}", raw_password));
        // carrier_custom 是纯前端展示字段，不落盘
        // 全局字段保存 + reload
        assert!(g.settings.global.pause.enabled);
        assert_eq!(g.save_calls, 1);
        assert_eq!(g.reload_calls, 1);
        // 响应回显凭证与 has_password
        assert_eq!(d["username"], "alice");
        assert_eq!(d["isp"], "cmcc");
        assert_eq!(d["carrier_custom"], "");
        assert_eq!(d["has_password"], true);
    }

    // ============ B4：PATCH 扁平 payload 不得清空未指定字段 ============

    /// PATCH 扁平 payload：未指定字段保持原值（不清空），响应为扁平结构
    #[tokio::test]
    async fn test_patch_settings_flat_payload_keeps_unspecified_fields() {
        let (app, inner) = mock_app();
        // 预置非默认值：pause.enabled=true、monitor.check_interval=120、username=orig
        {
            let mut g = inner.lock().unwrap();
            g.settings.global.pause.enabled = true;
            g.settings.global.monitor.check_interval = 120;
            g.profile.username = "orig".into();
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "username": "alice" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let d = &v["data"];
        let g = inner.lock().unwrap();
        // 未指定字段保持原值：旧实现会被 serde default 清成默认值
        assert!(g.settings.global.pause.enabled, "pause.enabled 不应被清空");
        assert_eq!(
            g.settings.global.monitor.check_interval, 120,
            "monitor.check_interval 不应被清空"
        );
        // 指定字段生效：username 写入 Profile
        assert_eq!(g.profile.username, "alice");
        // 仅含 Profile 字段的 payload 不触发 settings 落盘（无可合并的全局字段，
        // 旧实现会做一次无变化的 save），但 reload 照常发布
        assert_eq!(g.save_calls, 0);
        assert_eq!(g.reload_calls, 1);
        // 响应与 GET/PATCH 同形（扁平结构 + 回显凭证）
        assert_eq!(d["username"], "alice");
        assert_eq!(d["pause"]["enabled"], true);
        for key in [
            "browser",
            "monitor",
            "logging",
            "retry",
            "app_settings",
            "worker",
            "updater",
        ] {
            assert!(d.get(key).is_some(), "PATCH 响应缺少扁平字段 {key}");
        }
        assert!(
            d.get("global").is_none(),
            "PATCH 响应不应再返回嵌套 SettingsData 结构"
        );
    }

    /// monitor 局部 PATCH 只改显式字段，不用历史默认值覆盖同域其他配置。
    #[tokio::test]
    async fn test_patch_monitor_keeps_unspecified_fields() {
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.settings.global.monitor.disable_proxy = false;
            g.settings.global.monitor.check_interval = 77;
            g.settings.global.monitor.http_enabled = true;
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "monitor": { "enable_tcp_check": true }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let monitor = &g.settings.global.monitor;
        assert!(monitor.tcp_enabled);
        assert!(monitor.http_enabled, "未指定的 HTTP 探针开关应保留");
        assert!(!monitor.disable_proxy, "未指定的代理设置应保留");
        assert_eq!(monitor.check_interval, 77, "未指定的检查间隔应保留");
    }

    /// PATCH 不合法字段值返回 400（类型不匹配在合并反序列化时暴露）
    #[tokio::test]
    async fn test_patch_settings_rejects_invalid_field_value() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "pause": { "enabled": "not-a-bool" } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 校验失败时不应落盘
        assert_eq!(inner.lock().unwrap().save_calls, 0);
    }

    /// 端口硬校验：port=0 / 越界值返回 400 且不落盘，合法值（含前端字段名
    /// app_settings 映射到 app）正常保存
    #[tokio::test]
    async fn test_patch_settings_rejects_invalid_port() {
        for invalid in [0, 65536, -1] {
            let (app, inner) = mock_app();
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri("/api/config")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({ "app_settings": { "port": invalid } }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "port={invalid} 应被拒绝"
            );
            assert_eq!(
                inner.lock().unwrap().save_calls,
                0,
                "port={invalid} 校验失败不应落盘"
            );
        }

        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "app_settings": { "port": 8080 } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().settings.global.app.port, 8080);
    }

    /// 同一请求含凭证与非法全局字段时，两域均不得落盘。
    #[tokio::test]
    async fn test_patch_settings_invalid_global_does_not_partially_save_profile() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "username": "should-not-persist",
                            "pause": { "enabled": "not-a-bool" },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let state = inner.lock().unwrap();
        assert_eq!(state.profile.username, "");
        assert_eq!(state.save_calls, 0);
    }

    // ============ G16：profile 加载失败不得静默丢弃凭证修改 ============

    /// 携带凭证字段的 PATCH 在 Profile 加载失败时返回 400，且全局设置不落盘
    #[tokio::test]
    async fn test_patch_settings_reports_profile_load_failure() {
        let raw_password =
            std::env::var("TEST_PASSWORD").unwrap_or_else(|_| "test-password".into());
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.profile_load_fails = true;
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "username": "alice",
                            "password": raw_password,
                            "pause": { "enabled": true },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("加载失败"),
            "错误消息应指明 profile 加载失败: {msg}"
        );
        assert!(msg.contains("凭证"), "错误消息应说明凭证修改未生效: {msg}");
        // 凭证与全局设置均未落盘（不出现“部分成功”）
        let g = inner.lock().unwrap();
        assert_eq!(g.profile.username, "");
        assert_eq!(g.profile.password, "");
        assert!(!g.settings.global.pause.enabled);
        assert_eq!(g.save_calls, 0);
    }
}
