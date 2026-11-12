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

use crate::config::{ConfigApi, ProfileApi, ProfileMergeFn, SettingsMergeFn};
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};
use crate::web::routes::profiles::validate_login_task_binding;

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
    State(tasks): State<Arc<dyn TaskApi>>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    apply_flat_settings_patch(&config, &profiles, &tasks, &patch).await?;
    reload_and_flat_response(&config).await
}

/// 将前端扁平 patch 应用并保存（PATCH /api/config）
///
/// 凭证字段（username/password/auth_url/isp/active_task/active_http_task）直接写入
/// 活跃 Profile；全局设置经 [`ConfigApi::modify_settings_tx`] 的提交事务落盘——
/// 「读取→合并→校验→持久化」在同一 `settings_lock` 临界区内完成。历史实现锁外读取
/// 合并整份设置再 `save_settings`（仅锁最终写入），两个并发修改不同字段的请求
/// 会相互覆盖（丢更新）。
///
/// `tasks` 仅用于校验直连任务绑定（见 [`validate_login_task_binding`]）——这里不复用
/// `ProfileApi`，因为绑定校验是**任务域的事实**，与方案业务无关。
/// 扁平 patch 按 key 分类后的产物（[`split_patch_keys`] 输出）
struct FlatPatchParts {
    global_patch: serde_json::Map<String, Value>,
    profile_patch: serde_json::Map<String, Value>,
    other_patch: serde_json::Map<String, Value>,
    /// 变更字段名（仅字段名，绝不记录值：payload 可能含密码/密钥）
    changed_fields: Vec<String>,
}

/// 按前端字段名把扁平 patch 分拣到 global / Profile / 其它三个桶
///
/// monitor 字段在此完成前端→后端 DTO 映射；未知字段进 other 桶由合并闭包
/// 写到 settings 顶层（历史字段 `carrier_custom` 显式忽略）。
fn split_patch_keys(obj: &serde_json::Map<String, Value>) -> Result<FlatPatchParts, ApiError> {
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
        "network",
        "username",
        "password",
        "auth_url",
        "trigger_url",
        "isp",
        "active_task",
        // 登录渠道与「渠道 → 任务」绑定：GET 扁平响应会回传，客户端原样回传时必须落回
        // Profile；否则落入 other_patch 被 json_merge 写到 settings 顶层成脏数据。
        // 注意直连的**请求参数**已不在方案里（搬进 `tasks/http/<id>.json`），
        // 这里只剩渠道与两条绑定。
        "login_channel",
        "active_http_task",
        "active_script_task",
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
        "notification",
    ];

    for (k, v) in obj {
        if k == "carrier_custom" {
            // 历史字段（v5 及以前的「自定义运营商」文本，v6 起由 `isp` 单字段承载）。
            // 后端已无对应存储，但旧客户端仍可能原样回传：显式忽略，
            // 避免落入 other_patch 被 json_merge 写到 settings 顶层成脏数据。
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

    Ok(FlatPatchParts {
        global_patch,
        profile_patch,
        other_patch,
        changed_fields,
    })
}

/// 构造活跃 Profile 的合并事务（含 WEB-7 字段类型预检与活跃 Profile 预检）
///
/// `profile_patch` 为空时返回 `None`（无可提交内容）。
async fn build_profile_tx(
    config: &Arc<dyn ConfigApi>,
    profiles: &Arc<dyn ProfileApi>,
    tasks: &Arc<dyn TaskApi>,
    profile_patch: serde_json::Map<String, Value>,
    active_profile_id: Option<&str>,
) -> Result<Option<(String, ProfileMergeFn)>, ApiError> {
    if profile_patch.is_empty() {
        return Ok(None);
    }

    // WEB-7：profile 字段类型错误显式 400（与 password 口径对齐），不再静默
    // 跳过——静默丢字段会让用户误以为已保存
    for key in [
        "username",
        "auth_url",
        "trigger_url",
        "isp",
        "active_task",
        "active_http_task",
        "active_script_task",
    ] {
        if profile_patch.get(key).is_some_and(|v| !v.is_string()) {
            return Err(ApiError::BadRequest(format!("{key} 必须是字符串")));
        }
    }

    let active_id = match active_profile_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => config.load_settings_async().await.active_profile_id,
    };
    // Profile 加载失败必须显式报错：旧实现 if let Ok 静默丢弃整个
    // profile_patch 仍返回成功，用户以为密码已保存实际未生效。
    // （事务内会在锁内重新加载，此处预检仅为保留既有的 400 文案口径）
    config.load_profile(&active_id).map_err(|e| {
        ApiError::BadRequest(format!(
            "Profile {active_id} 加载失败（{e}），凭证修改未生效，请重试"
        ))
    })?;

    let profiles = profiles.clone();
    let tasks = tasks.clone();
    Ok(Some((
        active_id,
        Box::new(move |mut profile| {
            let tasks = tasks.clone();
            Box::pin(async move {
                if let Some(network) = profile_patch.get("network") {
                    profile.network = Some(
                        serde_json::from_value(network.clone())
                            .map_err(|_| "网络出口配置格式无效")?,
                    );
                }
                if let Some(username) = profile_patch.get("username").and_then(|v| v.as_str()) {
                    profile.username = username.to_string();
                }
                if let Some(auth_url) = profile_patch.get("auth_url").and_then(|v| v.as_str()) {
                    let trimmed = auth_url.trim();
                    if !trimmed.is_empty() {
                        validate_auth_url(trimmed).map_err(|e| e.to_string())?;
                    }
                    profile.auth_url = trimmed.to_string();
                }
                if let Some(trigger_url) = profile_patch.get("trigger_url").and_then(|v| v.as_str())
                {
                    let trimmed = trigger_url.trim();
                    if !trimmed.is_empty() {
                        validate_trigger_url(trimmed).map_err(|e| e.to_string())?;
                    }
                    profile.trigger_url = trimmed.to_string();
                }
                if let Some(isp) = profile_patch.get("isp").and_then(|v| v.as_str()) {
                    profile.isp = isp.to_string();
                }
                if let Some(active_task) = profile_patch.get("active_task").and_then(|v| v.as_str())
                {
                    profile.active_task = active_task.to_string();
                }
                // 登录渠道是枚举（"browser"/"http"/"script"），不走字符串校验分支；
                // 非法值显式 400，避免静默保留旧渠道让用户以为已切换
                if let Some(channel) = profile_patch.get("login_channel") {
                    profile.login_channel =
                        serde_json::from_value::<crate::config::LoginChannel>(channel.clone())
                            .map_err(|_| {
                                ApiError::BadRequest(
                                    "login_channel 仅支持 browser、http 或 script".into(),
                                )
                            })
                            .map_err(|e| e.to_string())?;
                }
                // 任务绑定：字符串（空串 = 未绑定）
                if let Some(v) = profile_patch
                    .get("active_http_task")
                    .and_then(|v| v.as_str())
                {
                    profile.active_http_task = v.to_string();
                }
                if let Some(v) = profile_patch
                    .get("active_script_task")
                    .and_then(|v| v.as_str())
                {
                    profile.active_script_task = v.to_string();
                }
                // 直连 / 脚本渠道必须有可用的任务绑定。**与 `POST/PUT /api/profiles/{id}`
                // 完全同一口径**（共用 `validate_login_task_binding`）：三条保存路径若只有一条
                // 放宽，用户从设置页存一次就能把方案改成登录时必失败的状态，且毫无提示。
                // 校验的是**合并后**的渠道与绑定，故只改渠道不改绑定（或反之）同样整体判定。
                validate_login_task_binding(
                    &tasks,
                    profile.login_channel,
                    &profile.active_http_task,
                    &profile.active_script_task,
                )
                .await
                .map_err(|e| e.to_string())?;
                if let Some(password) = profile_patch.get("password") {
                    // 全局设置页使用三态契约：null 保留、空串清除、非空字符串加密更新。
                    // Profile 编辑接口仍沿用其既有的“空串保留”语义，避免改变旧客户端行为。
                    match password {
                        Value::Null => {}
                        Value::String(pwd_str) if pwd_str.is_empty() => profile.password.clear(),
                        Value::String(pwd_str) => {
                            profile.password =
                                profiles.save_password(Some(pwd_str), &profile.password);
                        }
                        _ => return Err("password 必须是字符串或 null".to_string()),
                    }
                }
                super::profiles::validate_profile_network(&profile)?;
                Ok(profile)
            })
        }),
    )))
}

