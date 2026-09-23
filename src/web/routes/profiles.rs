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

use crate::config::{ConfigApi, LoginChannel, ProfileApi, ProfileData};
use crate::engine::{EngineApi, EngineCommand, ProfileSwitchSource};
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};

/// POST /api/profiles/{id} 请求体：创建 Profile（可选匹配/认证字段与 PUT 同语义，创建即完整落盘）
#[derive(Deserialize)]
pub struct ProfileCreateBody {
    /// 与路径参数冗余的历史字段：路径已携带 id，body 内可省略（路径优先）
    pub id: Option<String>,
    pub name: String,
    pub username: String,
    pub password: Zeroizing<String>,
    /// 编辑器同屏的可选匹配/认证设置：创建即完整落盘（此前仅收 4 字段，
    /// 网关/SSID/认证地址等会被静默丢弃，须再编辑一次才能保存）
    pub auth_url: Option<String>,
    /// 自定义重定向触发地址；认证地址留空时生效，空值使用内置默认值
    pub trigger_url: Option<String>,
    pub isp: Option<String>,
    pub gateway_ip: Option<String>,
    pub wifi_ssid: Option<String>,
    pub active_task: Option<String>,
    pub login_channel: Option<LoginChannel>,
    /// 直连渠道绑定的直连任务 ID（空 = 未绑定，直连登录不可用）
    pub active_http_task: Option<String>,
}

/// 校验 http/https URL 并返回 trim 结果（认证地址/重定向触发地址共用；空串直通）
fn validate_http_url(label: &str, raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Ok(trimmed);
    }
    let parsed = trimmed
        .parse::<url::Url>()
        .map_err(|_| ApiError::BadRequest(format!("{label}格式非法: {trimmed}")))?;
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
        .ok_or_else(|| ApiError::BadRequest(format!("{label}缺少主机名: {trimmed}")))?;
    if host.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "{label}缺少主机名: {trimmed}"
        )));
    }
    Ok(trimmed)
}

/// 校验方案的直连任务绑定（新建 / 更新 / `PATCH /api/config` 三条保存路径共用）。
///
/// 为什么直连比浏览器渠道多出这一道：浏览器渠道有内置默认任务兜底，绑定为空或
/// 失效时登录会回退到 `default`，最坏也只是"用了默认任务"；直连**没有**兜底
/// （门户地址无法内置），绑定为空或指错任务时登录必然失败——而那一刻用户早已离开
/// 保存页面，看到的是"登录失败"而非"配置没选对"。放过去等于把配置错误伪装成运行
/// 错误，故必须在保存时就拦下。
///
/// 只读一次任务并同时判「存在」与「类型」：仅查存在性会放行被改成 browser/script
/// 的同名任务，与 `LoginOrchestrator::resolve_http_task` 的判定口径保持一致。
pub(crate) async fn validate_http_task_binding(
    tasks: &Arc<dyn TaskApi>,
    login_channel: LoginChannel,
    active_http_task: &str,
) -> Result<(), ApiError> {
    if login_channel != LoginChannel::Http {
        // 浏览器渠道不看这个字段：切回浏览器后残留的绑定无害，不强制用户清空
        return Ok(());
    }
    let id = active_http_task.trim();
    if id.is_empty() {
        return Err(ApiError::BadRequest("请为直连渠道选择一个直连任务".into()));
    }
    match tasks.load_task(id).await {
        Ok(crate::tasks::TaskKind::Http(_)) => Ok(()),
        Ok(other) => {
            tracing::warn!(
                task_id = id,
                task_type = other.type_name(),
                "方案绑定的任务不是直连任务，保存被拒"
            );
            Err(ApiError::BadRequest(format!(
                "直连任务 {id} 不存在或不是直连任务"
            )))
        }
        Err(e) => {
            tracing::warn!(task_id = id, error = %e, "方案绑定的直连任务加载失败，保存被拒");
            Err(ApiError::BadRequest(format!(
                "直连任务 {id} 不存在或不是直连任务"
            )))
        }
    }
}

/// PUT /api/profiles/{id} 请求体：字段全可选，仅覆盖出现的字段（空密码 = 保留原密码）
#[derive(Deserialize)]
pub struct ProfileUpdateBody {
    pub name: Option<String>,
    pub username: Option<String>,
    pub password: Option<Zeroizing<String>>,
    /// 显式清除已保存密码。
    ///
    /// `password` 的空串语义是「未修改，保留原密码」，因此**无法**用它表达清除
    /// （`Some("")` 与 `None` 在此接口等价）。清除曾只能通过 `PATCH /api/config`
    /// 对活跃方案完成——那是「设置 · 账号」页专用路径。账号页并入方案页后，
    /// 该能力必须在本接口可用，否则「清除已保存密码」这个入口会整体消失。
    /// 与 `password` 同现时以本字段为准（显式清除优先于「保留」）。
    #[serde(default)]
    pub clear_password: bool,
    pub auth_url: Option<String>,
    /// 自定义重定向触发地址；认证地址留空时生效，空值使用内置默认值
    pub trigger_url: Option<String>,
    pub isp: Option<String>,
    pub gateway_ip: Option<String>,
    pub wifi_ssid: Option<String>,
    pub active_task: Option<String>,
    pub login_channel: Option<LoginChannel>,
    /// 直连渠道绑定的直连任务 ID（空 = 未绑定，直连登录不可用）
    pub active_http_task: Option<String>,
}

/// POST /api/profiles/switch 请求体：要切换到的目标 Profile ID
#[derive(Deserialize)]
pub struct SwitchBody {
    pub profile_id: String,
}

/// POST /api/profiles/auto-switch 请求体：启用/禁用自动切换
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
///
/// 响应带 `has_password`（口径同 `GET /api/config`）：方案编辑器据此显示
/// 「已保存 / 未设置」并决定是否提供「清除已保存密码」。只回 `settings` 一个
/// 空串密码时，前端无法区分「没设密码」和「有密码但被抹掉了」——两者都会渲染成
/// 空输入框，占位文案只能猜。
pub async fn get_profile(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = config.load_profile(&id)?;
    let has_password =
        crate::web::routes::config::effective_has_password(config.as_ref(), &profile);
    // 避免将密码（密文）泄露给前端
    profile.password = String::new();
    Ok(data(serde_json::json!({
        "settings": serde_json::to_value(profile)?,
        "has_password": has_password,
    })))
}

/// 方案分享载荷的格式版本号
///
/// 导入时校验：缺失或非数字视为非本应用导出的文件（拒绝而非猜测），
/// 高于当前值提示"由更新版本导出"。
const PROFILE_SHARE_FORMAT: u32 = 1;