/// 构造全局设置的合并闭包（持锁读-改-写，闭包失败不落盘）
///
/// 两个桶均为空时返回 `None`（无可提交内容）。
fn build_settings_merge(
    global_patch: serde_json::Map<String, Value>,
    other_patch: serde_json::Map<String, Value>,
) -> Option<SettingsMergeFn> {
    if global_patch.is_empty() && other_patch.is_empty() {
        return None;
    }
    // 空否以前置 Map 判定为准：Value::Object 包裹后 as_object() 恒为 Some，
    // 此处不再 unwrap（此前写法正确但制造 panic 观感）
    let global_empty = global_patch.is_empty();
    let other_empty = other_patch.is_empty();
    // monitor 的 url_expected_responses 必须整体替换而非深合并：url_check_urls 是
    // 前端回传的**完整** URL 检测清单（url_targets 数组同批整体替换），而 json_merge
    // 对空对象是 no-op、对新对象是按键合并——清空列表清不掉旧映射、换列表会残留
    // 被移除 URL 的旧期望条目。合并前取出补丁值，合并后整体覆盖。
    let monitor_expected = global_patch
        .get("monitor")
        .and_then(|m| m.get("url_expected_responses"))
        .cloned();
    let global_patch = Value::Object(global_patch);
    let other_patch = Value::Object(other_patch);
    Some(Box::new(move |settings| {
        let mut current_value =
            serde_json::to_value(&settings).map_err(|e| format!("设置序列化失败: {e}"))?;
        // 合并 global 字段
        if !global_empty && let Some(global) = current_value.get_mut("global") {
            json_merge(global, &global_patch);
        }
        // 合并其他字段（如 active_profile_id 等）
        if !other_empty {
            json_merge(&mut current_value, &other_patch);
        }
        // monitor 期望响应映射整体替换（深合并无法表达「清空/移除条目」）
        if let Some(expected) = monitor_expected
            && let Some(monitor) = current_value
                .get_mut("global")
                .and_then(|g| g.get_mut("monitor"))
                .and_then(Value::as_object_mut)
        {
            monitor.insert("url_expected_responses".to_string(), expected);
        }
        serde_json::from_value::<crate::config::SettingsData>(current_value)
            .map_err(|e| format!("设置合并后校验失败: {e}"))
            // 推送通知节保存侧全量校验（URL scheme / 超时与重试上下界 / 事件类型
            // 白名单 / 通道结构与凭据）；任何全局保存路径都会跑到这里，保证
            // notification 不会以非法状态落盘
            .and_then(|settings| {
                settings
                    .global
                    .notification
                    .validate()
                    .map(|()| settings)
                    .map_err(|msg| format!("推送通知配置无效: {msg}"))
            })
    }))
}

/// 将前端扁平 patch 应用并保存（PATCH /api/config）
///
/// 凭证字段（username/password/auth_url/isp/active_task/active_http_task）直接写入
/// 活跃 Profile；全局设置经 [`ConfigApi::modify_settings_tx`] 的提交事务落盘——
/// 「读取→合并→校验→持久化」在同一 `settings_lock` 临界区内完成。历史实现锁外读取
/// 合并整份设置再 `save_settings`（仅锁最终写入），两个并发修改不同字段的请求
/// 会相互覆盖（丢更新）。
///
/// `tasks` 仅用于校验直连任务绑定（见 [`validate_login_task_binding`]）——这里不复用
/// `ProfileApi`，因为绑定校验是**任务域的事实**，与方案业务无关。
/// 编排顺序：`split_patch_keys` 分拣 → WEB-9 / 端口预检 → `build_profile_tx` /
/// `build_settings_merge` → 双域或单域事务提交。
async fn apply_flat_settings_patch(
    config: &Arc<dyn ConfigApi>,
    profiles: &Arc<dyn ProfileApi>,
    tasks: &Arc<dyn TaskApi>,
    patch: &Value,
) -> Result<(), ApiError> {
    let Some(obj) = patch.as_object() else {
        // 非对象 patch：无字段可合并（与旧实现一致，不落盘直接成功）
        return Ok(());
    };

    let FlatPatchParts {
        global_patch,
        profile_patch,
        other_patch,
        changed_fields,
    } = split_patch_keys(obj)?;

    // WEB-9：active_profile_id 切换必须在合并前校验目标 Profile 存在——悬空 id
    // 落盘后读取配置会加载失败并静默回退 ProfileData::default()（空凭据），
    // 用户看到与设置不符且无错误提示
    let explicit_profile_id = obj.get("active_profile_id").and_then(|v| v.as_str());
    if let Some(id) = explicit_profile_id {
        if id.is_empty() {
            return Err(ApiError::BadRequest("active_profile_id 不能为空".into()));
        }
        config.load_profile(id).map_err(|e| {
            ApiError::BadRequest(format!(
                "active_profile_id 指向的 Profile 不存在: {id}（{e}）"
            ))
        })?;
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
    let profile_tx =
        build_profile_tx(config, profiles, tasks, profile_patch, explicit_profile_id).await?;

    let settings_merge = build_settings_merge(global_patch, other_patch);

    // 提交：Profile 与全局设置同时变更走双域事务；仅其一走各自单域事务
    let result = match profile_tx {
        Some((profile_id, profile_merge)) => {
            config
                .modify_profile_and_settings_tx(&profile_id, profile_merge, settings_merge)
                .await
        }
        None => match settings_merge {
            Some(merge) => config.modify_settings_tx(merge).await,
            // 两个 patch 均为空：无可提交内容（原实现同口径，直接跳过落盘）
            None => Ok(Ok(())),
        },
    };
    match result {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(ApiError::BadRequest(msg)),
        Err(e) => return Err(e.into()),
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
/// 字段顺序与历史响应完全一致（新增字段追加在末尾，避免打乱既有顺序）；
/// monitor 字段做后端→前端字段名映射
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
        "notification": settings.global.notification,
        "network": crate::network::settings::NetworkSettings::effective(profile, &settings.global),
        "username": profile.username,
        "auth_url": profile.auth_url,
        "trigger_url": profile.trigger_url,
        "isp": profile.isp,
        "active_task": profile.active_task,
        "has_password": has_password,
        // 活跃方案的登录渠道与「渠道 → 任务」绑定：设置页「账号」Tab 与引导向导据此编辑、
        // 分流，无需为一次编辑再拉整个方案列表。这几个字段属 Profile 域
        // （后端写入活跃 Profile，不是全局 settings）；直连的请求参数不在这里，
        // 它们属于 `tasks/http/<id>.json`，前端按 `active_http_task` 去任务页取；
        // 脚本任务的正文同理属于 `tasks/scripts/<id>.json`。
        "login_channel": profile.login_channel,
        "active_http_task": profile.active_http_task,
        "active_script_task": profile.active_script_task,
    })
}

/// 计算 has_password：必须反映「密码可用」（能解密），而非仅「字段非空」
///
/// 否则密钥变更/格式不兼容时，前端误认为已保存 → 不重新输入 →
/// 登录报缺少 password；刚保存显示成功、刷新又提示需重输，体验割裂。
///
/// `pub(crate)`：`GET /api/profiles/{id}` 需要同一口径的判定（方案页要显示
/// 「密码已保存」并据此决定是否提供清除入口），两处必须同源，否则「账号页说
/// 已保存、方案页说未设置」这类漂移会再次出现。
pub(crate) fn effective_has_password(
    config: &dyn ConfigApi,
    profile: &crate::config::ProfileData,
) -> bool {
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
    // 落盘成功后必须 reload 发布 RuntimeConfig，否则内存快照滞后、纯净模式开关不生效
    //（对照同文件 PUT/PATCH 的 reload_and_flat_response 口径）。reload 失败仅告警不阻断：
    // 文件已落盘，下次 reload / 重启后自然生效，此时回滚响应反而会让前端与磁盘状态不一致。
    if let Err(e) = config.reload().await {
        tracing::warn!(error = %e, "纯净模式切换后 reload RuntimeConfig 失败（文件已落盘，待下次 reload 生效）");
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
        "auth_url_targets": [],
        "url_check_urls": url_check_urls,
        "strict_login_mode": m.strict_login_mode,
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
    strict_login_mode: Option<bool>,
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
        if let Some(value) = self.strict_login_mode {
            backend.insert("strict_login_mode".into(), Value::from(value));
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

        // 旧客户端的手动检查开关仅兼容接收，不能重新启用已删除的检查。
        let _ = (self.enable_local_check, self.check_auth_url);
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
            strict_login_mode: true,
            disable_proxy: true,
            profile_check_interval: 300,
            tcp_timeout: 5,
            http_timeout: 5,
            url_timeout: 5,
            auth_url_timeout: 5,
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
    fn monitor_legacy_manual_checks_are_ignored() {
        let front = monitor_backend_to_frontend(&sample_monitor());
        assert!(front.get("enable_local_check").is_none());
        assert!(front.get("check_auth_url").is_none());
        let back = monitor_frontend_to_backend(&serde_json::json!({
            "enable_local_check": true,
            "check_auth_url": true,
            "strict_login_mode": false,
        }))
        .unwrap();
        assert!(back.get("local_check_enabled").is_none());
        assert!(back.get("check_auth_url").is_none());
        assert_eq!(back["strict_login_mode"], false);
    }

    #[test]
    fn monitor_roundtrip_preserves_strict_login_mode() {
        // 严格登录模式须真实往返：GET 给出后端值，PATCH 能写回
        let mut original = sample_monitor();
        assert!(original.strict_login_mode, "默认应开启（严格口径）");
        assert_eq!(
            monitor_backend_to_frontend(&original)["strict_login_mode"],
            serde_json::json!(true)
        );
        // 关闭严格模式（= 启用宽松触发）后必须能被表达并写回
        original.strict_login_mode = false;
        let front = monitor_backend_to_frontend(&original);
        assert_eq!(front["strict_login_mode"], serde_json::json!(false));
        let back = monitor_frontend_to_backend(&front).unwrap();
        assert_eq!(back["strict_login_mode"], serde_json::json!(false));
    }

    /// 关闭严格模式是显式值而非缺省：缺省（未提供）不得被当成 `false` 写回，
    /// 否则前端任意一次局部保存都会把用户配置悄悄切成宽松口径
    #[test]
    fn monitor_patch_omitting_strict_mode_does_not_flip_it() {
        let back = monitor_frontend_to_backend(&serde_json::json!({
            "enable_tcp_check": true
        }))
        .unwrap();
        assert_eq!(back, serde_json::json!({ "tcp_enabled": true }));
        assert!(
            back.get("strict_login_mode").is_none(),
            "未提供该字段时不得出现在 patch 中"
        );
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

    /// url_check_urls 是前端回传的**完整**清单：换列表必须移除被删 URL 的旧期望
    /// 条目、清空列表必须清空整张映射（深合并做不到「清空/移除」，属性测试 P2 挖出）。
    #[tokio::test]
    async fn test_patch_monitor_url_check_urls_replaces_expected_map() {
        let (app, inner) = mock_app();
        // 预置两个旧条目
        {
            let mut g = inner.lock().unwrap();
            g.settings.global.monitor.url_targets =
                vec!["http://old.com".into(), "http://keep.com".into()];
            g.settings
                .global
                .monitor
                .url_expected_responses
                .insert("http://old.com".into(), "OLD".into());
            g.settings
                .global
                .monitor
                .url_expected_responses
                .insert("http://keep.com".into(), "KEEP".into());
        }
        // 换成只含 keep.com 的清单：old 条目必须消失、keep 值更新
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "monitor": { "url_check_urls": ["http://keep.com|KEEP2"] }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        {
            let g = inner.lock().unwrap();
            let m = &g.settings.global.monitor;
            assert_eq!(m.url_targets, vec!["http://keep.com".to_string()]);
            assert_eq!(
                m.url_expected_responses.len(),
                1,
                "被移除 URL 的旧条目必须消失"
            );
            assert_eq!(
                m.url_expected_responses
                    .get("http://keep.com")
                    .map(String::as_str),
                Some("KEEP2")
            );
        }
        // 清空列表：整张期望响应映射必须为空
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "monitor": { "url_check_urls": [] } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let m = &g.settings.global.monitor;
        assert!(m.url_targets.is_empty());
        assert!(
            m.url_expected_responses.is_empty(),
            "清空 url_check_urls 必须清空期望响应映射，实际: {:?}",
            m.url_expected_responses
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
    use crate::tasks::TaskError;

    #[derive(Default)]
    struct MockInner {
        settings: crate::config::SettingsData,
        profile: ProfileData,
        save_calls: usize,
        reload_calls: usize,
        /// 打开后 load_profile 返回错误，用于验证凭证写入失败路径（G16）
        profile_load_fails: bool,
    }

    /// 内存 TaskApi：只实现 `validate_login_task_binding` 用到的 `load_task`。
    /// 固定放一个直连任务、一个浏览器任务与一个脚本任务，覆盖"存在且类型对 /
    /// 类型不对 / 不存在"三态（两种进程内渠道各一条）。
    struct MockTaskApi;

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<crate::tasks::TaskSummary> {
            Vec::new()
        }

        async fn load_task(&self, task_id: &str) -> Result<crate::tasks::TaskKind, TaskError> {
            match task_id {
                "portal-http" => Ok(crate::tasks::TaskKind::Http(Box::default())),
                "portal-browser" => Ok(crate::tasks::TaskKind::Browser(
                    crate::tasks::TaskConfig::default(),
                )),
                "portal-login" => Ok(crate::tasks::TaskKind::Script(
                    crate::tasks::ScriptTaskConfig::default(),
                )),
                other => Err(TaskError::TaskNotFound(other.to_string())),
            }
        }

        async fn embed_task_config(&self, _task_id: &str, _params: &mut Value) -> bool {
            false
        }

        async fn save_task(
            &self,
            _task_id: &str,
            _task: &crate::tasks::TaskKind,
        ) -> Result<(), TaskError> {
            Ok(())
        }

        async fn delete_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_task_detail(
            &self,
            task_id: &str,
        ) -> Result<crate::tasks::TaskDetail, TaskError> {
            Err(TaskError::TaskNotFound(task_id.to_string()))
        }

        async fn load_order(&self) -> crate::tasks::OrderData {
            crate::tasks::OrderData::default()
        }

        async fn save_order(&self, _order: &crate::tasks::OrderData) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_script_path(&self, _task_id: &str) -> Option<std::path::PathBuf> {
            None
        }

        fn has_task(&self, task_id: &str) -> bool {
            matches!(task_id, "portal-http" | "portal-browser")
        }
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
            network: Default::default(),
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
                login_channel: crate::config::LoginChannel::default(),
                active_http_task: String::new(),
                active_script_task: String::new(),
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
        async fn update_profile(
            &self,
            _id: &str,
            _data: ProfileData,
            _clear_password: bool,
        ) -> Result<(), ConfigError> {
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

    /// 双域 state：ConfigApi + ProfileApi + TaskApi 各自经 FromRef 委派提取
    ///
    /// patch_settings 声明多 State 依赖（凭证写入活跃 Profile，直连绑定校验查任务）
    #[derive(Clone)]
    struct PatchTestState {
        config: Arc<dyn ConfigApi>,
        profiles: Arc<dyn ProfileApi>,
        tasks: Arc<dyn TaskApi>,
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

    impl axum::extract::FromRef<PatchTestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &PatchTestState) -> Self {
            state.tasks.clone()
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
            tasks: Arc::new(MockTaskApi),
        };
        let app = axum::Router::new()
            .route("/api/config", get(get_settings).patch(patch_settings))
            .route("/api/config/log-levels", get(get_log_levels))
            .route("/api/config/log-level", axum::routing::put(set_log_level))
            .route("/api/pure-mode", get(get_pure_mode).post(set_pure_mode))
            .with_state(state);
        (app, inner)
    }

    // 测试脚手架统一走共享 test_support（WE2-5：原逐文件复制的 body_json 已收敛）
    use crate::web::routes::test_support::body_json;

    /// WEB-9：PATCH 携带指向不存在 Profile 的 active_profile_id → 400 且不落盘
    /// （悬空 id 落盘后读取配置会静默回退空凭据，必须在合并前拦下）
    #[tokio::test]
    async fn test_patch_dangling_active_profile_id_rejected() {
        let (app, inner) = mock_app();
        inner.lock().unwrap().profile_load_fails = true;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"active_profile_id": "ghost"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let g = inner.lock().unwrap();
        assert_eq!(g.save_calls, 0, "拒绝路径不得落盘");
        assert_eq!(
            g.settings.active_profile_id, "default",
            "悬空 id 不得写入设置"
        );
    }

    /// WEB-9 对照：active_profile_id 指向存在的 Profile → 正常合并落盘
    #[tokio::test]
    async fn test_patch_active_profile_id_existing_ok() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"active_profile_id": "default"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
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
        // 活跃方案登录渠道：引导向导分流与「按渠道抑制环境提示」的数据源
        assert_eq!(d["login_channel"], "browser");
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

    /// GET /api/config 回传活跃方案的直连渠道与直连任务绑定（避免前端为判定登录
    /// 方式/测试目标再拉一次方案列表）
    #[tokio::test]
    async fn test_get_settings_reports_http_login_channel() {
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.profile.login_channel = crate::config::LoginChannel::Http;
            g.profile.active_http_task = "portal-http".into();
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
        assert_eq!(v["data"]["login_channel"], "http");
        assert_eq!(v["data"]["active_http_task"], "portal-http");
    }

    /// PATCH 回传 login_channel + active_http_task 必须落回 Profile（不是全局设置）
    #[tokio::test]
    async fn test_patch_login_channel_updates_profile_not_global_settings() {
        let (app, inner) = mock_app();
        let settings_before = {
            let g = inner.lock().unwrap();
            serde_json::to_value(&g.settings).unwrap()
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "login_channel": "http",
                            "active_http_task": "portal-http"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["login_channel"], "http", "响应需回显新渠道");
        assert_eq!(v["data"]["active_http_task"], "portal-http");
        let g = inner.lock().unwrap();
        assert_eq!(g.profile.login_channel, crate::config::LoginChannel::Http);
        assert_eq!(g.profile.active_http_task, "portal-http");
        // 渠道属 Profile 域：全局设置不得被改动（WEB-2 未知键落到 global/顶层的同源风险）
        assert_eq!(
            serde_json::to_value(&g.settings).unwrap(),
            settings_before,
            "全局设置不应被 login_channel 影响"
        );
    }

    /// 脚本渠道在 PATCH 路径上与直连同一口径：绑定必须有效，且落回 Profile 而非全局设置
    #[tokio::test]
    async fn test_patch_script_channel_validates_binding_and_updates_profile() {
        let (app, inner) = mock_app();
        let settings_before = {
            let g = inner.lock().unwrap();
            serde_json::to_value(&g.settings).unwrap()
        };

        // 切到脚本渠道但不给绑定 → 400（合并后整体无效，与新建/更新路径同判据）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "login_channel": "script" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 带上有效绑定 → 200 且落回 Profile
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "login_channel": "script",
                            "active_script_task": "portal-login"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["login_channel"], "script", "响应需回显新渠道");
        assert_eq!(v["data"]["active_script_task"], "portal-login");
        let g = inner.lock().unwrap();
        assert_eq!(g.profile.login_channel, crate::config::LoginChannel::Script);
        assert_eq!(g.profile.active_script_task, "portal-login");
        assert_eq!(
            serde_json::to_value(&g.settings).unwrap(),
            settings_before,
            "全局设置不应被脚本渠道字段影响"
        );
    }

    /// 非法 login_channel 显式 400，不静默保留旧渠道
    #[tokio::test]
    async fn test_patch_login_channel_rejects_invalid_value() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "login_channel": "carrier_pigeon" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// PATCH 路径的直连绑定校验必须与方案接口同口径：切到 http 却不给绑定 → 400
    /// （否则用户从设置页存一次就能把方案改成登录时必失败的状态）
    #[tokio::test]
    async fn test_patch_switch_to_http_without_binding_is_rejected() {
        let (app, inner) = mock_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "login_channel": "http" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("请为直连渠道选择一个直连任务"),
            "{json}"
        );
        assert_eq!(inner.lock().unwrap().save_calls, 0, "拒绝路径不得落盘");
    }

    /// PATCH 绑定指向不存在的任务 / 非直连任务 → 400，且不得半落盘
    #[tokio::test]
    async fn test_patch_rejects_unusable_http_binding() {
        let (app, inner) = mock_app();
        for bad in ["ghost", "portal-browser"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri("/api/config")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "login_channel": "http",
                                "active_http_task": bad
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "绑定 {bad} 应被拒");
        }
        let g = inner.lock().unwrap();
        assert_eq!(g.save_calls, 0, "拒绝路径不得落盘");
        assert_eq!(
            g.profile.login_channel,
            crate::config::LoginChannel::Browser,
            "被拒的渠道变更不得部分生效"
        );
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
        // 落盘后必须 reload 发布 RuntimeConfig，否则纯净模式开关不生效（P1 修复回归）
        assert_eq!(inner.lock().unwrap().reload_calls, 1);
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
    /// 网络字段必须落在活跃方案，非法 DNS 不得部分覆盖已有设置。
    #[tokio::test]
    async fn patch_network_is_profile_scoped_and_invalid_dns_is_atomic() {
        let (app, inner) = mock_app();
        let network = serde_json::json!({"mode":"interface", "interface_id":"campus-adapter", "proxy_url":"", "dns_servers":["10.0.0.53"]});
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"network":network}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            serde_json::to_value(inner.lock().unwrap().profile.network.as_ref().unwrap()).unwrap(),
            network
        );
        let response = app.oneshot(Request::builder().method("PATCH").uri("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"network":{"mode":"interface","interface_id":"campus-adapter","dns_servers":["invalid"]}}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_value(inner.lock().unwrap().profile.network.as_ref().unwrap()).unwrap(),
            network
        );
    }

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
        // carrier_custom 是 v6 前的历史字段：必须被显式忽略，不写入 Profile 也不写脏 settings
        // 全局字段保存 + reload
        assert!(g.settings.global.pause.enabled);
        assert_eq!(g.save_calls, 1);
        assert_eq!(g.reload_calls, 1);
        // 响应回显凭证与 has_password（不再回显已废弃的 carrier_custom）
        assert_eq!(d["username"], "alice");
        assert_eq!(d["isp"], "cmcc");
        assert!(d.get("carrier_custom").is_none());
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
        // pause 默认启用，落盘失败也不应改变它（此前 payload 写 true 是为了反证；
        // 默认值调整后 mock 的初始状态本身就是 true，断言改盯落盘计数即可）
        assert!(g.settings.global.pause.enabled);
        assert_eq!(g.save_calls, 0);
    }

    // ============ 属性测试：PATCH 合并语义（proptest，T4-2b） ============
    //
    // 被测对象为 apply_flat_settings_patch 编排链（split_patch_keys 分拣 →
    // build_profile_tx / build_settings_merge → 双域或单域事务），以模块上方
    // 的内存 mock 承载。proptest 用例保持同步外壳、每个用例内建
    // current_thread 运行时驱动 async：mock 全内存、开销微秒级；不能在
    // #[tokio::test] 里嵌套 block_on（运行时内禁止再起运行时），故不采用外层
    // 异步测试。
    //
    // 生成器覆盖面：
    // - 合法字段族：全局各段（pause/logging/retry/app_settings/browser/worker/
    //   updater/monitor DTO）+ 凭证（username/isp/auth_url/trigger_url/
    //   active_task/password 三态/network）+ active_profile_id + 渠道与绑定的
    //   复合族（browser 单独 / http+portal-http / script+portal-login）
    // - 必拒条目：monitor DTO 违例、各段类型错位、端口越界、非法更新通道、
    //   非字符串凭证、非法 URL、非法网络出口、空 active_profile_id
    // - 未知键与 carrier_custom：前者交由 schema 丢弃（无字段污染），后者显式忽略
    // - 顶层非对象（数组/字符串/数字/null）：无字段可合并，直接成功不落盘
    //
    // 生成器排除的跨族冲突：网卡出口（interface）× 脚本渠道的互斥单独由
    // reject 条目与既有单测覆盖，合法族生成器不混出该组合，保证「合法 patch
    // 必成功」的前提可被精确断言。

    use proptest::prelude::*;
    use serde_json::json;

    /// 属性测试统一口径：512 cases（CI 亲和；随机失败必可 shrink 复现）
    fn prop_config() -> proptest::test_runner::Config {
        proptest::test_runner::Config::with_cases(512)
    }

    /// proptest 用例内驱动 async 合并路径（每个用例独立 current_thread 运行时）
    fn prop_block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试运行时构建失败")
            .block_on(fut)
    }

    /// 合法字段族（可含多个顶层键：渠道族同时携带绑定）
    type PropFamily = Vec<(String, Value)>;

    /// 预置非默认初始状态：让「指定字段生效 / 未指定字段保持」都可观测
    ///（逐字段赋值而非结构体字面量：预设的是深层嵌套字段，字面量写法更冗长）
    #[allow(clippy::field_reassign_with_default)]
    fn prop_preset() -> MockInner {
        let mut settings = crate::config::SettingsData::default();
        settings.global.pause.enabled = false;
        settings.global.pause.start_hour = 1;
        settings.global.monitor.check_interval = 123;
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.post_login_delay = 9;
        settings.global.monitor.tcp_targets = vec!["8.8.8.8:53".into()];
        settings.global.logging.level = "WARN".into();
        settings.global.logging.retention_days = 3;
        settings.global.retry_settings.max_retries = 7;
        settings.global.app.port = 50722;
        settings.global.app.show_tray = false;
        settings.global.updater.channel = crate::config::UpdateChannel::All;
        settings.global.updater.auto_check_enabled = false;
        settings.global.browser.headless = false;
        settings.global.worker.keep_alive = true;
        settings.global.worker.idle_timeout_seconds = 777;
        let mut profile = ProfileData::default();
        profile.username = "orig-user".into();
        profile.isp = "orig-isp".into();
        profile.active_task = "orig-task".into();
        MockInner {
            settings,
            profile,
            save_calls: 0,
            reload_calls: 0,
            profile_load_fails: false,
        }
    }

    /// 属性测试的内存三域 mock 句柄（ConfigApi / ProfileApi / TaskApi + 状态内省）
    type PropHandles = (
        Arc<dyn ConfigApi>,
        Arc<dyn ProfileApi>,
        Arc<dyn TaskApi>,
        Arc<std::sync::Mutex<MockInner>>,
    );

    fn prop_state() -> PropHandles {
        let inner = Arc::new(std::sync::Mutex::new(prop_preset()));
        (
            Arc::new(MockConfigApi(inner.clone())),
            Arc::new(MockProfileApi),
            Arc::new(MockTaskApi),
            inner,
        )
    }

    fn prop_settings_json(inner: &std::sync::Mutex<MockInner>) -> Value {
        serde_json::to_value(&inner.lock().unwrap().settings).unwrap()
    }

    fn prop_profile_json(inner: &std::sync::Mutex<MockInner>) -> Value {
        serde_json::to_value(&inner.lock().unwrap().profile).unwrap()
    }

    /// monitor 前端键（与 MonitorPatch DTO 字段一一对应；往返保真字段不在此列）
    const PROP_MONITOR_FRONT_KEYS: [&str; 11] = [
        "check_interval_seconds",
        "enable_tcp_check",
        "enable_http_check",
        "enable_url_check",
        "strict_login_mode",
        "disable_proxy",
        "network_check_timeout",
        "post_login_delay",
        "ping_targets",
        "test_urls",
        "url_check_urls",
    ];

    /// monitor DTO 的随机合法局部载荷（1..=4 个子字段，值合法）
    fn prop_arb_monitor_payload() -> impl proptest::strategy::Strategy<Value = Value> {
        proptest::collection::vec(
            prop_oneof![
                (1u32..=600).prop_map(|v| (0u8, json!(v))),
                any::<bool>().prop_map(|v| (1u8, json!(v))),
                any::<bool>().prop_map(|v| (2u8, json!(v))),
                any::<bool>().prop_map(|v| (3u8, json!(v))),
                any::<bool>().prop_map(|v| (4u8, json!(v))),
                any::<bool>().prop_map(|v| (5u8, json!(v))),
                (1u64..=30).prop_map(|v| (6u8, json!(v))),
                (0u64..=60).prop_map(|v| (7u8, json!(v))),
                proptest::collection::vec(
                    prop_oneof![
                        Just("10.0.0.53:53".to_owned()),
                        Just("192.168.1.1:53".to_owned()),
                    ],
                    1..=2,
                )
                .prop_map(|v| (8u8, json!(v))),
                proptest::collection::vec(Just("http://portal.edu.cn".to_owned()), 1..=2)
                    .prop_map(|v| (9u8, json!(v))),
                proptest::collection::vec(
                    prop_oneof![
                        Just("http://a.com|OK".to_owned()),
                        Just("http://b.com".to_owned()),
                        Just("http://c.com|成功|备用".to_owned()),
                    ],
                    0..=2,
                )
                .prop_map(|v| (10u8, json!(v))),
            ],
            1..=4,
        )
        .prop_map(|entries| {
            // 子字段去重：Map 后写覆盖先写会让断言与生效条目失去一一对应
            let mut obj = serde_json::Map::new();
            let mut seen = std::collections::HashSet::new();
            for (idx, v) in entries {
                if seen.insert(idx) {
                    obj.insert(PROP_MONITOR_FRONT_KEYS[idx as usize].to_owned(), v);
                }
            }
            Value::Object(obj)
        })
    }

    /// 网络出口的随机合法载荷。
    /// interface 模式刻意不进合法族池：它与脚本渠道互斥（validate_profile_network），
    /// 属跨族冲突，由 reject 条目与既有单测覆盖。
    fn prop_arb_network_payload() -> impl proptest::strategy::Strategy<Value = Value> {
        prop_oneof![
            Just(json!({"mode": "direct", "interface_id": "", "proxy_url": "", "dns_servers": []})),
            Just(json!({"mode": "system", "interface_id": "", "proxy_url": "", "dns_servers": []})),
            (0u8..=2).prop_map(|i| {
                let url = [
                    "http://127.0.0.1:7890",
                    "socks5://10.0.0.1:1080",
                    "socks5h://127.0.0.1:1080",
                ][i as usize];
                json!({
                    "mode": "proxy",
                    "interface_id": "",
                    "proxy_url": url,
                    "dns_servers": [],
                })
            }),
        ]
    }

    /// 单个必拒条目（每一条在现行口径下都必须整体拒绝且不落盘）
    fn prop_arb_reject_entry() -> impl proptest::strategy::Strategy<Value = (String, Value)> {
        prop_oneof![
            // monitor DTO：未知字段 / 类型错位 / 非对象
            (0u8..=4).prop_map(|i| (
                "monitor".to_owned(),
                match i {
                    0 => json!({"bogus_field": 1}),
                    1 => json!({"check_interval_seconds": "soon"}),
                    2 => json!(5),
                    3 => Value::Null,
                    _ => json!({"ping_targets": "not-array"}),
                },
            )),
            // pause：布尔错位 / 整段标量替换 / 子字段类型错位
            (0u8..=2).prop_map(|i| (
                "pause".to_owned(),
                match i {
                    0 => json!({"enabled": "yes"}),
                    1 => json!(5),
                    _ => json!({"start_hour": "7"}),
                },
            )),
            // 端口越界 / 非数值
            (0u8..=3).prop_map(|i| (
                "app_settings".to_owned(),
                match i {
                    0 => json!({"port": 0}),
                    1 => json!({"port": 65536}),
                    2 => json!({"port": -1}),
                    _ => json!({"port": "8080"}),
                },
            )),
            // updater 非法通道 / 负间隔 / 非字符串通道
            (0u8..=2).prop_map(|i| (
                "updater".to_owned(),
                match i {
                    0 => json!({"channel": "nightly"}),
                    1 => json!({"check_interval_hours": -5}),
                    _ => json!({"channel": 3}),
                },
            )),
            // 各段类型错位
            Just(("logging".to_owned(), json!({"retention_days": "x"}))),
            Just(("worker".to_owned(), json!({"idle_timeout_seconds": -1}))),
            Just(("browser".to_owned(), json!({"headless": "yes"}))),
            Just(("retry".to_owned(), json!({"max_retries": -2}))),
            // 渠道与凭证
            prop_oneof![Just("weird"), Just("")]
                .prop_map(|v| { ("login_channel".to_owned(), json!(v)) }),
            (0u8..=3).prop_map(|i| (
                "username".to_owned(),
                match i {
                    0 => json!(123),
                    1 => Value::Null,
                    2 => json!([1]),
                    _ => json!({"u": 1}),
                },
            )),
            (0u8..=2).prop_map(|i| (
                "password".to_owned(),
                match i {
                    0 => json!(42),
                    1 => json!(true),
                    _ => json!(["p"]),
                },
            )),
            prop_oneof![Just("ftp://x"), Just("not a url")]
                .prop_map(|v| { ("auth_url".to_owned(), json!(v)) }),
            Just(("trigger_url".to_owned(), json!("mailto:a@b"))),
            (0u8..=3).prop_map(|i| (
                "network".to_owned(),
                match i {
                    0 => json!({"mode": "interface", "dns_servers": ["bad-dns"]}),
                    1 => json!({"mode": "interface"}),
                    2 => json!({"mode": "proxy"}),
                    _ => json!(5),
                },
            )),
            // 空 active_profile_id（WEB-9）
            Just(("active_profile_id".to_owned(), json!(""))),
        ]
    }

    /// 随机合法字段族
    fn prop_arb_valid_family() -> impl proptest::strategy::Strategy<Value = PropFamily> {
        prop_oneof![
            2 => (any::<bool>(), 0u8..=23, 0u8..=59, 0u8..=23, 0u8..=59).prop_map(
                |(e, sh, sm, eh, em)| {
                    vec![(
                        "pause".to_owned(),
                        json!({
                            "enabled": e,
                            "start_hour": sh,
                            "start_minute": sm,
                            "end_hour": eh,
                            "end_minute": em,
                        }),
                    )]
                },
            ),
            2 => (
                prop_oneof![Just("INFO"), Just("DEBUG"), Just("WARN"), Just("ERROR")],
                1u32..=90,
                any::<bool>(),
            )
                .prop_map(|(lvl, days, fe)| {
                    vec![(
                        "logging".to_owned(),
                        json!({"level": lvl, "retention_days": days, "file_enabled": fe}),
                    )]
                }),
            2 => (0u32..=10, 0u32..=60).prop_map(|(mr, ri)| {
                vec![("retry".to_owned(), json!({"max_retries": mr, "retry_interval": ri}))]
            }),
            2 => (1u16..=65535, any::<bool>(), any::<bool>(), 0u32..=48).prop_map(
                |(port, asb, st, arh)| {
                    vec![(
                        "app_settings".to_owned(),
                        json!({
                            "port": port,
                            "auto_start_browser": asb,
                            "show_tray": st,
                            "auto_restart_hours": arh,
                        }),
                    )]
                },
            ),
            1 => (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
                |(h, pm, lrm, pc)| {
                    vec![(
                        "browser".to_owned(),
                        json!({
                            "headless": h,
                            "pure_mode": pm,
                            "low_resource_mode": lrm,
                            "persistent_context": pc,
                        }),
                    )]
                },
            ),
            1 => (60u32..=3600, any::<bool>()).prop_map(|(it, ka)| {
                vec![(
                    "worker".to_owned(),
                    json!({"idle_timeout_seconds": it, "keep_alive": ka}),
                )]
            }),
            2 => (
                prop_oneof![Just("stable"), Just("prerelease"), Just("all")],
                any::<bool>(),
                0u32..=72,
                any::<bool>(),
            )
                .prop_map(|(ch, ace, cih, up)| {
                    vec![(
                        "updater".to_owned(),
                        json!({
                            "channel": ch,
                            "auto_check_enabled": ace,
                            "check_interval_hours": cih,
                            "use_proxy": up,
                        }),
                    )]
                }),
            3 => prop_arb_monitor_payload().prop_map(|v| vec![("monitor".to_owned(), v)]),
            2 => prop_oneof![
                Just("alice".to_owned()),
                Just("20230001".to_owned()),
                Just(String::new()),
            ].prop_map(
                |v| vec![("username".to_owned(), json!(v))],
            ),
            1 => prop_oneof![Just("cmcc"), Just("电信")].prop_map(|v| {
                vec![("isp".to_owned(), json!(v))]
            }),
            1 => prop_oneof![
                Just("http://10.0.0.1/".to_owned()),
                Just("https://portal.edu.cn/login".to_owned()),
                Just(String::new()),
            ]
                .prop_map(|v| vec![("auth_url".to_owned(), json!(v))]),
            1 => prop_oneof![Just("http://detect.edu.cn/".to_owned()), Just(String::new())].prop_map(|v| {
                vec![("trigger_url".to_owned(), json!(v))]
            }),
            1 => prop_oneof![Just("t1".to_owned()), Just(String::new())].prop_map(|v| {
                vec![("active_task".to_owned(), json!(v))]
            }),
            // 密码三态契约：null 保留 / 空串清除 / 非空串加密更新
            2 => (0u8..=2).prop_map(|i| {
                vec![(
                    "password".to_owned(),
                    match i {
                        0 => Value::Null,
                        1 => json!(""),
                        _ => json!("new-pass-123"),
                    },
                )]
            }),
            1 => prop_arb_network_payload().prop_map(|v| vec![("network".to_owned(), v)]),
            1 => prop_oneof![Just("default"), Just("secondary")].prop_map(|v| {
                vec![("active_profile_id".to_owned(), json!(v))]
            }),
            // 渠道与绑定复合族：单独改渠道会因绑定校验被拒，必须成对出现
            2 => prop_oneof![
                Just(vec![("login_channel".to_owned(), json!("browser"))]),
                Just(vec![
                    ("login_channel".to_owned(), json!("http")),
                    ("active_http_task".to_owned(), json!("portal-http")),
                ]),
                Just(vec![
                    ("login_channel".to_owned(), json!("script")),
                    ("active_script_task".to_owned(), json!("portal-login")),
                ]),
            ],
        ]
    }

    /// 混生条目（P1）：合法族 / 必拒条目 / 未知键 / carrier_custom
    fn prop_arb_any_entry() -> impl proptest::strategy::Strategy<Value = PropFamily> {
        prop_oneof![
            4 => prop_arb_valid_family(),
            3 => prop_arb_reject_entry().prop_map(|e| vec![e]),
            2 => (
                prop_oneof![
                    Just("future_field".to_owned()),
                    Just("hidden_flag".to_owned()),
                    Just("x_setting".to_owned()),
                ],
                prop_oneof![
                    Just(Value::Null),
                    any::<bool>().prop_map(|b| json!(b)),
                    (0i64..=1000).prop_map(|n| json!(n)),
                    Just(json!({"nested": true})),
                    Just(json!(["a", 1])),
                ],
            )
                .prop_map(|(k, v)| vec![(k, v)]),
            1 => Just(vec![("carrier_custom".to_owned(), json!("历史字段"))]),
        ]
    }

    /// 未知键不得进入配置：settings 顶层键集恒为 schema 四键（无字段污染）
    fn prop_assert_no_pollution(
        settings_out: &Value,
    ) -> Result<(), proptest::test_runner::TestCaseError> {
        let keys: Vec<&str> = settings_out
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let expected = [
            "active_profile_id",
            "auto_switch",
            "config_version",
            "global",
        ];
        prop_assert!(
            keys.len() == expected.len() && keys.iter().all(|k| expected.contains(k)),
            "settings 顶层出现未知键污染: {:?}",
            keys
        );
        Ok(())
    }

    /// 断言单个合法键精确生效（深度断言，含前端→后端字段名映射）
    fn prop_assert_valid_entry(
        key: &str,
        payload: &Value,
        settings_out: &Value,
        profile_out: &Value,
    ) -> Result<(), proptest::test_runner::TestCaseError> {
        match key {
            // 1:1 段（retry/app_settings 完成前端名映射后逐子键对照）
            "pause" | "logging" | "retry" | "app_settings" | "browser" | "worker" | "updater" => {
                let target_key = match key {
                    "retry" => "retry_settings",
                    "app_settings" => "app",
                    other => other,
                };
                let section = &settings_out["global"][target_key];
                let obj = payload.as_object().expect("合法族载荷必为对象");
                for (k, v) in obj {
                    prop_assert_eq!(&section[k], v, "合法键 {}.{} 必须精确生效", target_key, k);
                }
            }
            "monitor" => {
                let mon = &settings_out["global"]["monitor"];
                let obj = payload.as_object().expect("monitor 载荷必为对象");
                // 前端名 → 后端名的 1:1 映射子集
                const MAP: [(&str, &str); 8] = [
                    ("check_interval_seconds", "check_interval"),
                    ("enable_tcp_check", "tcp_enabled"),
                    ("enable_http_check", "http_enabled"),
                    ("enable_url_check", "url_enabled"),
                    ("strict_login_mode", "strict_login_mode"),
                    ("disable_proxy", "disable_proxy"),
                    ("network_check_timeout", "tcp_timeout"),
                    ("post_login_delay", "post_login_delay"),
                ];
                for (front, back) in MAP {
                    if let Some(v) = obj.get(front) {
                        prop_assert_eq!(&mon[back], v, "monitor.{} → {} 必须精确生效", front, back);
                    }
                }
                if let Some(v) = obj.get("ping_targets") {
                    prop_assert_eq!(&mon["tcp_targets"], v, "monitor.ping_targets → tcp_targets");
                }
                if let Some(v) = obj.get("test_urls") {
                    prop_assert_eq!(&mon["http_targets"], v, "monitor.test_urls → http_targets");
                }
                if let Some(items_v) = obj.get("url_check_urls") {
                    let items = items_v.as_array().expect("url_check_urls 必为数组");
                    let mut targets = Vec::new();
                    let mut expected = serde_json::Map::new();
                    for raw in items {
                        let raw = raw.as_str().expect("url_check_urls 条目必为字符串");
                        if let Some((u, e)) = raw.split_once('|') {
                            let u = u.trim();
                            targets.push(Value::String(u.to_owned()));
                            expected.insert(u.to_owned(), Value::String(e.trim().to_owned()));
                        } else {
                            targets.push(Value::String(raw.trim().to_owned()));
                        }
                    }
                    prop_assert_eq!(
                        &mon["url_targets"],
                        &Value::Array(targets.clone()),
                        "url_check_urls → url_targets 拆分"
                    );
                    prop_assert_eq!(
                        &mon["url_expected_responses"],
                        &Value::Object(expected),
                        "url_check_urls → url_expected_responses 拆分"
                    );
                    // 显式 enable_url_check 优先；缺省时沿用「非空即启用」兼容派生
                    let expected_enabled = obj
                        .get("enable_url_check")
                        .cloned()
                        .unwrap_or_else(|| json!(!targets.is_empty()));
                    prop_assert_eq!(
                        &mon["url_enabled"],
                        &expected_enabled,
                        "url_enabled 派生口径"
                    );
                }
                // 未指定的子字段不得被历史默认值覆盖（预设哨兵保持）
                let sentinels: [(&str, &str, Value); 5] = [
                    ("check_interval_seconds", "check_interval", json!(123)),
                    ("enable_tcp_check", "tcp_enabled", json!(false)),
                    ("enable_url_check", "url_enabled", json!(false)),
                    ("post_login_delay", "post_login_delay", json!(9)),
                    ("ping_targets", "tcp_targets", json!(["8.8.8.8:53"])),
                ];
                for (front, back, preset) in sentinels {
                    // url_check_urls 在场时 url_enabled 走派生口径（上面已断言），跳过哨兵
                    let skip = front == "enable_url_check" && obj.contains_key("url_check_urls");
                    if !obj.contains_key(front) && !skip {
                        prop_assert_eq!(
                            &mon[back],
                            &preset,
                            "未指定的 monitor.{} 必须保持预设值",
                            front
                        );
                    }
                }
            }
            "username" | "isp" | "auth_url" | "trigger_url" | "active_task" => {
                prop_assert_eq!(
                    &profile_out[key],
                    payload,
                    "凭证字段 {} 必须写入活跃 Profile",
                    key
                );
            }
            "password" => match payload {
                Value::Null => {
                    prop_assert_eq!(
                        &profile_out["password"],
                        &json!(""),
                        "password=null 保留原值（预设本就为空）"
                    );
                }
                Value::String(s) if s.is_empty() => {
                    prop_assert_eq!(
                        &profile_out["password"],
                        &json!(""),
                        "password=\"\" 清除密码"
                    );
                }
                Value::String(s) => {
                    prop_assert_eq!(
                        &profile_out["password"],
                        &json!(format!("ENC:mock:{s}")),
                        "非空密码经 save_password 加密语义"
                    );
                }
                _ => unreachable!("合法族不含非法 password"),
            },
            "network" => {
                prop_assert_eq!(
                    &profile_out["network"],
                    payload,
                    "network 必须写入活跃 Profile"
                );
            }
            "active_profile_id" => {
                prop_assert_eq!(&settings_out["active_profile_id"], payload);
            }
            "login_channel" | "active_http_task" | "active_script_task" => {
                prop_assert_eq!(
                    &profile_out[key],
                    payload,
                    "渠道/绑定必须写入活跃 Profile（{}）",
                    key
                );
            }
            other => unreachable!("未知合法族 {}", other),
        }
        Ok(())
    }

    /// 把合法族去重后合成一个 patch，返回（patch, 生效条目）
    fn prop_build_patch(families: Vec<PropFamily>) -> (Value, PropFamily) {
        let mut obj = serde_json::Map::new();
        let mut applied: PropFamily = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for fam in families {
            if fam.iter().all(|(k, _)| !seen.contains(k.as_str())) {
                for (k, v) in fam {
                    seen.insert(k.clone());
                    obj.insert(k.clone(), v.clone());
                    applied.push((k, v));
                }
            }
        }
        (Value::Object(obj), applied)
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【属性 P1·①④】任意 patch（合法/必拒/未知键混生，含顶层非对象）：
        /// 永不 panic，仅产出 Ok 或 4xx 类错误；Ok 时 settings 顶层无未知键污染；
        /// Err 时两域状态与落盘计数完全不变（原子语义的对外面）。
        #[test]
        fn prop_patch_mixed_never_panics(
            entries in proptest::collection::vec(prop_arb_any_entry(), 0..=6),
            root_shape in 0u8..=4,
        ) {
            prop_block_on(async move {
                let (config, profiles, tasks, inner) = prop_state();
                let settings_before = prop_settings_json(&inner);
                let profile_before = prop_profile_json(&inner);
                let save_before = inner.lock().unwrap().save_calls;

                // 顶层形状：0=对象 / 1..=4=非对象（数组/字符串/数字/null）
                let patch = if root_shape == 0 {
                    let mut obj = serde_json::Map::new();
                    for fam in entries {
                        for (k, v) in fam {
                            obj.insert(k, v);
                        }
                    }
                    Value::Object(obj)
                } else {
                    match root_shape {
                        1 => json!([1, 2, 3]),
                        2 => json!("not-an-object"),
                        3 => json!(42),
                        _ => Value::Null,
                    }
                };

                let result = apply_flat_settings_patch(&config, &profiles, &tasks, &patch).await;
                match result {
                    Ok(()) => {
                        if root_shape == 0 {
                            prop_assert_no_pollution(&prop_settings_json(&inner))?;
                        } else {
                            // 非对象 patch：无字段可合并，直接成功不落盘
                            prop_assert_eq!(
                                prop_settings_json(&inner),
                                settings_before,
                                "非对象 patch 不得改动全局设置"
                            );
                            prop_assert_eq!(
                                prop_profile_json(&inner),
                                profile_before,
                                "非对象 patch 不得改动活跃 Profile"
                            );
                        }
                    }
                    Err(e) => {
                        prop_assert!(
                            e.status().is_client_error(),
                            "拒绝必须为 4xx 类错误，实际 {e:?}"
                        );
                        prop_assert_eq!(
                            prop_settings_json(&inner),
                            settings_before,
                            "拒绝路径不得改动全局设置"
                        );
                        prop_assert_eq!(
                            prop_profile_json(&inner),
                            profile_before,
                            "拒绝路径不得改动活跃 Profile"
                        );
                        prop_assert_eq!(
                            inner.lock().unwrap().save_calls,
                            save_before,
                            "拒绝路径不得落盘"
                        );
                    }
                }
                Ok(())
            })?;
        }

        /// 【属性 P2·②】合法 patch 精确生效：随机选定字段深度断言新值（含
        /// monitor 前后端映射与「非空即启用」派生），未指定字段保持预设；
        /// settings 顶层无未知键污染。
        #[test]
        fn prop_valid_patch_precise_effect(
            families in proptest::collection::vec(prop_arb_valid_family(), 1..=4),
        ) {
            prop_block_on(async move {
                let (config, profiles, tasks, inner) = prop_state();
                let (patch, applied) = prop_build_patch(families);

                if let Err(e) = apply_flat_settings_patch(&config, &profiles, &tasks, &patch).await {
                    return Err(proptest::test_runner::TestCaseError::fail(format!(
                        "合法 patch 必须成功: {e:?}\npatch: {patch:?}"
                    )));
                }

                let settings_out = prop_settings_json(&inner);
                let profile_out = prop_profile_json(&inner);
                for (k, v) in &applied {
                    prop_assert_valid_entry(k, v, &settings_out, &profile_out)?;
                }
                prop_assert_no_pollution(&settings_out)?;
                Ok(())
            })?;
        }

        /// 【属性 P3·③】含必拒条目的 patch 一律整体拒绝（4xx），且两域状态与
        /// 落盘计数完全不变——合并是原子语义，绝不部分生效。
        #[test]
        fn prop_patch_rejection_is_atomic(
            rejects in proptest::collection::vec(prop_arb_reject_entry(), 1..=2),
            extras in proptest::collection::vec(prop_arb_any_entry(), 0..=3),
        ) {
            prop_block_on(async move {
                let (config, profiles, tasks, inner) = prop_state();
                let settings_before = prop_settings_json(&inner);
                let profile_before = prop_profile_json(&inner);
                let save_before = inner.lock().unwrap().save_calls;

                // 与必拒条目同顶层键的额外条目必须剔除：Map 后写覆盖会让
                // 「必拒」被合法值顶掉，破坏「必拒」前提
                let mut obj = serde_json::Map::new();
                let mut reject_keys = std::collections::HashSet::new();
                for (k, v) in rejects {
                    reject_keys.insert(k.clone());
                    obj.insert(k, v);
                }
                for fam in extras {
                    if fam.iter().any(|(k, _)| reject_keys.contains(k.as_str())) {
                        continue;
                    }
                    for (k, v) in fam {
                        obj.insert(k, v);
                    }
                }
                let patch = Value::Object(obj);

                let result = apply_flat_settings_patch(&config, &profiles, &tasks, &patch).await;
                match result {
                    Ok(()) => Err(proptest::test_runner::TestCaseError::fail(format!(
                        "含必拒条目的 patch 必须被整体拒绝\npatch: {patch:?}"
                    ))),
                    Err(e) => {
                        prop_assert!(
                            e.status().is_client_error(),
                            "拒绝必须为 4xx 类错误，实际 {e:?}"
                        );
                        prop_assert_eq!(
                            prop_settings_json(&inner),
                            settings_before,
                            "拒绝路径不得改动全局设置（原子性）"
                        );
                        prop_assert_eq!(
                            prop_profile_json(&inner),
                            profile_before,
                            "拒绝路径不得改动活跃 Profile（原子性）"
                        );
                        prop_assert_eq!(
                            inner.lock().unwrap().save_calls,
                            save_before,
                            "拒绝路径不得落盘（原子性）"
                        );
                        Ok(())
                    }
                }
            })?;
        }

        /// 【属性 P5·⑤】两次合法 patch 的先后应用 ≡ 等价单次合并应用
        /// （顶层键不相交的族划分抽样；两域终态逐字段相等）。
        #[test]
        fn prop_two_valid_patches_associative(
            families in proptest::collection::vec(prop_arb_valid_family(), 2..=5),
        ) {
            prop_block_on(async move {
                // 全局去重后按奇偶划分到两个不相交 patch（≥2 条保证两侧非空）
                let mut seen = std::collections::HashSet::new();
                let mut distinct: Vec<PropFamily> = Vec::new();
                for fam in families {
                    if fam.iter().all(|(k, _)| !seen.contains(k.as_str())) {
                        for (k, _) in &fam {
                            seen.insert(k.clone());
                        }
                        distinct.push(fam);
                    }
                }
                if distinct.len() < 2 {
                    // 族去重后不足两条：划分退化，跳过本轮抽样（生成器权重下极少发生）
                    return Ok(());
                }
                let mut map1 = serde_json::Map::new();
                let mut map2 = serde_json::Map::new();
                for (i, fam) in distinct.iter().enumerate() {
                    let target = if i % 2 == 0 { &mut map1 } else { &mut map2 };
                    for (k, v) in fam {
                        target.insert(k.clone(), v.clone());
                    }
                }
                let p1 = Value::Object(map1);
                let p2 = Value::Object(map2);
                let mut merged = serde_json::Map::new();
                for (k, v) in p1.as_object().unwrap() {
                    merged.insert(k.clone(), v.clone());
                }
                for (k, v) in p2.as_object().unwrap() {
                    merged.insert(k.clone(), v.clone());
                }
                let merged = Value::Object(merged);

                // 状态 A：先后应用两次
                let (c1, pr1, t1, inner1) = prop_state();
                apply_flat_settings_patch(&c1, &pr1, &t1, &p1)
                    .await
                    .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("P1 合法必须成功: {e:?}")))?;
                apply_flat_settings_patch(&c1, &pr1, &t1, &p2)
                    .await
                    .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("P2 合法必须成功: {e:?}")))?;
                // 状态 B：单次合并应用
                let (c2, pr2, t2, inner2) = prop_state();
                apply_flat_settings_patch(&c2, &pr2, &t2, &merged)
                    .await
                    .map_err(|e| proptest::test_runner::TestCaseError::fail(format!("合并 patch 合法必须成功: {e:?}")))?;

                prop_assert_eq!(
                    prop_settings_json(&inner1),
                    prop_settings_json(&inner2),
                    "settings 终态必须等价（先后应用 ≡ 单次合并）"
                );
                prop_assert_eq!(
                    prop_profile_json(&inner1),
                    prop_profile_json(&inner2),
                    "Profile 终态必须等价（先后应用 ≡ 单次合并）"
                );
                Ok(())
            })?;
        }
    }
}