/// 构造方案分享载荷
///
/// **剔除 `username` 与 `password`**：密码在磁盘上是 `ENC:` 密文，密钥存放于
/// `~/.campus_network_auth/.enc_key.rs` 而非 config 目录，跨机器无法解密；若原样
/// 带出，接收方的 `save_password` 会因 `can_decrypt_password` 为假而把它当**明文
/// 再加密一次**（双重加密），导入后密码变成"那段密文本身"，登录必然失败且
/// `password_decryption_failed` 仍为 false，排障时毫无线索。账号同样剔除——那是
/// 接收方自己的学号，跟着走只会误导（也避免导出者无意识泄露）。
///
/// `active_task` / `active_http_task` 置空：两者都是"本机任务 ID"的绑定关系，接收方
/// 通常没有同名任务。浏览器侧保留会静默回退到默认任务
/// （`LoginOrchestrator::resolve_active_task`），直连侧保留则会直接指向一个不存在的
/// 任务——两种结果都在对方的机器上表现为"配置看起来是好的但登录不对"。
fn build_share_payload(profile: &ProfileData) -> Value {
    serde_json::json!({
        "campus_auth_profile": PROFILE_SHARE_FORMAT,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "app_version": env!("CARGO_PKG_VERSION"),
        // 导入时的命名建议：取自源方案 id（恒为 ASCII slug），比拿方案名重算更可用。
        // 方案名多为中文，slug 后为空会退化成 imported-profile/-2/-3，接收方看到一串
        // 无意义 id；直接沿用源 id 则导入后就是 dorm / home 这类可辨识的名字。
        // 仅作建议，导入方本就占用时仍会自动追加后缀。
        "suggested_id": profile.id,
        "profile": {
            "name": profile.name,
            // 明确空值而非缺字段：接收端 UI 据此提示"需自行填写账号密码"
            "username": "",
            "password": "",
            "auth_url": profile.auth_url,
            "trigger_url": profile.trigger_url,
            "isp": profile.isp,
            "gateway_ip": profile.gateway_ip,
            "wifi_ssid": profile.wifi_ssid,
            // 绑定任务不随方案迁移（接收方多半没有该任务）
            "active_task": "",
            "login_channel": profile.login_channel,
            // 直连请求参数已整体搬进直连任务（`tasks/http/<id>.json`）：方案侧只剩这条
            // 绑定关系，与 `active_task` 同口径清空。任务本身走任务页的导入导出通道。
            "active_http_task": "",
        }
    })
}

/// 从分享载荷中提取待导入的 ProfileData 与建议 ID
///
/// 格式仅认「本应用导出的信封」：顶层数据字段为可选包装（兼容外壳/包裹写法），
/// 但 `campus_auth_profile` 版本号必须存在——缺少即视为非本应用文件。宁可明确
/// 报错，也不做"猜测字段"的宽松解析：错误猜测会导入一个看似成功却少了关键
/// 判定关键字的方案，用户要到下次登录失败才发现。
///
/// 返回的第三个值 `legacy_http_config_dropped` 表示"这份文件里还带着 v9 的内联直连
/// 配置（`http_url` 非空）"。新 `ProfileData` 没有这些字段，serde 会静默忽略——静默
/// 正是问题：用户导入后会发现直连方案"没有请求地址了"却不知道为什么。故把事实回报给
/// 前端，由它提示"请到任务页重新配置直连任务"。
fn parse_share_payload(body: &Value) -> Result<(ProfileData, String, bool), ApiError> {
    // 兼容 API 信封 { data: {...} } 与 { profile: {...} } 包裹写法
    let root = body.get("data").filter(|v| v.is_object()).unwrap_or(body);
    let version = root
        .get("campus_auth_profile")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ApiError::BadRequest("不是有效的方案分享文件（缺少 campus_auth_profile 标记）".into())
        })?;
    if version > u64::from(PROFILE_SHARE_FORMAT) {
        return Err(ApiError::BadRequest(format!(
            "该方案由更新版本的应用导出（格式 {version}，当前支持 {PROFILE_SHARE_FORMAT}），请先升级"
        )));
    }
    let obj = root
        .get("profile")
        .filter(|v| v.is_object())
        .ok_or_else(|| ApiError::BadRequest("分享文件缺少 profile 字段".into()))?;

    // 逐字段按类型取值：类型不符直接忽略该字段（serde 的默认值兜底），
    // 全量 as_str 转换失败会得到空串而非报错，符合"部分字段缺失仍可导入"的预期
    let text = |key: &str| -> String {
        obj.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut profile = ProfileData {
        name: text("name"),
        auth_url: text("auth_url"),
        trigger_url: text("trigger_url"),
        isp: text("isp"),
        gateway_ip: text("gateway_ip"),
        wifi_ssid: text("wifi_ssid"),
        ..Default::default()
    };
    // 枚举字段显式解析：非法值报错而非静默退回默认，避免"导入成功但渠道变了"
    if let Some(v) = obj.get("login_channel") {
        profile.login_channel = serde_json::from_value(v.clone()).map_err(|_| {
            ApiError::BadRequest("分享文件的 login_channel 非法（仅 browser/http）".into())
        })?;
    }
    // 老分享文件里的 `http_*` 键（含 `http_method` / `http_ignore_https_errors`）一律
    // 忽略：这些字段在 v10 已不存在，serde 静默丢弃是正确行为，只在下面回报一个布尔
    // 供前端提示，不做校验也不做迁移（迁移只对**本机**的 v9 磁盘配置负责，见
    // `config::migration::migrate_v9_to_v10`；来自别人机器的地址搬过去也未必可用）。
    let legacy_http_config_dropped = !text("http_url").trim().is_empty();
    if legacy_http_config_dropped {
        tracing::info!("分享文件包含 v9 内联直连配置，已忽略（直连配置现由任务承载）");
    }
    if profile.name.trim().is_empty() {
        return Err(ApiError::BadRequest("分享文件缺少方案名称".into()));
    }
    if !profile.auth_url.trim().is_empty() {
        validate_http_url("认证地址", &profile.auth_url)?;
    }
    if !profile.trigger_url.trim().is_empty() {
        validate_http_url("重定向触发地址", &profile.trigger_url)?;
    }

    // 建议 ID：优先用导出方带的 `suggested_id`（源方案 id，恒为 ASCII slug），
    // 缺失时才退回按名称推导。**必须与 `create_profile` 走同一 slug 规则**
    // （`_` 归一为 `-`）——否则这里探测 `dorm_2` 是否占用、实际落盘却是 `dorm-2`，
    // 冲突判定与被写入的 id 不一致，重名时会静默覆盖或报意外冲突。
    let suggested = {
        let from_payload = root
            .get("suggested_id")
            .and_then(Value::as_str)
            .map(crate::config::profiles::slugify_id)
            .filter(|s| !s.is_empty());
        from_payload.unwrap_or_else(|| {
            let by_name = crate::config::profiles::slugify_id(&profile.name);
            if by_name.is_empty() {
                "imported-profile".to_string()
            } else {
                by_name
            }
        })
    };
    Ok((profile, suggested, legacy_http_config_dropped))
}

/// GET /api/profiles/{id}/export — 导出方案为可分享的 JSON（剔除账号与密码）
pub async fn export_profile(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let profile = config.load_profile(&id)?;
    tracing::info!(profile_id = %id, "导出方案");
    Ok(data(build_share_payload(&profile)))
}

/// POST /api/profiles/import — 导入分享的方案 JSON
///
/// ID 冲突时自动追加 `-2`/`-3` 后缀而非报 409：导入的语义是"新增一份可用方案"，
/// 让用户先解决命名冲突再重试属于把内部标识泄漏成用户负担；同时绝不覆盖既有
/// 方案（覆盖会连带清掉对方的账号密码）。
pub async fn import_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (profile, suggested, legacy_http_config_dropped) = parse_share_payload(&body)?;

    // 在既有方案中挑一个未占用的 ID
    let existing: std::collections::HashSet<String> =
        profiles.list_profiles().into_iter().map(|p| p.id).collect();
    let mut target_id = suggested.clone();
    if existing.contains(&target_id) {
        // 上限兜底：极端情况下（10k 个同名）不无限循环
        let mut suffix = 2;
        loop {
            let candidate = format!("{suggested}-{suffix}");
            if !existing.contains(&candidate) {
                target_id = candidate;
                break;
            }
            suffix += 1;
            if suffix > 10_000 {
                return Err(ApiError::Conflict("同名方案过多，请先清理后再导入".into()));
            }
        }
    }

    let mut profile = profile;
    profile.id = target_id.clone();
    profiles.create_profile(&target_id, profile).await?;
    tracing::info!(profile_id = %target_id, "导入方案");
    // `legacy_http_config_dropped`：老分享文件带着 v9 内联直连配置时，前端据此提示
    // 「已忽略旧版直连配置，请到任务页重新配置」。字段始终回传（false 也表示明确结论），
    // 免得前端在"没这个键"和"键为 false"之间猜。
    Ok(data(serde_json::json!({
        "id": target_id,
        "legacy_http_config_dropped": legacy_http_config_dropped,
    })))
}

/// POST /api/profiles/{id} — 创建 Profile
///
/// body 必填 `name/username/password`（password 空串=不设独立密码）；
/// 可选 `auth_url/trigger_url/isp/gateway_ip/wifi_ssid/active_task/active_http_task`
/// 与 PUT 同语义，支持创建时一次带上编辑器内的全部字段。
pub async fn create_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
    Json(body): Json<ProfileCreateBody>,
) -> Result<Json<Value>, ApiError> {
    // 路径 id 优先于 body id；两者皆空则明确拒绝（body.id 已改为可选）
    let target_id = if id.is_empty() {
        body.id.clone().unwrap_or_default()
    } else {
        id
    };
    if target_id.is_empty() {
        return Err(ApiError::BadRequest(
            "缺少 profile id（路径或 body 至少提供一处）".into(),
        ));
    }
    // 纯新建语义：不从既有档案合并字段。命中既有 id 时由 Service 层的原子
    // create（ProfileIdConflict）统一拒绝为 409——此前这里 load_profile 后
    // 合并覆盖的 upsert 式写法是死代码，且其中的空密码分支一旦在放宽冲突
    // 检查后生效，会把既有加密密码静默清空（WE2-4），直接删除该陷阱。
    let mut profile = crate::config::ProfileData {
        id: target_id.clone(),
        name: body.name,
        username: body.username,
        ..Default::default()
    };
    // 空密码表示“不设置独立密码”，必须保持为空；若把空串加密成 ENC:，
    // 后续 has_password 会误判为已有密码，而运行时解密后仍为空。
    profile.password = if body.password.is_empty() {
        String::new()
    } else {
        config
            .encrypt_password(&body.password)
            .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?
    };
    // 可选设置字段与 PUT 语义一致（含 URL 校验），创建即完整落盘
    if let Some(auth_url) = body.auth_url {
        profile.auth_url = validate_http_url("认证地址", &auth_url)?;
    }
    if let Some(trigger_url) = body.trigger_url {
        profile.trigger_url = validate_http_url("重定向触发地址", &trigger_url)?;
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
    if let Some(login_channel) = body.login_channel {
        profile.login_channel = login_channel;
    }
    if let Some(active_http_task) = body.active_http_task {
        profile.active_http_task = active_http_task;
    }
    // 直连渠道必须有可用的直连任务绑定（见 validate_http_task_binding 的说明）
    validate_http_task_binding(&tasks, profile.login_channel, &profile.active_http_task).await?;
    profiles.create_profile(&target_id, profile).await?;
    tracing::info!(profile_id = %target_id, "创建 Profile");
    Ok(data(Value::String("ok".into())))
}

/// PUT /api/profiles/{id} — 更新 Profile
pub async fn update_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
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
        profile.auth_url = validate_http_url("认证地址", &auth_url)?;
    }
    if let Some(trigger_url) = body.trigger_url {
        profile.trigger_url = validate_http_url("重定向触发地址", &trigger_url)?;
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
    if let Some(login_channel) = body.login_channel {
        profile.login_channel = login_channel;
    }
    if let Some(active_http_task) = body.active_http_task {
        profile.active_http_task = active_http_task;
    }
    // 直连渠道必须有可用的直连任务绑定（与 create 同一口径，见
    // validate_http_task_binding）。注意这里判的是**合并后**的渠道与绑定：只改渠道
    // 不改绑定（或反之）时也必须整体有效。
    validate_http_task_binding(&tasks, profile.login_channel, &profile.active_http_task).await?;
    profiles
        .update_profile(&id, profile, body.clear_password)
        .await?;
    tracing::info!(profile_id = %id, "更新 Profile");
    Ok(data(Value::String("ok".into())))
}

/// DELETE /api/profiles/{id} — 删除 Profile
pub async fn delete_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    profiles.delete_profile(&id).await?;
    tracing::info!(profile_id = %id, "删除 Profile");
    Ok(data(Value::String("ok".into())))
}

/// POST /api/profiles/switch — 切换活跃 Profile
///
/// 切换本体同步完成（保留前端「响应即已生效」的时序契约：切换后立即
/// fetchConfig 必须拿到新 Profile 凭证），随后派发 Engine 的 ApplyProfile
/// 命令同步引擎派生状态（ActiveProfile 状态广播 + 监测中的即时探测与
/// 定时器重建）。历史实现只调 ProfileService、绕过 Engine：切换后引擎内
/// 的探测上下文不随切换刷新，Web 与自动切换两个入口的生效行为不一致。
pub async fn switch_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(engine): State<Arc<dyn EngineApi>>,
    Json(body): Json<SwitchBody>,
) -> Result<Json<Value>, ApiError> {
    profiles.switch_profile(&body.profile_id).await?;
    tracing::info!(profile_id = %body.profile_id, "切换活跃 Profile");
    // 磁盘+RuntimeConfig 为权威源，Engine 为派生状态：switch 已触发 reload 信号，
    // Engine 即使收不到 ApplyProfile 也会经 reload 收敛。此处派发失败不视为切换
    // 失败（否则前端看到错误但下次读取已是新 Profile，造成双状态困惑）。
    if let Err(e) = engine.try_dispatch(EngineCommand::ApplyProfile {
        profile_id: body.profile_id,
        source: ProfileSwitchSource::Manual,
    }) {
        tracing::warn!("Profile 已持久化，Engine 即时同步派发失败（将经 reload 收敛）: {e}");
    }
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
    use crate::engine::EngineError;
    use crate::tasks::{TaskApi, TaskError, TaskKind};

    #[derive(Default)]
    struct MockInner {
        profiles: Vec<ProfileData>,
        active: String,
        auto_switch: bool,
        /// switch_profile 派发的 ApplyProfile 目标 ID（验证 Engine 联动）
        dispatched_apply_profile: Vec<String>,
        /// 内存任务表：只服务 `validate_http_task_binding` 的存在性 + 类型判定
        tasks: Vec<(String, TaskKind)>,
    }

    /// 内存 EngineApi：仅记录 ApplyProfile 派发（switch 路由联动验证）
    struct MockEngineApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl EngineApi for MockEngineApi {
        fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
            if let EngineCommand::ApplyProfile { profile_id, .. } = cmd {
                self.0
                    .lock()
                    .unwrap()
                    .dispatched_apply_profile
                    .push(profile_id);
            }
            Ok(())
        }

        async fn test_network(&self) -> Result<crate::engine::TestNetworkResult, EngineError> {
            Err(EngineError::ChannelClosed)
        }
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
                    active_http_task: p.active_http_task.clone(),
                    login_channel: p.login_channel,
                    gateway_ip: p.gateway_ip.clone(),
                    wifi_ssid: p.wifi_ssid.clone(),
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

        async fn update_profile(
            &self,
            id: &str,
            data: ProfileData,
            clear_password: bool,
        ) -> Result<(), ConfigError> {
            let mut inner = self.0.lock().unwrap();
            let p = inner
                .profiles
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| ConfigError::ProfileNotFound { id: id.to_string() })?;
            let mut data = data;
            if clear_password {
                data.password = String::new();
            }
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

    /// 内存 TaskApi：只实现 `validate_http_task_binding` 用到的 `load_task`，
    /// 其余方法按「本域测试不触达」返回中性值（不 panic 以免掩盖误用）
    struct MockTaskApi(Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl TaskApi for MockTaskApi {
        async fn list_all_tasks(&self) -> Vec<crate::tasks::TaskSummary> {
            Vec::new()
        }

        async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .find(|(id, _)| id == task_id)
                .map(|(_, kind)| kind.clone())
                .ok_or_else(|| TaskError::TaskNotFound(task_id.to_string()))
        }

        async fn embed_task_config(&self, _task_id: &str, _params: &mut Value) -> bool {
            false
        }

        async fn save_task(&self, _task_id: &str, _task: &TaskKind) -> Result<(), TaskError> {
            Ok(())
        }

        async fn delete_task(&self, _task_id: &str) -> Result<(), TaskError> {
            Ok(())
        }

        async fn get_task_detail(
            &self,
            task_id: &str,
        ) -> Result<crate::tasks::TaskDetail, TaskError> {
            let config = self.load_task(task_id).await?;
            Ok(crate::tasks::TaskDetail {
                summary: crate::tasks::TaskSummary {
                    id: task_id.to_string(),
                    name: config.common().name.clone(),
                    description: config.common().description.clone(),
                    task_type: config.type_name().to_string(),
                    url: config.summary_url().to_string(),
                    http_method: config.http_request_method(),
                },
                config,
            })
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
            self.0
                .lock()
                .unwrap()
                .tasks
                .iter()
                .any(|(id, _)| id == task_id)
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

        async fn modify_settings_tx(
            &self,
            f: Box<dyn FnOnce(SettingsData) -> Result<SettingsData, String> + Send>,
        ) -> Result<Result<(), String>, ConfigError> {
            // profiles handler 测试不触达 settings 事务路径，按原样接受
            let _ = f;
            Ok(Ok(()))
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
            // profiles handler 测试不触达 runtime；误触时回退测试默认值而非 panic
            std::sync::Arc::new(crate::web::routes::test_support::test_runtime_config())
        }

        fn runtime_config_for_profile(
            &self,
            id: &str,
        ) -> Result<crate::config::RuntimeConfig, ConfigError> {
            let profile = self.load_profile(id)?;
            let mut runtime = crate::web::routes::test_support::test_runtime_config();
            runtime.profile.id = profile.id;
            runtime.profile.username = profile.username;
            runtime.profile.password = Zeroizing::new(profile.password);
            Ok(runtime)
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

    /// 多 State 提取的测试 Router：ProfileApi + ConfigApi + EngineApi + TaskApi
    /// 组合为单一 state 类型
    #[derive(Clone)]
    struct TestState {
        profiles: Arc<dyn ProfileApi>,
        config: Arc<dyn ConfigApi>,
        engine: Arc<dyn EngineApi>,
        tasks: Arc<dyn TaskApi>,
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

    impl axum::extract::FromRef<TestState> for Arc<dyn EngineApi> {
        fn from_ref(state: &TestState) -> Self {
            state.engine.clone()
        }
    }

    impl axum::extract::FromRef<TestState> for Arc<dyn TaskApi> {
        fn from_ref(state: &TestState) -> Self {
            state.tasks.clone()
        }
    }

    /// 测试用直连任务（`validate_http_task_binding` 只关心类型与 ID）
    fn http_task(id: &str) -> (String, TaskKind) {
        (
            id.to_string(),
            TaskKind::Http(crate::tasks::HttpTaskConfig {
                common: crate::tasks::CommonFields {
                    task_id: id.to_string(),
                    name: format!("直连任务 {id}"),
                    description: String::new(),
                },
                url: "http://10.1.1.55/login".into(),
                ..Default::default()
            }),
        )
    }

    /// 测试用浏览器任务：用于验证「绑定指向非直连任务」被拒
    fn browser_task(id: &str) -> (String, TaskKind) {
        (
            id.to_string(),
            TaskKind::Browser(crate::tasks::TaskConfig {
                common: crate::tasks::CommonFields {
                    task_id: id.to_string(),
                    name: format!("浏览器任务 {id}"),
                    description: String::new(),
                },
                url: "https://example.com".into(),
                ..Default::default()
            }),
        )
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            profiles: vec![profile_of("default"), profile_of("dorm")],
            active: "default".into(),
            auto_switch: false,
            dispatched_apply_profile: Vec::new(),
            tasks: vec![http_task("portal-http"), browser_task("portal-browser")],
        }));
        let state = TestState {
            profiles: Arc::new(MockProfileApi(inner.clone())),
            config: Arc::new(MockConfigApi(inner.clone())),
            engine: Arc::new(MockEngineApi(inner.clone())),
            tasks: Arc::new(MockTaskApi(inner.clone())),
        };
        let app = axum::Router::new()
            .route("/api/profiles", get(list_profiles))
            .route("/api/profiles/import", axum::routing::post(import_profile))
            .route("/api/profiles/{id}/export", get(export_profile))
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

    // 测试脚手架统一走共享 test_support（WE2-5：原逐文件复制的 body_json 已收敛）
    use crate::web::routes::test_support::body_json;

    /// 列表返回 map + active/auto_switch 元数据
    #[tokio::test]
    async fn test_list_profiles_shape() {
        let (app, inner) = mock_app();
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard
                .profiles
                .iter_mut()
                .find(|p| p.id == "dorm")
                .expect("mock 含 dorm");
            dorm.gateway_ip = "192.168.1.1".into();
            dorm.wifi_ssid = "Campus-Dorm-5G".into();
            dorm.password = "ENC:secret".into();
        }
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
        // 列表卡渲染依赖的字段必须回传：否则匹配规则恒显「无匹配规则」、
        // 登录方式无法展示（前端 ProfileSummary 契约，见 api/types.ts）
        assert_eq!(d["profiles"]["dorm"]["login_channel"], "browser");
        assert_eq!(d["profiles"]["dorm"]["gateway_ip"], "192.168.1.1");
        assert_eq!(d["profiles"]["dorm"]["wifi_ssid"], "Campus-Dorm-5G");
        // 密码等敏感字段不得出现在摘要中
        assert!(d["profiles"]["dorm"].get("password").is_none());
    }

    /// 切换到存在的 Profile 成功且派发 Engine ApplyProfile；不存在的返回错误
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
        {
            let g = inner.lock().unwrap();
            assert_eq!(g.active, "dorm");
            // Engine 联动：切换成功后必须派发 ApplyProfile 同步派生状态
            assert_eq!(g.dispatched_apply_profile, vec!["dorm".to_string()]);
        }

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
        // 切换失败时不得派发 Engine 命令
        assert_eq!(
            inner.lock().unwrap().dispatched_apply_profile,
            vec!["dorm".to_string()]
        );
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

    /// 创建时可选设置字段（网关/SSID/认证地址等）完整落盘，不再被静默丢弃。
    /// 直连渠道的请求参数已不在方案里，方案只落**任务绑定**这条关系。
    #[tokio::test]
    async fn test_create_profile_persists_optional_settings() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/full-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "full-profile",
                            "name": "完整方案",
                            "username": "student",
                            "password": "secret",
                            "auth_url": "http://10.1.1.55/",
                            "trigger_url": "http://www.msftconnecttest.com/connecttest.txt",
                            "isp": "电信",
                            "gateway_ip": "192.168.1.1",
                            "wifi_ssid": "Campus-Dorm-5G",
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

        let guard = inner.lock().unwrap();
        let created = guard
            .profiles
            .iter()
            .find(|p| p.id == "full-profile")
            .unwrap();
        assert_eq!(created.auth_url, "http://10.1.1.55/");
        assert_eq!(
            created.trigger_url,
            "http://www.msftconnecttest.com/connecttest.txt"
        );
        assert_eq!(created.isp, "电信");
        assert_eq!(created.gateway_ip, "192.168.1.1");
        assert_eq!(created.wifi_ssid, "Campus-Dorm-5G");
        assert_eq!(created.login_channel, LoginChannel::Http);
        assert_eq!(created.active_http_task, "portal-http");
    }

    // ============ 直连任务绑定校验（直连没有兜底任务，错绑必须在保存时就拦） ============

    /// 直连渠道未绑定任务 → 400：放过去只会在登录时才失败，那时用户已离开保存页
    #[tokio::test]
    async fn test_create_http_profile_without_binding_is_rejected() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/no-binding")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "直连未绑定",
                            "username": "u",
                            "password": "p",
                            "login_channel": "http"
                        })
                        .to_string(),
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
            "文案须指向「选任务」: {json}"
        );
        assert!(
            !inner
                .lock()
                .unwrap()
                .profiles
                .iter()
                .any(|p| p.id == "no-binding"),
            "校验失败不得半落盘"
        );
    }

    /// 绑定一个不存在的任务 → 400（用户看到的是"不存在或不是直连任务"）
    #[tokio::test]
    async fn test_create_http_profile_with_missing_task_is_rejected() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/ghost-binding")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "幽灵任务",
                            "username": "u",
                            "password": "p",
                            "login_channel": "http",
                            "active_http_task": "nope"
                        })
                        .to_string(),
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
                .contains("直连任务 nope 不存在或不是直连任务"),
            "{json}"
        );
    }

    /// 绑定一个浏览器任务 → 400：仅查存在性会放行，直连拿不到请求参数只能失败
    #[tokio::test]
    async fn test_create_http_profile_with_wrong_task_type_is_rejected() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/wrong-type")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "绑错类型",
                            "username": "u",
                            "password": "p",
                            "login_channel": "http",
                            "active_http_task": "portal-browser"
                        })
                        .to_string(),
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
                .contains("不存在或不是直连任务"),
            "{json}"
        );
    }

    /// 更新路径同一口径：把既有方案切成 http 却不带绑定时同样 400
    #[tokio::test]
    async fn test_update_switch_to_http_requires_binding() {
        let (app, inner) = mock_app();
        {
            // 先把 dorm 造成一个已绑定的直连方案，再验证"只改渠道不改绑定"的合并语义
            let mut g = inner.lock().unwrap();
            let dorm = g.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.login_channel = LoginChannel::Http;
        }
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "name": "改名" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "已绑空绑定的 http 方案改名也必须被拦（合并后整体无效）"
        );

        // 带上有效绑定后放行
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "改名",
                            "active_http_task": "portal-http"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let dorm = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.active_http_task, "portal-http");
        assert_eq!(dorm.name, "改名");
    }

    /// 浏览器渠道不强制绑定：残留的绑定不构成错误（切回浏览器后旧绑定无害）
    #[tokio::test]
    async fn test_browser_channel_ignores_http_binding() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "login_channel": "browser",
                            "active_http_task": "ghost"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        assert_eq!(
            g.profiles
                .iter()
                .find(|p| p.id == "dorm")
                .unwrap()
                .active_http_task,
            "ghost"
        );
    }

    /// 创建时非法认证地址 → 400（与 PUT 同一校验助手）
    #[tokio::test]
    async fn test_create_profile_rejects_bad_auth_url() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/bad-url")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "id": "bad-url",
                            "name": "x",
                            "username": "u",
                            "password": "",
                            "auth_url": "ftp://10.1.1.55/"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 校验失败不得半落盘
        assert!(
            !inner
                .lock()
                .unwrap()
                .profiles
                .iter()
                .any(|p| p.id == "bad-url"),
            "校验失败不应创建 Profile"
        );
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

    // ============ WE2-4：POST 纯新建语义（既有 id 一律 409，不合并） ============

    /// 重复 POST 同一 id 返回 409，且既有档案数据完全不变
    #[tokio::test]
    async fn test_post_existing_id_conflicts_and_keeps_data() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "改名尝试",
                            "username": "入侵者",
                            "password": "新密码"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // 既有档案不被触碰（不含 load_profile 合并路径的任何痕迹）
        let g = inner.lock().unwrap();
        let dorm = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.name, "档案 dorm");
        assert_eq!(dorm.username, "");
    }

    /// POST 新档案空密码 = 不设独立密码（落盘为空串，不是 ENC: 伪密文）
    #[tokio::test]
    async fn test_post_new_profile_empty_password_stays_empty() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/fresh")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "新档案",
                            "username": "u1",
                            "password": ""
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let fresh = g.profiles.iter().find(|p| p.id == "fresh").unwrap();
        assert_eq!(fresh.password, "", "空密码必须保持空串");
    }

    /// PUT 空密码保持既有密码不变（空串 = 未修改 的既有契约）
    #[tokio::test]
    async fn test_put_empty_password_keeps_existing() {
        let (app, inner) = mock_app();
        // 预置既有密码
        {
            let mut g = inner.lock().unwrap();
            g.profiles
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
                        serde_json::json!({
                            "name": "只改名",
                            "password": ""
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let dorm = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.password, "ENC:old-secret", "空密码不得清空既有密码");
        assert_eq!(dorm.name, "只改名");
    }

    /// GET 单个 Profile 回传 has_password，供方案页区分「已保存 / 未设置」
    #[tokio::test]
    async fn test_get_profile_reports_has_password() {
        let (app, inner) = mock_app();
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            // 预置：dorm 有密码、default 无
            dorm.password = "ENC:secret".into();
        }

        for (id, expected) in [("dorm", true), ("default", false)] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/profiles/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v = body_json(resp).await;
            assert_eq!(
                v["data"]["has_password"], expected,
                "{id} 的 has_password 判定错误"
            );
            // 密码本体仍不得出现在响应里（has_password 只是布尔）
            assert_eq!(v["data"]["settings"]["password"], "");
        }
    }

    /// clear_password 显式清除已保存密码（空串 password 无法表达该意图）
    #[tokio::test]
    async fn test_put_clear_password_empties_existing_password() {
        let (app, inner) = mock_app();
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.password = "ENC:old-secret".into();
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "clear_password": true }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let dorm = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.password, "", "clear_password 必须真的清空密码");
    }

    /// clear_password 缺省（老客户端不传）时仍保留既有密码，语义不变
    #[tokio::test]
    async fn test_put_without_clear_password_keeps_existing() {
        let (app, inner) = mock_app();
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.password = "ENC:old-secret".into();
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "name": "x" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let g = inner.lock().unwrap();
        let dorm = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(
            dorm.password, "ENC:old-secret",
            "未传 clear_password 不得清空"
        );
    }

    // ============ 方案分享（导出 / 导入） ============

    /// 给名为 `id` 的方案填充可用于分享断言的全部字段
    fn seed_shareable(inner: &Arc<std::sync::Mutex<MockInner>>, id: &str) {
        let mut g = inner.lock().unwrap();
        let p = g
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .expect("方案存在");
        p.name = "宿舍移动".into();
        p.username = "20230001".into();
        p.password = "ENC:Az3tbg8xvGEGbXsOZ".into();
        p.auth_url = "http://10.1.1.55/".into();
        p.isp = "移动".into();
        p.gateway_ip = "10.1.1.1".into();
        p.wifi_ssid = "Campus-Dorm".into();
        p.active_task = "dorm-checkin".into();
        p.login_channel = LoginChannel::Http;
        // 直连请求参数已在任务里，方案侧只剩这条绑定（导出时同样会被清空）
        p.active_http_task = "portal-http".into();
    }

    async fn export_of(app: &axum::Router, id: &str) -> Value {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/profiles/{id}/export"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    /// 导出必须剔除账号与密码：密码是跨机器不可解的 ENC: 密文，原样带出会被
    /// 接收方当作明文**再加密一次**（双重加密），导入后登录必然失败且无任何提示。
    /// 两条任务绑定（浏览器 / 直连）同样清空：任务属于本机资源。
    #[tokio::test]
    async fn test_export_strips_credentials_and_binding() {
        let (app, inner) = mock_app();
        seed_shareable(&inner, "dorm");

        let json = export_of(&app, "dorm").await;
        let raw = json.to_string();
        let profile = &json["data"]["profile"];

        // 账号与密码双清（空串占位，便于前端提示"需自行填写"）
        assert_eq!(profile["username"], "");
        assert_eq!(profile["password"], "");
        // 密文本身也不得出现在响应任何位置
        assert!(!raw.contains("ENC:"), "导出体不得含密文: {raw}");
        assert!(!raw.contains("20230001"), "导出体不得含账号: {raw}");
        // 任务绑定不随方案迁移：接收方多半没有同名任务，保留会让"看起来配好了但登录不对"
        assert_eq!(profile["active_task"], "");
        assert_eq!(profile["active_http_task"], "");

        // 方案自身的可分享字段完整保留（直连请求参数已不在方案里，改由任务承载）
        assert_eq!(profile["name"], "宿舍移动");
        assert_eq!(profile["login_channel"], "http");
        assert_eq!(profile["auth_url"], "http://10.1.1.55/");
        assert_eq!(profile["gateway_ip"], "10.1.1.1");
        assert_eq!(profile["wifi_ssid"], "Campus-Dorm");
        // 旧内联直连键不得再出现在导出体里（v9 的字段已不存在）
        for key in [
            "http_method",
            "http_url",
            "http_headers",
            "http_body",
            "http_success_pattern",
            "http_failure_pattern",
            "http_crypto_script",
            "http_ignore_https_errors",
        ] {
            assert!(
                profile.get(key).is_none(),
                "导出体不应再含旧内联直连字段 {key}: {raw}"
            );
        }
        // 格式标记与来源信息
        assert_eq!(json["data"]["campus_auth_profile"], 1);
        assert_eq!(json["data"]["app_version"], env!("CARGO_PKG_VERSION"));
    }

    /// 导出不存在的方案 → 404
    #[tokio::test]
    async fn test_export_missing_profile_is_not_found() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/profiles/nope/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 导出→导入往返：可分享字段逐字保留，凭据与任务绑定留空待接收方自填
    #[tokio::test]
    async fn test_export_import_roundtrip_preserves_shareable_fields() {
        let (app, inner) = mock_app();
        seed_shareable(&inner, "dorm");
        let exported = export_of(&app, "dorm").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(exported["data"].to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let new_id = json["data"]["id"].as_str().unwrap().to_string();
        assert_eq!(json["data"]["legacy_http_config_dropped"], false);

        let g = inner.lock().unwrap();
        let imported = g
            .profiles
            .iter()
            .find(|p| p.id == new_id)
            .expect("导入的方案已落盘");
        assert_eq!(imported.login_channel, LoginChannel::Http);
        assert_eq!(imported.auth_url, "http://10.1.1.55/");
        assert_eq!(imported.gateway_ip, "10.1.1.1");
        assert_eq!(imported.wifi_ssid, "Campus-Dorm");
        // 任务绑定必须为空：接收方要自己选一个直连任务（前端据此提示）
        assert_eq!(imported.active_http_task, "");
        assert_eq!(imported.active_task, "");
        // 凭据留空，由接收方自填
        assert_eq!(imported.username, "");
        assert_eq!(imported.password, "");
    }

    /// 导入老分享文件（v9 内联 `http_url` 非空）：字段被忽略，但必须**明确回报**
    /// `legacy_http_config_dropped`，否则用户看到的是"直连方案的请求地址凭空消失"
    #[tokio::test]
    async fn test_import_reports_dropped_legacy_http_config() {
        let (app, inner) = mock_app();
        let body = serde_json::json!({
            "campus_auth_profile": 1,
            "suggested_id": "legacy-dorm",
            "profile": {
                "name": "老方案",
                "login_channel": "http",
                "auth_url": "http://10.1.1.55/",
                "http_method": "POST",
                "http_url": "http://10.1.1.55/login?u={username}",
                "http_headers": "Content-Type: application/x-www-form-urlencoded",
                "http_body": "user={username}&pass={password}",
                "http_success_pattern": "登录成功",
                "http_failure_pattern": "密码错误",
                "http_crypto_script": "function transform(ctx){ return {}; }",
                "http_ignore_https_errors": false
            }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(
            json["data"]["legacy_http_config_dropped"], true,
            "老 payload 带着非空 http_url，必须回报已忽略: {json}"
        );
        let id = json["data"]["id"].as_str().unwrap().to_string();
        let g = inner.lock().unwrap();
        let imported = g.profiles.iter().find(|p| p.id == id).expect("已落盘");
        assert_eq!(
            imported.active_http_task, "",
            "导入不得凭空绑定直连任务（接收方没有该任务）"
        );
        assert_eq!(imported.auth_url, "http://10.1.1.55/");
    }

    /// 仅空 `http_url` 的老文件不算"含旧配置"：不误报提示
    #[tokio::test]
    async fn test_import_legacy_flag_false_when_http_url_empty() {
        let (app, _inner) = mock_app();
        let body = serde_json::json!({
            "campus_auth_profile": 1,
            "profile": { "name": "空直连键", "http_url": "   " }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["legacy_http_config_dropped"], false);
    }

    /// 导入 ID 冲突时自动改名而非报 409，且**不得覆盖**既有方案
    #[tokio::test]
    async fn test_import_renames_on_id_conflict_without_overwriting() {
        let (app, inner) = mock_app();
        seed_shareable(&inner, "dorm");
        let exported = export_of(&app, "dorm").await;
        // 原始方案改名会丢信息：这里直接确认既有 dorm 的凭据不被导入动作清空
        let before = {
            let g = inner.lock().unwrap();
            let p = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
            (p.username.clone(), p.password.clone())
        };

        let mut imported_ids = Vec::new();
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/profiles/import")
                        .header("content-type", "application/json")
                        .body(Body::from(exported["data"].to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            imported_ids.push(
                body_json(resp).await["data"]["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }

        // 名称「宿舍移动」slug 后为连字符形态；两次导入互不冲突且都不等于 dorm
        assert_ne!(imported_ids[0], imported_ids[1]);
        let g = inner.lock().unwrap();
        let after = g.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(
            (after.username.clone(), after.password.clone()),
            before,
            "既有方案不得被导入覆盖"
        );
        assert_eq!(g.profiles.iter().filter(|p| p.id == "dorm").count(), 1);
    }

    /// 导入的建议 ID 必须与 `create_profile` 的 slugify 落盘 id 完全一致
    ///
    /// 回归：曾打算另写一份"下划线"规则，则冲突探测算的是 `dorm_2`、实际落盘
    /// `dorm-2`，不一致会漏判冲突（静默覆盖或报意外 409）。用**含下划线与大写**
    /// 的 ASCII 名称才能暴露该差异——中文名 slug 后为空，走回退分支，测不到规则本身。
    #[tokio::test]
    async fn test_import_suggested_id_matches_slugify() {
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            g.profiles.iter_mut().find(|p| p.id == "dorm").unwrap().name = "My_Dorm Net".into();
        }
        let exported = export_of(&app, "dorm").await;
        // 载荷不带 suggested_id 时才走"按名称推导"分支，这里先摘掉它以测该分支
        let mut body = exported["data"].clone();
        body.as_object_mut().unwrap().remove("suggested_id");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let assigned = body_json(resp).await["data"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // 必须恰等于 create_profile 内部会写盘的那个 id（下划线归一为连字符）
        assert_eq!(
            assigned,
            crate::config::profiles::slugify_id("My_Dorm Net"),
            "导入 id 必须与 create_profile 的 slugify 结果逐字一致"
        );
        assert_eq!(assigned, "my-dorm-net");
        let g = inner.lock().unwrap();
        assert!(
            g.profiles.iter().any(|p| p.id == assigned),
            "响应的 id 必须是实际落盘的 id"
        );
    }

    /// 中文方案名导入后应得到可辨识的 id（沿用导出方的源 id），而非 imported-profile
    ///
    /// 端到端实测发现：方案名多为中文，slug 后为空会退化成 `imported-profile`，
    /// 同名分享给多个同学还会各自变成 `-2`/`-3`，接收方看到的是一串无意义编号。
    #[tokio::test]
    async fn test_import_prefers_source_id_for_chinese_names() {
        let (app, inner) = mock_app();
        seed_shareable(&inner, "dorm"); // name = "宿舍移动"（中文，slug 后为空）
        let exported = export_of(&app, "dorm").await;
        assert_eq!(exported["data"]["suggested_id"], "dorm");

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(exported["data"].to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let assigned = body_json(resp).await["data"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        // 源 id 已被占用（dorm 自身）→ 追加后缀，但仍是可辨识的 dorm-2 而非 imported-profile
        assert_eq!(
            assigned, "dorm-2",
            "中文名方案应沿用源 id 而非退化为 imported-profile"
        );
    }

    /// 恶意/异常 `suggested_id` 不得绕过 slug 与合法性校验（路径穿越等）
    #[tokio::test]
    async fn test_import_sanitizes_suggested_id() {
        let (app, inner) = mock_app();
        // 期望值：前两条路径穿越载荷都归一为 "evil"（第二条因重名追加后缀），
        // 第三条按 slug 规则得 "my-dorm"，第四条空串退回 imported-profile
        for (raw, expect) in [
            ("../../evil", "evil"),
            ("..\\..\\evil", "evil-2"),
            ("My Dorm!", "my-dorm"),
            ("", "imported-profile"),
        ] {
            let body = serde_json::json!({
                "campus_auth_profile": 1,
                "suggested_id": raw,
                "profile": { "name": "中文名", "http_url": "http://10.1.1.1/login" }
            });
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/profiles/import")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "suggested_id={raw:?}");
            let assigned = body_json(resp).await["data"]["id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(
                assigned, expect,
                "suggested_id={raw:?} 应被规范化为 {expect}"
            );
            assert!(!assigned.contains('/') && !assigned.contains('\\') && !assigned.contains('.'));
        }
        // 所有落盘 id 均为合法 profile id（无路径分隔符 / 点号 / 保留名）
        let g = inner.lock().unwrap();
        let mut count = 0;
        for p in &g.profiles {
            assert!(
                p.id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "落盘 id 非法: {}",
                p.id
            );
            count += 1;
        }
        assert_eq!(count, 2 + 4, "原有 2 个 + 4 次导入");
    }

    /// 连字符 id 必须能通过前端的保存校验字符集 `^[a-zA-Z0-9_-]+$`
    ///
    /// 回归：前端曾只允许 `_`，而落盘 id 恒为连字符形态（slugify 把 `_` 归一为 `-`），
    /// 于是任何含下划线/空格/中文名的新建方案都是"创建后再也改不动"——编辑器里
    /// ID 输入框 disabled，改名字保存会被前端拦下。导入的方案 id 同样来自该 slugify，
    /// 故这里断言两者一致，防止任一侧单独放宽或收紧。
    #[test]
    fn test_slugified_ids_satisfy_frontend_id_charset() {
        // 与 frontend/src/composables/useProfiles.ts 的 saveProfile 正则保持一致
        let frontend_ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        for name in [
            "My_Dorm Net",
            "宿舍移动",
            "dorm",
            "DORM-2",
            "a  b__c",
            "profile!",
        ] {
            let slug = crate::config::profiles::slugify_id(name);
            if slug.is_empty() {
                // 空 slug 走 imported-profile 回退，不参与本断言
                continue;
            }
            assert!(
                frontend_ok(&slug),
                "slugify({name:?}) = {slug:?} 会被前端 ID 校验拒绝"
            );
        }
    }

    /// 非本应用导出的 JSON 必须被拒绝，不做"猜字段"的宽松解析
    #[tokio::test]
    async fn test_import_rejects_foreign_payload() {
        let (app, _inner) = mock_app();
        for (label, body) in [
            ("缺标记", serde_json::json!({ "profile": { "name": "x" } })),
            ("空对象", serde_json::json!({})),
            (
                "裸 ProfileData",
                serde_json::json!({ "name": "x", "http_url": "http://a.b/c" }),
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/profiles/import")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "应拒绝: {label}");
        }
    }

    /// 更高格式版本须明确提示升级，而非按当前版本强行解析
    #[tokio::test]
    async fn test_import_rejects_future_format_version() {
        let (app, _inner) = mock_app();
        let body = serde_json::json!({
            "campus_auth_profile": 99,
            "profile": { "name": "未来方案" }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        let msg = json["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("更新版本"), "应提示来源版本更新: {msg}");
    }

    /// 非法枚举值须报错，而非静默退回默认（否则"导入成功但渠道被改"）。
    /// 直连请求参数整体搬进任务后，方案侧只剩 `login_channel` 一个枚举。
    #[tokio::test]
    async fn test_import_rejects_invalid_login_channel() {
        let (app, _inner) = mock_app();
        let body = serde_json::json!({
            "campus_auth_profile": 1,
            "profile": { "name": "枚举方案", "login_channel": "curl" }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法渠道应被拒");
    }

    /// 无名称的方案不可导入（无法命名也就无法提示用户）
    #[tokio::test]
    async fn test_import_requires_name() {
        let (app, _inner) = mock_app();
        let body = serde_json::json!({
            "campus_auth_profile": 1,
            "profile": { "name": "   " }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 带 `{ data: ... }` 外壳的载荷同样可导入（兼容 API 信封写法）
    #[tokio::test]
    async fn test_import_accepts_envelope_wrapping() {
        let (app, _inner) = mock_app();
        let body = serde_json::json!({
            "data": {
                "campus_auth_profile": 1,
                "profile": { "name": "信封方案", "login_channel": "http" }
            }
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/import")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
