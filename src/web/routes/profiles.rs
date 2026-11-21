//! Profile 路由：认证档案 CRUD + 切换 + 自动检测
//!
//! M1 细粒度 state（profiles 域）：handler 声明 `State<Arc<dyn ProfileApi>>` /
//! `State<Arc<dyn ConfigApi>>` 依赖（经 AppState 的 FromRef 委派提取），
//! 不再触达 `state.container`，测试可注入内存实现（见模块测试）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use zeroize::Zeroizing;

use crate::bridge::BridgeApi;
use crate::config::{ConfigApi, LoginChannel, ProfileApi, ProfileData};
use crate::engine::{EngineApi, EngineCommand, ProfileSwitchSource};
use crate::tasks::TaskApi;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

// ---- 契约 DTO（T5-1a profiles 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`：成功响应一律
// `body = DataEnvelope<XxxData>`，非 2xx 一律 `body = ErrorResponse`。
// `ProfileSummaryData` / `ProfileShareData` 是对 `crate::config` 侧序列化
// 形状的**镜像声明**（handler 仍走 `data(json!({...}))` 组装），后续强类型化
// 批次再让 handler 直接返回这些 DTO；`ProfileDetailData.settings` 暂以
// `Value` 标注（整体为 `ProfileData` 序列化，形状随 `src/config/schema.rs`
// 演进，待强类型化批次收敛）。

/// GET /api/profiles 列表项（镜像 `crate::config::ProfileSummary` 的线格式）
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileSummaryData {
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
    /// 直连渠道绑定的直连任务 ID（空 = 未绑定）
    pub active_http_task: String,
    /// 脚本渠道绑定的脚本任务 ID（空 = 未绑定）
    pub active_script_task: String,
    /// 登录执行渠道（browser=浏览器自动化 / http=直连请求 / script=自定义脚本）
    pub login_channel: String,
    /// 网关 IP 匹配规则（空 = 无匹配规则）
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则（空 = 无匹配规则）
    pub wifi_ssid: String,
}

/// GET /api/profiles 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileListData {
    /// 全部方案摘要（不含密码字段），键为 Profile ID
    pub profiles: std::collections::HashMap<String, ProfileSummaryData>,
    /// 活跃方案 ID
    pub active_profile: String,
    /// 是否启用按网关/SSID 自动切换
    pub auto_switch: bool,
}

/// GET /api/profiles/{id} 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDetailData {
    /// 方案详情：`ProfileData` 序列化（password 恒为空串——响应前已清空，
    /// 不回传密文；network 为运行时 effective 出口）。形状随
    /// `crate::config::ProfileData` 演进，待强类型化批次收敛
    #[schema(value_type = Object)]
    pub settings: Value,
    /// 密码是否可用（口径同 GET /api/config：能解密而非仅字段非空）
    pub has_password: bool,
}

/// 方案分享载荷中的 profile 部分（`GET /api/profiles/{id}/export` 导出格式）
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileShareProfileData {
    /// 显示名称
    pub name: String,
    /// 恒为空串：导出剔除账号（接收方填自己的账号）
    pub username: String,
    /// 恒为空串：导出剔除密码（密文跨机器不可解，原样带出会被接收方二次加密）
    pub password: String,
    /// 固定登录网址
    pub auth_url: String,
    /// 自定义重定向触发地址
    pub trigger_url: String,
    /// 运营商
    pub isp: String,
    /// 网关 IP 匹配规则
    pub gateway_ip: String,
    /// WiFi SSID 匹配规则
    pub wifi_ssid: String,
    /// 恒为空串：任务绑定属本机资源，不随方案迁移
    pub active_task: String,
    /// 登录执行渠道（browser/http/script；渠道本身是方案属性，随方案走）
    pub login_channel: String,
    /// 恒为空串：直连任务绑定不随方案迁移
    pub active_http_task: String,
    /// 恒为空串：脚本任务绑定不随方案迁移
    pub active_script_task: String,
}

/// GET /api/profiles/{id}/export 响应载荷（分享文件信封）
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileShareData {
    /// 分享格式版本号（当前 1；导入方遇到更高版本提示升级）
    pub campus_auth_profile: u32,
    /// 导出时间（UTC RFC3339）
    pub exported_at: String,
    /// 导出方应用版本
    pub app_version: String,
    /// 建议的导入 ID（取源方案 id；仅建议，被占用时自动追加后缀）
    pub suggested_id: String,
    /// 方案内容（已剔除账号与密码、任务绑定置空）
    pub profile: ProfileShareProfileData,
}

/// POST /api/profiles/import 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileImportData {
    /// 实际落盘的方案 ID（与既有方案冲突时自动追加 -2/-3 后缀）
    pub id: String,
    /// 分享文件是否携带 v9 内联直连配置（已忽略；前端据此提示到任务页重配直连任务）
    pub legacy_http_config_dropped: bool,
}

/// POST /api/profiles/detect 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDetectData {
    /// 检测到的网关 IP（未检测到时为 null）
    pub gateway_ip: Option<String>,
    /// 检测到的 WiFi SSID（未检测到时为 null）
    pub ssid: Option<String>,
    /// 匹配到的方案 ID（无匹配时为 null）
    pub matched_profile_id: Option<String>,
    /// 匹配到的方案名称（无匹配时为 null）
    pub matched_profile_name: Option<String>,
}

/// POST /api/profiles/auto-switch 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoSwitchData {
    /// 当前活跃方案 ID
    pub active_profile: String,
    /// 面向用户的提示消息
    pub message: String,
}

/// POST /api/profiles/{id}/config-session/start 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfigSessionStartData {
    /// 配置浏览器是否已拉起且录制已开启（成功响应恒为 true）
    pub started: bool,
}

/// `crate::config::schema::RecordedAction` 的线格式镜像声明（与 Worker 录制契约
/// 同形）：config 持久化树走 schemars::JsonSchema 体系、与 utoipa ToSchema 互不
/// 复用（见 `Cargo.toml` 对 schemars 的用途注记），profiles 域契约用本镜像标注
/// 响应形状。字段须与源类型逐字对齐（含 `type` 改名与 `value` 的省略语义）
#[derive(Debug, Serialize, ToSchema)]
pub struct RecordedActionData {
    /// 动作类型：`click`（按钮/提交类点击）、`change`（`<select>` 选择）、
    /// `fill`（输入框填写，值可为凭据占位符）或 `navigate`（页面导航）
    #[serde(rename = "type")]
    pub action_type: String,
    /// 目标元素选择器（Worker 按启发式优先级取首个命中）
    pub selector: String,
    /// 选择器采用的启发式级别（与 `selector` 配对）
    pub strategy: String,
    /// 事件时间戳（毫秒；Worker 侧按此升序返回并做 500ms 去重）
    pub ts: i64,
    /// `change`/`fill`/`navigate` 动作的值（`fill` 可为 `{{USERNAME}}`/`{{PASSWORD}}`
    /// 占位符、`navigate` 为落地页 URL）；仅 `click` 无此键（序列化时跳过，与
    /// Worker 返回形状一致）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// 非顶层 frame 内捕获的动作：主文档到目标 iframe 的 CSS 选择器链，
    /// ' >> ' 分隔；顶层捕获缺此键
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
}

/// POST /api/profiles/{id}/config-session/stop 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfigSessionStopData {
    /// 本次会话录制的动作数（Worker 侧截断后的真实数量；discard 时仅回显不落盘）
    pub recorded_count: u64,
    /// 动作数超出 Worker 单次上限（50 条）被截断
    pub truncated: bool,
    /// 方案的持久会话开关：apply=true（缺省）且非 discard 恒为 true（写回后的值），
    /// 其余情况回方案当前值（未写回，不虚报）
    pub persistent_session_enabled: bool,
    /// 本次会话录制的动作数组（informational：无论 discard 与否、apply 与否，
    /// Worker 回包携带即返回——已过凭据等值归一，值与方案凭据恒等时替换为
    /// 占位符；Worker 侧已有单次 50 条截断守卫，量级可控）。
    /// 任务录制流程经 `apply=false` 消费本字段，前端不必再发一次 GET 方案详情
    pub recorded_actions: Vec<RecordedActionData>,
}

/// POST /api/profiles/{id}/config-session/start 查询参数
#[derive(Deserialize, ToSchema)]
pub struct ConfigSessionStartQuery {
    /// `true`（缺省）为录制配置会话（注入录制器，stop 可落盘录制）；
    /// `false` 为**自由浏览**模式：同样拉起 headed 持久浏览器并导航到门户地址，
    /// 但不注入录制器——供已启用持久会话的方案单独打开浏览器手动登录（勾选
    /// 门户「记住密码」），用完直接关窗即结束（幽灵收尾口径与录制会话一致）
    #[serde(default = "true_default")]
    pub record: bool,
}

/// `serde(default)` 需要的 `true` 常量函数（保持缺省录制口径）
fn true_default() -> bool {
    true
}

/// POST /api/profiles/{id} 请求体：创建 Profile（可选匹配/认证字段与 PUT 同语义，创建即完整落盘）
#[derive(Deserialize, ToSchema)]
pub struct ProfileCreateBody {
    /// 本方案统一网络出口。形状：`{mode: direct|system|proxy|interface,
    /// interface_id, proxy_url, dns_servers[]}`（整体结构见
    /// `crate::network::settings::NetworkSettings`，待强类型化批次收敛）
    #[schema(value_type = Object)]
    pub network: Option<crate::network::settings::NetworkSettings>,
    /// 与路径参数冗余的历史字段：路径已携带 id，body 内可省略（路径优先）
    pub id: Option<String>,
    pub name: String,
    pub username: String,
    /// 登录密码明文；空串 = 不设独立密码（服务端加密为 `ENC:` 密文存储，响应不回传）
    #[schema(value_type = String)]
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
    /// 登录执行渠道：browser（默认）/ http（直连）/ script（自定义脚本）
    #[schema(value_type = String)]
    pub login_channel: Option<LoginChannel>,
    /// 直连渠道绑定的直连任务 ID（空 = 未绑定，直连登录不可用）
    pub active_http_task: Option<String>,
    /// 脚本渠道绑定的脚本任务 ID（空 = 未绑定，脚本登录不可用）
    pub active_script_task: Option<String>,
    /// 是否启用持久浏览器会话（门户「记住密码」模式）；缺省不启用。
    /// 录制动作（recorded_actions）不进创建/更新通道：它只由配置会话
    /// stop 端点写回，本字段只承载开关
    pub persistent_session_enabled: Option<bool>,
}

/// 所有保存路径共用的出口校验；任意脚本无法由本程序强制绑定网卡。
pub(crate) fn validate_profile_network(profile: &ProfileData) -> Result<(), String> {
    if let Some(network) = &profile.network {
        network.validate()?;
        if profile.login_channel == LoginChannel::Script
            && network.mode == crate::network::settings::NetworkMode::Interface
        {
            return Err(
                "指定网卡出口适用于浏览器或 HTTP 登录，任意脚本不能保证绑定，请更换登录渠道".into(),
            );
        }
    }
    Ok(())
}

/// 校验 http/https URL 并返回 trim 结果（认证地址/重定向触发地址共用；空串直通）
///
/// scheme + 主机名两级判定已上提至 `crate::web::validate`（与 config 域同一
/// 实现），本包装只保留 profiles 域特有的 trim 与空串直通语义。
fn validate_http_url(label: &str, raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Ok(trimmed);
    }
    crate::web::validate::validate_http_url(label, &trimmed)?;
    Ok(trimmed)
}

/// 校验方案的「渠道 → 任务」绑定（新建 / 更新 / `PATCH /api/config` 三条保存路径共用）。
///
/// 为什么直连与脚本比浏览器渠道多出这一道：浏览器渠道有内置默认任务兜底，绑定为空或
/// 失效时登录会回退到 `default`，最坏也只是"用了默认任务"；直连与脚本都**没有**兜底
/// （门户地址无法内置，登录逻辑只能由用户自己写），绑定为空或指错任务时登录必然失败
/// ——而那一刻用户早已离开保存页面，看到的是"登录失败"而非"配置没选对"。放过去等于把
/// 配置错误伪装成运行错误，故必须在保存时就拦下。
///
/// 只读一次任务并同时判「存在」与「类型」：仅查存在性会放行被改成别类的同名任务，
/// 与 `LoginOrchestrator::resolve_http_task` / `resolve_script_task` 的判定口径保持一致。
pub(crate) async fn validate_login_task_binding(
    tasks: &Arc<dyn TaskApi>,
    login_channel: LoginChannel,
    active_http_task: &str,
    active_script_task: &str,
) -> Result<(), ApiError> {
    if !login_channel.is_supported() {
        return Err(ApiError::BadRequest(
            "OpenWrt 版不支持浏览器登录，请选择 HTTP 登录或自定义脚本".into(),
        ));
    }
    // 浏览器渠道不看这两个字段：切回浏览器后残留的绑定无害，不强制用户清空
    let (id, field, kind, want_http) = match login_channel {
        LoginChannel::Browser => return Ok(()),
        LoginChannel::Http => (
            active_http_task.trim(),
            "active_http_task",
            "直连任务",
            true,
        ),
        LoginChannel::Script => (
            active_script_task.trim(),
            "active_script_task",
            "脚本任务",
            false,
        ),
    };
    if id.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "请为{}渠道选择一个{kind}",
            if want_http { "直连" } else { "脚本" }
        )));
    }
    let type_matches = |task: &crate::tasks::TaskKind| {
        if want_http {
            matches!(task, crate::tasks::TaskKind::Http(_))
        } else {
            matches!(task, crate::tasks::TaskKind::Script(_))
        }
    };
    match tasks.load_task(id).await {
        Ok(task) if type_matches(&task) => Ok(()),
        Ok(other) => {
            tracing::warn!(
                task_id = id,
                task_type = other.type_name(),
                field,
                "方案绑定的任务类型不符，保存被拒"
            );
            Err(ApiError::BadRequest(format!(
                "{kind} {id} 不存在或不是{kind}"
            )))
        }
        Err(e) => {
            tracing::warn!(task_id = id, field, error = %e, "方案绑定的任务加载失败，保存被拒");
            Err(ApiError::BadRequest(format!(
                "{kind} {id} 不存在或不是{kind}"
            )))
        }
    }
}

/// PUT /api/profiles/{id} 请求体：字段全可选，仅覆盖出现的字段（空密码 = 保留原密码）
#[derive(Deserialize, ToSchema)]
pub struct ProfileUpdateBody {
    /// 本方案统一网络出口。形状：`{mode: direct|system|proxy|interface,
    /// interface_id, proxy_url, dns_servers[]}`（整体结构见
    /// `crate::network::settings::NetworkSettings`，待强类型化批次收敛）
    #[schema(value_type = Object)]
    pub network: Option<crate::network::settings::NetworkSettings>,
    pub name: Option<String>,
    pub username: Option<String>,
    /// 登录密码明文；空串 = 未修改（保留原密码），非空由服务端加密为 `ENC:` 密文
    #[schema(value_type = String)]
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
    /// 登录执行渠道：browser（默认）/ http（直连）/ script（自定义脚本）
    #[schema(value_type = String)]
    pub login_channel: Option<LoginChannel>,
    /// 直连渠道绑定的直连任务 ID（空 = 未绑定，直连登录不可用）
    pub active_http_task: Option<String>,
    /// 脚本渠道绑定的脚本任务 ID（空 = 未绑定，脚本登录不可用）
    pub active_script_task: Option<String>,
    /// 是否启用持久浏览器会话（门户「记住密码」模式）；仅显式出现时覆盖。
    ///
    /// **录制动作（recorded_actions）刻意不进本请求体**：它是配置会话
    /// stop 端点写回的编排产物，不是用户手编字段。serde 对未知字段静默
    /// 忽略（无 `deny_unknown_fields`），前端即使把编辑器全文回传也碰不到
    /// 它（守护测试钉住该行为）
    pub persistent_session_enabled: Option<bool>,
}

/// POST /api/profiles/switch 请求体：要切换到的目标 Profile ID
#[derive(Deserialize, ToSchema)]
pub struct SwitchBody {
    /// 目标 Profile ID（须已存在）
    pub profile_id: String,
}

/// POST /api/profiles/auto-switch 请求体：启用/禁用自动切换
#[derive(Deserialize, ToSchema)]
pub struct AutoSwitchBody {
    /// 是否启用按网关/SSID 自动切换活跃方案
    pub enabled: bool,
}

/// GET /api/profiles — 列出全部 Profile
#[utoipa::path(
    get,
    path = "/api/profiles",
    operation_id = "get_api_profiles",
    tag = "profiles",
    responses(
        (status = 200, description = "全部方案摘要（不含密码）+ 活跃方案与自动切换开关", body = DataEnvelope<ProfileListData>),
    )
)]
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
///
/// 契约注解：`settings` 为 `ProfileData` 整体序列化，暂以 `Value` 标注
/// （待强类型化批次收敛）；其中 password 恒为空串（响应前清空，不回传密文）。
#[utoipa::path(
    get,
    path = "/api/profiles/{id}",
    operation_id = "get_api_profiles_id",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
    ),
    responses(
        (status = 200, description = "方案详情（settings.password 恒为空串，network 为运行时 effective 出口）+ has_password", body = DataEnvelope<ProfileDetailData>),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 500, description = "方案读取或序列化失败", body = ErrorResponse),
    )
)]
pub async fn get_profile(
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = config.load_profile(&id)?;
    let has_password =
        crate::web::routes::config::effective_has_password(config.as_ref(), &profile);
    // 避免将密码（密文）泄露给前端
    profile.password = String::new();
    profile.network = Some(crate::network::settings::NetworkSettings::effective(
        &profile,
        &config.load_settings_async().await.global,
    ));
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
/// `active_task` / `active_http_task` / `active_script_task` 置空：三者都是"本机任务
/// ID"的绑定关系，接收方通常没有同名任务。浏览器侧保留会静默回退到默认任务
/// （`LoginOrchestrator::resolve_active_task`），直连与脚本侧保留则会直接指向一个不
/// 存在的任务——两种结果都在对方的机器上表现为"配置看起来是好的但登录不对"。
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
            // 脚本任务同理：登录脚本的正文在 `tasks/scripts/<id>.json`，不随方案走
            "active_script_task": "",
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
            ApiError::BadRequest("分享文件的 login_channel 非法（仅 browser/http/script）".into())
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
#[utoipa::path(
    get,
    path = "/api/profiles/{id}/export",
    operation_id = "get_api_profiles_id_export",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
    ),
    responses(
        (status = 200, description = "分享文件信封（username/password 恒为空串，任务绑定置空；可直接作为 POST /api/profiles/import 的请求体）", body = DataEnvelope<ProfileShareData>),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 500, description = "方案读取失败", body = ErrorResponse),
    )
)]
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
///
/// 契约注解：请求体为宽松 `Value`——格式仅认「本应用导出的信封」（须含
/// `campus_auth_profile` 版本标记），兼容 `{ data: {...} }` 外壳写法，
/// 载荷内的账号/密码字段一律忽略，故不声明强类型请求体。
#[utoipa::path(
    post,
    path = "/api/profiles/import",
    operation_id = "post_api_profiles_import",
    tag = "profiles",
    request_body(content = Value, description = "方案分享文件（GET /api/profiles/{id}/export 的导出格式；须含 campus_auth_profile 版本标记，载荷中的账号与密码字段被忽略）"),
    responses(
        (status = 200, description = "导入结果（id 冲突时自动追加 -2/-3 后缀，绝不覆盖既有方案）", body = DataEnvelope<ProfileImportData>),
        (status = 400, description = "非本应用导出的分享文件（缺版本标记 / 格式更新 / 方案名缺失 / URL 或渠道值非法）", body = ErrorResponse),
        (status = 409, description = "同名方案过多（自动后缀超过 10000 个）", body = ErrorResponse),
        (status = 500, description = "方案写入失败", body = ErrorResponse),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/profiles/{id}",
    operation_id = "post_api_profiles_id",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID（路径优先于 body.id；两者皆空则 400）"),
    ),
    request_body(content = ProfileCreateBody, description = "创建载荷：name/username/password 必填，其余可选字段与 PUT 同语义，创建即完整落盘"),
    responses(
        (status = 200, description = "创建成功（纯新建语义：命中既有 id 由 Service 层拒绝为 409）", body = DataEnvelope<String>),
        (status = 400, description = "缺少 id、URL 格式非法、网络出口无效、或渠道任务绑定校验失败", body = ErrorResponse),
        (status = 409, description = "Profile ID 已存在", body = ErrorResponse),
        (status = 500, description = "密码加密或方案写入失败", body = ErrorResponse),
    )
)]
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
    if let Some(active_script_task) = body.active_script_task {
        profile.active_script_task = active_script_task;
    }
    if let Some(persistent_session_enabled) = body.persistent_session_enabled {
        profile.persistent_session_enabled = persistent_session_enabled;
    }
    if let Some(network) = body.network {
        network.validate().map_err(ApiError::BadRequest)?;
        profile.network = Some(network);
    }
    validate_profile_network(&profile).map_err(ApiError::BadRequest)?;
    // 直连 / 脚本渠道必须有可用的任务绑定（见 validate_login_task_binding 的说明）
    validate_login_task_binding(
        &tasks,
        profile.login_channel,
        &profile.active_http_task,
        &profile.active_script_task,
    )
    .await?;
    profiles.create_profile(&target_id, profile).await?;
    tracing::info!(profile_id = %target_id, "创建 Profile");
    Ok(data(Value::String("ok".into())))
}

/// PUT /api/profiles/{id} — 更新 Profile
#[utoipa::path(
    put,
    path = "/api/profiles/{id}",
    operation_id = "put_api_profiles_id",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
    ),
    request_body(content = ProfileUpdateBody, description = "更新载荷：仅覆盖出现的字段；password 空串 = 保留原密码，clear_password = true 显式清除（优先于 password）"),
    responses(
        (status = 200, description = "更新成功（合并/校验在 profiles_lock 事务临界区内完成）", body = DataEnvelope<String>),
        (status = 400, description = "字段校验失败（URL 非法、网络出口无效、渠道任务绑定缺失等）", body = ErrorResponse),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 500, description = "密码加密或方案写入失败", body = ErrorResponse),
    )
)]
pub async fn update_profile(
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(id): Path<String>,
    Json(body): Json<ProfileUpdateBody>,
) -> Result<Json<Value>, ApiError> {
    // 密码加密在事务外完成（纯函数，不依赖锁内状态）：事务闭包的错误只来自
    // 校验（400），加密失败保持 500 语义。空串契约是“未修改，保留原密码”
    // （GET 会清空密码，前端编辑后保存会回传 ""），不能加密成合法 ENC: 覆盖原密码。
    let encrypted_password = match body.password.as_deref() {
        Some(p) if !p.is_empty() => Some(
            config
                .encrypt_password(p)
                .map_err(|e| ApiError::Internal(format!("密码加密失败: {e}")))?,
        ),
        _ => None,
    };
    let clear_password = body.clear_password;
    let tasks = tasks.clone();
    // F7 Profile 侧事务：合并/校验在 profiles_lock 临界区内完成——并发保存
    // 同一 Profile 不再互相覆盖丢字段、清除密码不会被并发的「保留原密码」
    // 交错复活（旧实现锁外 load→改→save 存在丢失更新窗口）
    let result = profiles
        .update_profile_tx(
            &id,
            Box::new(move |mut profile| {
                let tasks = tasks.clone();
                Box::pin(async move {
                    // 密码语义（与 ProfileService::update_profile 同口径）：
                    // clear_password 优先 → 显式清除；非空新密码 → 已加密密文直写；
                    // 其余（None / 空串 = 未修改）→ 保留锁内读到的既有密文
                    if clear_password {
                        profile.password = String::new();
                    } else if let Some(enc) = encrypted_password {
                        profile.password = enc;
                    }
                    if let Some(name) = body.name {
                        profile.name = name;
                    }
                    if let Some(username) = body.username {
                        profile.username = username;
                    }
                    if let Some(auth_url) = body.auth_url {
                        profile.auth_url =
                            validate_http_url("认证地址", &auth_url).map_err(|e| e.to_string())?;
                    }
                    if let Some(trigger_url) = body.trigger_url {
                        profile.trigger_url = validate_http_url("重定向触发地址", &trigger_url)
                            .map_err(|e| e.to_string())?;
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
                    if let Some(active_script_task) = body.active_script_task {
                        profile.active_script_task = active_script_task;
                    }
                    if let Some(persistent_session_enabled) = body.persistent_session_enabled {
                        profile.persistent_session_enabled = persistent_session_enabled;
                    }
                    if let Some(network) = body.network {
                        network.validate()?;
                        profile.network = Some(network);
                    }
                    validate_profile_network(&profile)?;
                    // 直连 / 脚本渠道必须有可用的任务绑定（与 create 同一口径，见
                    // validate_login_task_binding）。注意这里判的是**合并后**的渠道与绑定：只改渠道
                    // 不改绑定（或反之）时也必须整体有效。校验在事务临界区内完成，
                    // 避免校验通过后、落盘前被并发保存改掉绑定。
                    validate_login_task_binding(
                        &tasks,
                        profile.login_channel,
                        &profile.active_http_task,
                        &profile.active_script_task,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    Ok(profile)
                })
            }),
        )
        .await?;
    match result {
        Ok(_) => {
            tracing::info!(profile_id = %id, "更新 Profile");
            Ok(data(Value::String("ok".into())))
        }
        Err(msg) => Err(ApiError::BadRequest(msg)),
    }
}

/// DELETE /api/profiles/{id} — 删除 Profile
#[utoipa::path(
    delete,
    path = "/api/profiles/{id}",
    operation_id = "delete_api_profiles_id",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
    ),
    responses(
        (status = 200, description = "删除成功", body = DataEnvelope<String>),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 409, description = "default 方案不可删除", body = ErrorResponse),
        (status = 500, description = "方案删除失败", body = ErrorResponse),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/profiles/switch",
    operation_id = "post_api_profiles_switch",
    tag = "profiles",
    request_body(content = SwitchBody, description = "要切换到的目标 Profile ID"),
    responses(
        (status = 200, description = "切换已持久化（Engine 派生状态同步失败仅告警，经 reload 收敛）", body = DataEnvelope<String>),
        (status = 404, description = "目标方案不存在", body = ErrorResponse),
        (status = 500, description = "切换落盘失败", body = ErrorResponse),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/profiles/detect",
    operation_id = "post_api_profiles_detect",
    tag = "profiles",
    responses(
        (status = 200, description = "网关 IP / SSID 检测结果与匹配到的方案（无匹配时对应字段为 null）", body = DataEnvelope<ProfileDetectData>),
        (status = 500, description = "网关或 SSID 检测失败", body = ErrorResponse),
    )
)]
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
#[utoipa::path(
    post,
    path = "/api/profiles/auto-switch",
    operation_id = "post_api_profiles_auto_switch",
    tag = "profiles",
    request_body(content = AutoSwitchBody, description = "启用/禁用按网关/SSID 自动切换活跃方案"),
    responses(
        (status = 200, description = "开关已落库并回显当前活跃方案", body = DataEnvelope<AutoSwitchData>),
        (status = 500, description = "设置落盘失败", body = ErrorResponse),
    )
)]
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

/// POST /api/profiles/{id}/config-session/start — 启动持久配置会话（无请求体）
///
/// 编排口径与登录编排的持久自动登录装配同源：
/// - `user_data_dir`：按方案 id 拼接的持久目录（browser_profiles/<id>/），
///   派发前 ensure 创建（持久目录按需创建口径）；
/// - `portal_url`：方案浏览器导航有效地址（认证地址回退链，留空时落触发
///   地址，恒非空）；
/// - `browser_settings`：整体下发运行时浏览器设置（资源路由 / 直连 / stealth
///   口径对齐 Worker 启动逻辑）。
///
/// 两种模式（`?record=` 查询参数，缺省 `true`）：
/// - **录制配置会话**（缺省）：headed 持久浏览器 + 注入录制器，stop 结算时
///   录制动作与开关写回方案；
/// - **自由浏览**（`record=false`）：同样 headed 持久浏览器并导航到门户地址，
///   但不注入录制器——供已启用持久会话的方案单独「打开浏览器」手动登录
///   （勾选门户「记住密码」），用完直接关窗即结束（Worker 幽灵收尾，
///   user_data_dir 原样保留）。该模式要求方案已启用 `persistent_session_enabled`
///   （400 拦截），stop 端点拒绝其非 discard 结算（无录制可落盘）。
///
/// 占用互斥：Worker 侧同一时刻仅允许一个活跃配置会话（重复 start 回结构化
/// 错误）；Rust 侧会话矩阵把 `profile_session_start/stop` 归为持久类命令，
/// 在途登录 / 调试 / 重定向检测期间快速失败（409）。
///
/// 归属记录：start 成功后把 (session_id, profile_id, record) 记入 Bridge 的
/// 归属注册表——Worker 不感知方案 id，stop 端点依赖该记录拒绝「把 A 方案
/// 会话的录制写进 B 方案」与「未录制会话的落盘结算」。
///
/// 超时回执时延：命令预算（[`crate::bridge::CONFIG_SESSION_START_TIMEOUT`]）
/// 耗尽后可能附加至多 ~10s 槽位宽限等待（[`crate::bridge::GRACE_WAIT_DURATION`]，
/// 见该常量处说明）——超时判定成立且槽位由本请求持有时，Bridge 先等 Worker
/// 自愈释放槽位再返回超时。
#[utoipa::path(
    post,
    path = "/api/profiles/{id}/config-session/start",
    operation_id = "post_api_profiles_id_config_session_start",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
        ("record" = bool, Query, description = "false 时以自由浏览模式打开持久浏览器（不注入录制器），须先启用持久会话；缺省 true 为录制配置会话"),
    ),
    responses(
        (status = 200, description = "配置浏览器已拉起（录制模式录制已开启；自由浏览模式不录制；会话存续期不受命令级超时约束，直到 stop）", body = DataEnvelope<ConfigSessionStartData>),
        (status = 400, description = "非浏览器渠道（持久会话仅浏览器渠道支持），或自由浏览模式要求方案已启用持久会话", body = ErrorResponse),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 409, description = "已有活跃配置会话（Worker 占用互斥）或会话槽位忙（在途登录/调试/重定向检测）", body = ErrorResponse),
        (status = 500, description = "持久会话目录创建失败或命令超时等内部错误", body = ErrorResponse),
        (status = 503, description = "Worker 未安装，或配置浏览器/门户页启动失败（环境不可用）", body = ErrorResponse),
    )
)]
pub async fn start_config_session(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
    Query(query): Query<ConfigSessionStartQuery>,
) -> Result<Json<Value>, ApiError> {
    let profile = config.load_profile(&id)?;
    // 渠道校验：持久会话依赖门户「记住密码」（cookie/localStorage 持久化），
    // 直连与脚本渠道没有浏览器会话可录
    if profile.login_channel != LoginChannel::Browser {
        return Err(ApiError::BadRequest(
            "持久会话仅浏览器渠道支持，请先将本方案的登录渠道切换为浏览器".into(),
        ));
    }
    // 自由浏览模式门禁：该模式的意义就是「用已启用的持久浏览器」，未启用时
    // 拒绝（录制模式不受此限——它是把开关打开的引导路径）
    if !query.record && !profile.persistent_session_enabled {
        return Err(ApiError::BadRequest(
            "请先启用本方案的持久会话（勾选「启用持久会话」并保存）后再打开浏览器".into(),
        ));
    }
    // portal_url 与 browser_settings 取运行时口径（含认证地址回退链与全局浏览器设置）
    let rt = config.runtime_config_for_profile(&id)?;
    let user_data_dir = crate::utils::paths::browser_profile_dir(&config.base_path(), &id);
    // 持久目录由编排侧 ensure（登录编排同口径）：目录建不出来时 Worker 启动
    // 持久上下文必然失败，早失败早提示
    std::fs::create_dir_all(&user_data_dir).map_err(|e| {
        ApiError::Internal(format!(
            "创建持久会话目录失败（{}）: {e}",
            user_data_dir.display()
        ))
    })?;
    let params = serde_json::json!({
        "user_data_dir": user_data_dir,
        "portal_url": rt.profile.effective_browser_login_url(),
        "record": query.record,
        "browser_settings": rt.browser,
    });
    let resp = bridge.execute_config_session_start(params).await?;
    // Worker 结构化失败（success=false）：占用互斥命中（重复启动）归 409，
    // 其余（浏览器/门户环境故障）归 503。占用文案钉在 python_worker 的
    // profile_session_start 契约上（「已存在活跃配置会话，请先停止当前配置会话」）
    if !resp.result.success {
        let msg = resp
            .result
            .error
            .unwrap_or_else(|| "配置会话启动失败".into());
        if msg.contains("已存在活跃配置会话") {
            return Err(ApiError::Conflict(msg));
        }
        return Err(ApiError::ServiceUnavailable(msg));
    }
    // 记录归属（覆盖旧记录）：Worker 单会话语义下新 start 即旧会话终结
    let session_id = resp
        .result
        .data
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    bridge.set_config_session_owner(crate::bridge::ConfigSessionOwner {
        session_id,
        profile_id: id.clone(),
        record: query.record,
    });
    tracing::info!(
        profile_id = %id,
        record = query.record,
        "配置会话已启动（headed 持久浏览器）"
    );
    Ok(data(serde_json::json!({ "started": true })))
}

/// POST /api/profiles/{id}/config-session/stop 查询参数
#[derive(Deserialize, ToSchema)]
pub struct ConfigSessionStopQuery {
    /// 丢弃本次录制：仅关闭浏览器，**不落盘录制、不写方案**（A47 对齐决策：
    /// 「只退出不保存」的显式出口）。录制内容仍回显真实计数与动作数组（信息无害）
    #[serde(default)]
    pub discard: bool,
    /// 结算写盘开关（缺省 true）：非 discard 时录制动作写回方案 +
    /// `persistent_session_enabled=true`（方案编辑器「完成配置」路径）；
    /// `false` 仅结算会话（关浏览器、取回动作）**不写方案**——任务录制流程
    /// 专用：录制生成浏览器任务不该顺带把方案翻成持久会话。apply=false 时
    /// 「未录制会话拒绝非 discard」守卫放行（本就不写方案，无空录制覆盖面）
    #[serde(default)]
    pub apply: Option<bool>,
}

/// 录制值等值归一：fill/change 值与方案凭据恒等时替换为对应占位符
///
/// P1-1b 结构性后备防线：Worker 侧启发式（`fillValueFor`）对非 `type=password`
/// 密码框可能录下明文（如点门户「显示密码」把 type 切成 `text`、自绘密码框
/// `type=text`），单靠前端启发式兜不住——本归一在 stop 结算处用方案快照明文
/// 凭据对 fill/change 值做恒等比对，命中（原文或 URL 编码形态）一律替换
/// `{{USERNAME}}`/`{{PASSWORD}}`。空凭据跳过（无可比对面）；占位符字面值不与
/// 真实凭据恒等、原样透传；`click`/`navigate` 值不参与比对（navigate 恒为
/// 落地页 URL）。URL 编码口径复用 `login::http_login::collect_secrets` 的
/// `form_urlencoded::byte_serialize` 手段，不另造轮子
fn normalize_recorded_credential_values(
    actions: &mut [crate::config::schema::RecordedAction],
    username: &str,
    password: &str,
) {
    for action in actions.iter_mut() {
        // 仅 fill/change 携带用户输入值；navigate 是页面导航 URL，click 无值
        if !matches!(action.action_type.as_str(), "fill" | "change") {
            continue;
        }
        let Some(value) = action.value.as_deref() else {
            continue;
        };
        if !username.is_empty() && value_matches_credential(value, username) {
            action.value = Some("{{USERNAME}}".to_string());
        } else if !password.is_empty() && value_matches_credential(value, password) {
            action.value = Some("{{PASSWORD}}".to_string());
        }
    }
}

/// 值与凭据恒等判定：原文或 URL 编码（`form_urlencoded::byte_serialize`）形态
fn value_matches_credential(value: &str, credential: &str) -> bool {
    if value == credential {
        return true;
    }
    value == url::form_urlencoded::byte_serialize(credential.as_bytes()).collect::<String>()
}

/// POST /api/profiles/{id}/config-session/stop — 停止配置会话并结算录制
///
/// 非 discard 且 apply=true（缺省）：解析 Worker 回传的 `recorded_actions`，经
/// 方案更新事务（与 PUT 同一 profiles_lock 临界区）一次性写回
/// `recorded_actions + persistent_session_enabled=true`——事务化是 A47 对齐项：
/// 并发保存同一方案不丢字段，录制不被「先 load 后 save」的陈旧快照覆盖。
///
/// 非 discard 且 apply=false：仅结算会话（关浏览器、取回动作）**不写方案**——
/// 任务录制流程专用：前端要的是动作数组去生成浏览器任务，不需要「录制任务」
/// 顺带把方案改成持久会话。响应里 `persistent_session_enabled` 回方案当前值
/// （与 discard 的回显口径一致）。
///
/// discard：仅关闭浏览器取回计数与动作数组，方案保持原样（录制不落盘、开关不动）。
///
/// 未录制会话（自由浏览模式打开，`owner.record == false`）拒绝 apply=true（缺省）
/// 且非 discard 的结算（400）：其回包 `recorded_actions` 恒为空数组，照单落盘会以
/// 空数组**覆盖方案既有录制**——守卫钉死这条数据丢失路径。apply=false 不写方案、
/// 无覆盖面，放行（取回的同样是空数组）。
///
/// 响应 `recorded_actions` 数组为 informational：无论 discard 与否、apply 与否，
/// Worker 回包携带即返回——结算处先经凭据等值归一（fill/change 值与方案凭据
/// 恒等时替换为 `{{USERNAME}}`/`{{PASSWORD}}`，见
/// [`normalize_recorded_credential_values`]），落盘与回显同一次覆盖。
///
/// 超时回执时延：命令预算（[`crate::bridge::CONFIG_SESSION_STOP_TIMEOUT`]）
/// 耗尽后可能附加至多 ~10s 槽位宽限等待（[`crate::bridge::GRACE_WAIT_DURATION`]，
/// 见该常量处说明）。
#[utoipa::path(
    post,
    path = "/api/profiles/{id}/config-session/stop",
    operation_id = "post_api_profiles_id_config_session_stop",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID"),
        ("discard" = bool, Query, description = "true 时仅关闭浏览器不落盘录制（缺省 false）"),
        ("apply" = Option<bool>, Query, description = "结算写盘开关（缺省 true）：非 discard 时录制动作写回方案；false 仅结算会话不写方案，任务录制流程专用"),
    ),
    responses(
        (status = 200, description = "会话已结束；响应携带录制动作数组（informational，discard/apply=false 也回显）——apply=true（缺省）且非 discard 时已写回方案，apply=false 仅结算不写方案", body = DataEnvelope<ConfigSessionStopData>),
        (status = 404, description = "方案不存在", body = ErrorResponse),
        (status = 409, description = "无活跃配置会话，或活跃会话属于其他方案（归属校验拒绝跨方案写盘）", body = ErrorResponse),
        (status = 500, description = "录制动作解析失败或命令超时等内部错误", body = ErrorResponse),
        (status = 503, description = "Worker 未安装或环境不可用（Worker 重启后无活跃会话的陈旧归属走 409 自愈）", body = ErrorResponse),
    )
)]
pub async fn stop_config_session(
    State(bridge): State<Arc<dyn BridgeApi>>,
    State(profiles): State<Arc<dyn ProfileApi>>,
    State(config): State<Arc<dyn ConfigApi>>,
    Path(id): Path<String>,
    Query(query): Query<ConfigSessionStopQuery>,
) -> Result<Json<Value>, ApiError> {
    // 缺省 true：方案编辑器「完成配置」路径与历史调用方不动，任务录制流程显式
    // 传 apply=false 关闭写盘副作用
    let apply = query.apply.unwrap_or(true);
    let profile = config.load_profile(&id)?;
    // 归属校验先行：本地无记录 / 记录指向别的方案时一律不派发——占用互斥
    // 以 Worker 为权威，但「会话属于哪个方案」只有 Rust 侧知道，跨方案
    // 写盘必须在这里拦下
    let owner = bridge
        .config_session_owner()
        .ok_or_else(|| ApiError::Conflict("无活跃配置会话".into()))?;
    if owner.profile_id != id {
        return Err(ApiError::Conflict(format!(
            "活跃配置会话属于方案「{}」，请在该方案下停止",
            owner.profile_id
        )));
    }
    // 未录制会话（自由浏览模式）没有可落盘的录制内容，apply=true（缺省）且非
    // discard 的结算会以空数组覆盖方案既有录制——在归属校验后、派发前拦下
    // （归属保留，可改用 discard 或 apply=false 重新结算）；apply=false 本就
    // 不写方案，无覆盖面，放行
    if !owner.record && apply && !query.discard {
        return Err(ApiError::BadRequest(
            "本次会话以自由浏览模式打开（未录制），无可保存的录制内容；请用「仅关闭」方式结束会话"
                .into(),
        ));
    }
    let resp = bridge
        .execute_config_session_stop(serde_json::json!({ "session_id": owner.session_id }))
        .await?;
    // 收到 Worker 结构化回包即清归属：成功 = 会话已拆除；结构化失败 = 会话
    // 本已不存在（Worker 重启/崩溃后的陈旧归属）。仅传输层错误（上方 `?`，
    // 超时/崩溃，会话状态未知）保持归属，等下次 stop 惰性自愈
    bridge.take_config_session_owner();
    if !resp.result.success {
        let msg = resp
            .result
            .error
            .unwrap_or_else(|| "停止配置会话失败".into());
        // Worker 侧无会话（文案钉在 python_worker 的 profile_session_stop 契约）：
        // 陈旧归属自愈为统一的 409「无活跃配置会话」
        if msg.contains("没有活跃配置会话") || msg.contains("配置会话不存在") {
            return Err(ApiError::Conflict("无活跃配置会话".into()));
        }
        return Err(ApiError::Internal(msg));
    }
    let truncated = resp
        .result
        .data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Worker 返回形状即方案存储形状（RecordedAction 契约逐字对齐），解析失败
    // 属于契约破裂，按内部错误处理而不静默丢录制
    let mut recorded: Vec<crate::config::schema::RecordedAction> = resp
        .result
        .data
        .get("recorded_actions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("录制动作解析失败: {e}")))?
        .unwrap_or_default();
    // 等值归一先于序列化：落盘与响应回显一次覆盖（明文凭据在两处都不得留存）。
    // 凭据取 `runtime_config_for_profile` 的解密快照——解密失败时密码为空、
    // 归一按空凭据跳过（fail-open：不归一也不丢录制）
    let credentials = config.runtime_config_for_profile(&id)?;
    normalize_recorded_credential_values(
        &mut recorded,
        &credentials.profile.username,
        &credentials.profile.password,
    );
    let recorded_count = recorded.len() as u64;
    // 动作数组进响应：无论 discard 与否、apply 与否，Worker 回包携带即原样返回
    // ——apply=false 的任务录制流程靠它取动作序列，省一次 GET 方案详情。
    // 序列化须在写盘分支 move 掉 recorded 之前完成
    let recorded_actions = serde_json::to_value(&recorded).unwrap_or_default();
    let mut persistent_enabled = profile.persistent_session_enabled;
    if !query.discard && apply {
        let actions = recorded;
        let result = profiles
            .update_profile_tx(
                &id,
                Box::new(move |mut profile| {
                    Box::pin(async move {
                        profile.recorded_actions = actions;
                        profile.persistent_session_enabled = true;
                        Ok(profile)
                    })
                }),
            )
            .await?;
        match result {
            Ok(_) => persistent_enabled = true,
            // 事务闭包在本路径无可失败校验，该分支按不可达防御处理
            Err(msg) => return Err(ApiError::BadRequest(msg)),
        }
    }
    tracing::info!(
        profile_id = %id,
        discard = query.discard,
        apply,
        recorded_count,
        truncated,
        "配置会话已停止"
    );
    Ok(data(serde_json::json!({
        "recorded_count": recorded_count,
        "truncated": truncated,
        "persistent_session_enabled": persistent_enabled,
        "recorded_actions": recorded_actions,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use crate::bridge::{BridgeError, ConfigSessionOwner, IpcResponse, IpcResult};
    use crate::config::{ConfigError, ProfileData, ProfileSummary, SettingsData};
    use crate::engine::EngineError;
    use crate::tasks::{TaskApi, TaskError, TaskKind};

    struct MockInner {
        profiles: Vec<ProfileData>,
        active: String,
        auto_switch: bool,
        /// switch_profile 派发的 ApplyProfile 目标 ID（验证 Engine 联动）
        dispatched_apply_profile: Vec<String>,
        /// 内存任务表：只服务 `validate_login_task_binding` 的存在性 + 类型判定
        tasks: Vec<(String, TaskKind)>,
        /// 方案根目录（base_path）：配置会话 start 在其下 ensure 持久目录，
        /// 用 TempDir 保证不污染进程工作目录
        #[allow(dead_code)]
        base: tempfile::TempDir,
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
                    active_script_task: p.active_script_task.clone(),
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

    /// 内存 TaskApi：只实现 `validate_login_task_binding` 用到的 `load_task`，
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
                    modified_at: String::new(),
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

    // ============ 配置会话端点（config-session）的 Bridge mock ============

    /// 预置回包脚本的一项
    enum MockReply {
        /// Worker 成功回包（success=true，data 为响应载荷）
        Ok(Value),
        /// Worker 结构化失败（success=false，error 为消息）
        WorkerError(String),
        /// 传输层错误（execute_with_timeout 直接返回 Err）
        Transport(BridgeError),
    }

    /// 内存 BridgeApi：按脚本依序回包并记录派发，归属注册表用真实语义
    struct MockBridgeApi {
        script: std::sync::Mutex<VecDeque<MockReply>>,
        sent: std::sync::Mutex<Vec<(String, Value)>>,
        owner: std::sync::Mutex<Option<ConfigSessionOwner>>,
    }

    impl MockBridgeApi {
        fn new(script: Vec<MockReply>) -> Self {
            Self {
                script: std::sync::Mutex::new(script.into()),
                sent: std::sync::Mutex::new(Vec::new()),
                owner: std::sync::Mutex::new(None),
            }
        }

        /// 派发的命令与参数（按序）
        fn sent(&self) -> Vec<(String, Value)> {
            self.sent.lock().unwrap().clone()
        }

        /// 当前归属
        fn owner(&self) -> Option<ConfigSessionOwner> {
            self.owner.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl BridgeApi for MockBridgeApi {
        async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
            self.execute_with_timeout(method, params, Duration::from_secs(1))
                .await
        }

        fn cancel(&self, _cancel_id: &str) {}

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: Duration,
        ) -> Result<IpcResponse, BridgeError> {
            self.sent.lock().unwrap().push((method.to_string(), params));
            match self.script.lock().unwrap().pop_front() {
                Some(MockReply::Ok(data)) => Ok(IpcResponse {
                    id: 1,
                    result: IpcResult {
                        success: true,
                        data,
                        error: None,
                    },
                }),
                Some(MockReply::WorkerError(msg)) => Ok(IpcResponse {
                    id: 1,
                    result: IpcResult {
                        success: false,
                        data: Value::Null,
                        error: Some(msg),
                    },
                }),
                Some(MockReply::Transport(e)) => Err(e),
                None => panic!("MockBridgeApi 回包脚本已耗尽: {method}"),
            }
        }

        async fn force_recycle(&self) {}

        fn has_live_worker(&self) -> bool {
            false
        }

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}

        fn config_session_owner(&self) -> Option<ConfigSessionOwner> {
            self.owner.lock().unwrap().clone()
        }

        fn set_config_session_owner(&self, owner: ConfigSessionOwner) {
            *self.owner.lock().unwrap() = Some(owner);
        }

        fn take_config_session_owner(&self) -> Option<ConfigSessionOwner> {
            self.owner.lock().unwrap().take()
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
            self.0.lock().unwrap().base.path().to_path_buf()
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
    /// + BridgeApi 组合为单一 state 类型
    #[derive(Clone)]
    struct TestState {
        profiles: Arc<dyn ProfileApi>,
        config: Arc<dyn ConfigApi>,
        engine: Arc<dyn EngineApi>,
        tasks: Arc<dyn TaskApi>,
        bridge: Arc<dyn BridgeApi>,
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

    impl axum::extract::FromRef<TestState> for Arc<dyn BridgeApi> {
        fn from_ref(state: &TestState) -> Self {
            state.bridge.clone()
        }
    }

    /// 测试用直连任务（`validate_login_task_binding` 只关心类型与 ID）
    fn http_task(id: &str) -> (String, TaskKind) {
        (
            id.to_string(),
            TaskKind::Http(Box::new(crate::tasks::HttpTaskConfig {
                common: crate::tasks::CommonFields {
                    task_id: id.to_string(),
                    name: format!("直连任务 {id}"),
                    description: String::new(),
                },
                url: "http://10.1.1.55/login".into(),
                ..Default::default()
            })),
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

    /// 测试用脚本任务（脚本渠道的绑定校验与直连同构，只是类型判据不同）
    fn script_task(id: &str) -> (String, TaskKind) {
        (
            id.to_string(),
            TaskKind::Script(crate::tasks::ScriptTaskConfig {
                common: crate::tasks::CommonFields {
                    task_id: id.to_string(),
                    name: format!("脚本任务 {id}"),
                    description: String::new(),
                },
                content: Some("print(1)".into()),
                ..Default::default()
            }),
        )
    }

    fn mock_app() -> (axum::Router, Arc<std::sync::Mutex<MockInner>>) {
        let (app, inner, _bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(Vec::new())));
        (app, inner)
    }

    /// 带预置回包脚本的 mock app（配置会话端点测试用）；返回 bridge 具体句柄供断言
    fn mock_app_with_bridge(
        bridge: Arc<MockBridgeApi>,
    ) -> (
        axum::Router,
        Arc<std::sync::Mutex<MockInner>>,
        Arc<MockBridgeApi>,
    ) {
        let inner = Arc::new(std::sync::Mutex::new(MockInner {
            profiles: vec![profile_of("default"), profile_of("dorm")],
            active: "default".into(),
            auto_switch: false,
            dispatched_apply_profile: Vec::new(),
            tasks: vec![
                http_task("portal-http"),
                browser_task("portal-browser"),
                script_task("portal-login"),
            ],
            base: tempfile::TempDir::new().unwrap(),
        }));
        let state = TestState {
            profiles: Arc::new(MockProfileApi(inner.clone())),
            config: Arc::new(MockConfigApi(inner.clone())),
            engine: Arc::new(MockEngineApi(inner.clone())),
            tasks: Arc::new(MockTaskApi(inner.clone())),
            bridge: bridge.clone(),
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
            .route(
                "/api/profiles/{id}/config-session/start",
                axum::routing::post(start_config_session),
            )
            .route(
                "/api/profiles/{id}/config-session/stop",
                axum::routing::post(stop_config_session),
            )
            .with_state(state);
        (app, inner, bridge)
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
        // 两条「渠道 → 任务」绑定都要在摘要里（任务页据此标出任务被谁在用）
        assert_eq!(d["profiles"]["dorm"]["active_http_task"], "");
        assert_eq!(d["profiles"]["dorm"]["active_script_task"], "");
        // 密码等敏感字段不得出现在摘要中
        assert!(d["profiles"]["dorm"].get("password").is_none());
    }

    /// 统一出口完整保存与回读，非法更新不得覆盖已有有效配置。
    #[tokio::test]
    async fn profile_network_roundtrip_and_invalid_update_is_atomic() {
        let (app, inner) = mock_app();
        let network = serde_json::json!({"mode":"interface", "interface_id":"campus-adapter", "proxy_url":"", "dns_servers":["10.0.0.53"]});
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/profiles/net")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"name":"校园网", "username":"u", "password":"", "network":network}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/profiles/net")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["data"]["settings"]["network"],
            network
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/net")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"network":{"mode":"interface","interface_id":""}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let saved = inner
            .lock()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.id == "net")
            .unwrap()
            .clone();
        assert_eq!(
            serde_json::to_value(saved.network.as_ref().unwrap()).unwrap(),
            network
        );
        assert!(
            build_share_payload(&saved)["profile"]
                .get("network")
                .is_none(),
            "网卡标识和代理凭据不应随分享文件泄露"
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/net")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"login_channel":"script"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

    /// 脚本渠道未绑定脚本任务 → 400（与直连同一口径：没有兜底任务，未绑定必然登不上）
    #[tokio::test]
    async fn test_create_script_profile_without_binding_is_rejected() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/script-unbound")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "脚本未绑定",
                            "username": "u",
                            "password": "p",
                            "login_channel": "script"
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
                .contains("请为脚本渠道选择一个脚本任务"),
            "{json}"
        );
    }

    /// 脚本渠道绑定到非脚本任务（这里是直连任务）→ 400：仅查存在性会放行，
    /// 登录时才发现拿到的是别类配置
    #[tokio::test]
    async fn test_create_script_profile_with_wrong_task_type_is_rejected() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/script-wrong-type")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "绑错类型",
                            "username": "u",
                            "password": "p",
                            "login_channel": "script",
                            "active_script_task": "portal-http"
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
                .contains("不存在或不是脚本任务"),
            "{json}"
        );
    }

    /// 脚本渠道绑定正确 → 落盘并可在详情里读回（新建路径）
    #[tokio::test]
    async fn test_create_script_profile_with_binding_persists() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/lab-script")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "实验室脚本",
                            "username": "u",
                            "password": "p",
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
        let g = inner.lock().unwrap();
        let created = g
            .profiles
            .iter()
            .find(|p| p.id == "lab-script")
            .expect("已创建 lab-script");
        assert_eq!(created.login_channel, LoginChannel::Script);
        assert_eq!(created.active_script_task, "portal-login");
    }

    /// 分享载荷必须清空脚本绑定（接收方多半没有同名脚本任务）
    #[tokio::test]
    async fn test_export_clears_script_task_binding() {
        let (app, inner) = mock_app();
        {
            let mut g = inner.lock().unwrap();
            let dorm = g.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.login_channel = LoginChannel::Script;
            dorm.active_script_task = "portal-login".into();
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/profiles/dorm/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let profile = &json["data"]["profile"];
        // 渠道本身跟着走（那是方案属性），绑定清空（那是本机任务）
        assert_eq!(profile["login_channel"], "script");
        assert_eq!(profile["active_script_task"], "");
        assert_eq!(profile["active_http_task"], "");
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

    // ============ 配置会话端点（config-session start/stop）============

    /// Worker 回包里的一条录制动作（与 profile_session_stop 响应契约同形）
    fn recorded_click_json(ts: i64) -> Value {
        serde_json::json!({
            "type": "click",
            "selector": "#login-btn",
            "strategy": "id",
            "ts": ts,
        })
    }

    async fn post_config_session(app: &axum::Router, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// start 成功：派发 profile_session_start（持久目录 / 门户地址 / 录制开启 /
    /// 浏览器设置整体下发）并记录归属
    #[tokio::test]
    async fn config_session_start_成功派发并记录归属() {
        let (app, _inner, bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::Ok(
                serde_json::json!({"session_id": "sess-1", "recorded": true}),
            )])));
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["started"], true);

        let sent = bridge.sent();
        assert_eq!(sent.len(), 1, "应恰好派发一条命令");
        assert_eq!(sent[0].0, "profile_session_start");
        let params = &sent[0].1;
        assert!(
            params["user_data_dir"]
                .as_str()
                .unwrap()
                .ends_with("browser_profiles/dorm"),
            "持久目录应按方案 id 拼接: {params}"
        );
        assert!(
            !params["portal_url"].as_str().unwrap().is_empty(),
            "portal_url 取 effective_browser_login_url 口径恒非空"
        );
        assert_eq!(params["record"], true, "录制必须开启");
        assert!(
            params["browser_settings"].is_object(),
            "browser_settings 整体下发: {params}"
        );
        let owner = bridge.owner().expect("start 成功后应记录归属");
        assert_eq!(owner.session_id, "sess-1");
        assert_eq!(owner.profile_id, "dorm");
        assert!(owner.record, "录制模式（缺省）归属应带 record=true");
    }

    /// start 时 Worker 报占用（重复启动）→ 409
    #[tokio::test]
    async fn config_session_start_worker占用归409() {
        let (app, _inner, bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::WorkerError(
                "已存在活跃配置会话，请先停止当前配置会话".into(),
            )])));
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let v = body_json(resp).await;
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("已存在活跃配置会话"),
            "{v}"
        );
        assert!(bridge.owner().is_none(), "start 失败不得留下归属记录");
    }

    /// start 时 Rust 会话矩阵拒绝（在途登录占用槽位）→ 409
    #[tokio::test]
    async fn config_session_start_会话槽位忙归409() {
        let (app, _inner, _bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::Transport(
                BridgeError::WorkerBusy,
            )])));
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// start 时方案不存在 → 404，且不派发任何命令
    #[tokio::test]
    async fn config_session_start_方案不存在404() {
        let (app, _inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        let resp = post_config_session(&app, "/api/profiles/ghost/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(bridge.sent().is_empty(), "404 前不得派发命令");
    }

    /// start 时非浏览器渠道 → 400（持久会话仅浏览器渠道）
    #[tokio::test]
    async fn config_session_start_非浏览器渠道400() {
        let (app, inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        inner
            .lock()
            .unwrap()
            .profiles
            .iter_mut()
            .find(|p| p.id == "dorm")
            .unwrap()
            .login_channel = LoginChannel::Http;
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("持久会话仅浏览器渠道"),
            "{v}"
        );
        assert!(bridge.sent().is_empty(), "渠道校验先于派发");
    }

    /// start 时 Worker 未安装 → 503（既有 From<BridgeError> 映射）
    #[tokio::test]
    async fn config_session_start_worker未安装503() {
        let (app, _inner, _bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::Transport(
                BridgeError::WorkerNotInstalled,
            )])));
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/start").await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 自由浏览模式（?record=false）：派发 record=false 并记录未录制归属
    #[tokio::test]
    async fn config_session_start_free模式派发并记录归属() {
        let (app, inner, bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::Ok(
                serde_json::json!({"session_id": "sess-2", "recorded": false}),
            )])));
        inner
            .lock()
            .unwrap()
            .profiles
            .iter_mut()
            .find(|p| p.id == "dorm")
            .unwrap()
            .persistent_session_enabled = true;
        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/start?record=false").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["started"], true);

        let sent = bridge.sent();
        assert_eq!(sent.len(), 1, "应恰好派发一条命令");
        assert_eq!(sent[0].0, "profile_session_start");
        assert_eq!(sent[0].1["record"], false, "自由浏览模式不得注入录制器");
        let owner = bridge.owner().expect("start 成功后应记录归属");
        assert_eq!(owner.profile_id, "dorm");
        assert!(!owner.record, "自由浏览模式归属应带 record=false");
    }

    /// 自由浏览模式门禁：方案未启用持久会话 → 400，且不派发命令
    #[tokio::test]
    async fn config_session_start_free模式未启用持久会话400() {
        let (app, _inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/start?record=false").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("启用本方案的持久会话"),
            "{v}"
        );
        assert!(bridge.sent().is_empty(), "门禁先于派发");
        assert!(bridge.owner().is_none(), "拒绝时不得留下归属记录");
    }

    /// 未录制会话（自由浏览模式）拒绝 apply=true（缺省）非 discard 结算：空数组
    /// 照单落盘会覆盖方案既有录制；改用 discard 仅关闭或 apply=false 不写盘结算
    /// 均放行并清空归属
    #[tokio::test]
    async fn config_session_stop_未录制会话拒绝落盘结算() {
        let (app, inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![
            // discard 结算的 Worker 回包（record=false 契约：空数组）
            MockReply::Ok(serde_json::json!({
                "recorded_actions": [],
                "truncated": false,
            })),
            // apply=false 结算的 Worker 回包（同样空数组，仅取回不写盘）
            MockReply::Ok(serde_json::json!({
                "recorded_actions": [],
                "truncated": false,
            })),
        ])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: false,
        });
        // 先录制前既有动作，验证不被空数组覆盖
        inner
            .lock()
            .unwrap()
            .profiles
            .iter_mut()
            .find(|p| p.id == "dorm")
            .unwrap()
            .recorded_actions = vec![crate::config::schema::RecordedAction {
            action_type: "click".into(),
            selector: "#real-btn".into(),
            strategy: "id".into(),
            ts: 42,
            value: None,
            frame: None,
        }];

        // 非 discard：400 且不派发、归属保留（可改用 discard 重新结算）
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("自由浏览模式打开（未录制）"),
            "{v}"
        );
        assert!(bridge.sent().is_empty(), "守卫先于派发");
        assert!(bridge.owner().is_some(), "守卫不得清空归属");

        // discard=true：仅关闭放行，既有录制原样保留
        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/stop?discard=true").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(
            v["data"]["persistent_session_enabled"], false,
            "discard 不写回，回方案当前值"
        );
        // 断言收进块作用域释放锁：clippy 的 await_holding_lock 只认作用域 drop，
        // 不认显式 drop（本测试后续仍有 await 点）
        {
            let guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
            assert_eq!(dorm.recorded_actions.len(), 1, "既有录制不得被覆盖");
            assert_eq!(dorm.recorded_actions[0].selector, "#real-btn");
        }
        assert!(bridge.owner().is_none(), "discard 结算后归属应清空");

        // apply=false：不写盘同样放行（守卫防的是空录制覆盖方案，本路径无覆盖面）
        // ——discard 结算已清归属，重新登记模拟会话仍在跑
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: false,
        });
        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/stop?apply=false").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(
            v["data"]["recorded_actions"],
            serde_json::json!([]),
            "record=false 契约回包为空数组，原样回显"
        );
        assert_eq!(
            v["data"]["persistent_session_enabled"], false,
            "apply=false 不写回，回方案当前值"
        );
        {
            let guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
            assert_eq!(dorm.recorded_actions.len(), 1, "既有录制不得被覆盖");
            assert_eq!(dorm.recorded_actions[0].selector, "#real-btn");
        }
        assert!(bridge.owner().is_none(), "apply=false 结算后归属应清空");
    }

    /// stop 非 discard：录制解析后事务写回方案（动作 + 开关），归属随结算清空
    #[tokio::test]
    async fn config_session_stop_落盘写回方案() {
        let (app, inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![
            MockReply::Ok(serde_json::json!({
                "recorded_actions": [
                    recorded_click_json(1000),
                    {"type": "change", "selector": "#isp", "strategy": "name", "ts": 2000, "value": "电信"}
                ],
                "truncated": true,
            })),
        ])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["recorded_count"], 2);
        assert_eq!(v["data"]["truncated"], true);
        assert_eq!(v["data"]["persistent_session_enabled"], true);

        // 派发参数带会话 id
        let sent = bridge.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "profile_session_stop");
        assert_eq!(sent[0].1["session_id"], "sess-1");

        // 方案写回断言
        let guard = inner.lock().unwrap();
        let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.recorded_actions.len(), 2);
        assert_eq!(dorm.recorded_actions[0].selector, "#login-btn");
        assert_eq!(dorm.recorded_actions[1].value.as_deref(), Some("电信"));
        assert!(dorm.persistent_session_enabled);
        drop(guard);
        // 归属已随结算清空
        assert!(bridge.owner().is_none(), "stop 结算后归属应清空");
    }

    /// stop discard=true：只关浏览器，录制不落盘、方案不动；响应仍回真实计数、
    /// 动作数组与方案当前开关值
    #[tokio::test]
    async fn config_session_stop_discard不落盘() {
        let (app, inner, bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::Ok(
                serde_json::json!({
                    "recorded_actions": [recorded_click_json(1000)],
                    "truncated": false,
                }),
            )])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/stop?discard=true").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["recorded_count"], 1, "discard 仍回显真实计数");
        assert_eq!(v["data"]["truncated"], false);
        assert_eq!(
            v["data"]["persistent_session_enabled"], false,
            "未写回，回方案当前值"
        );
        assert_eq!(
            v["data"]["recorded_actions"]
                .as_array()
                .expect("discard 也要回动作数组")
                .len(),
            1,
            "动作数组与计数同源回显"
        );
        assert_eq!(
            v["data"]["recorded_actions"][0]["selector"], "#login-btn",
            "动作数组内容为 Worker 回包原样"
        );

        let guard = inner.lock().unwrap();
        let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert!(dorm.recorded_actions.is_empty(), "discard 不得写回录制");
        assert!(!dorm.persistent_session_enabled, "discard 不得打开开关");
        drop(guard);
        assert!(bridge.owner().is_none());
    }

    /// stop apply=false 非 discard：仅结算会话不写方案——方案既有录制与持久
    /// 开关均保持原样（任务录制流程专用：响应带动作数组，前端不必再 GET 方案）
    #[tokio::test]
    async fn config_session_stop_apply_false不写方案仅回动作数组() {
        let (app, inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![
            MockReply::Ok(serde_json::json!({
                "recorded_actions": [
                    recorded_click_json(1000),
                    {"type": "fill", "selector": "#user", "strategy": "id", "ts": 2000, "value": "{{USERNAME}}"}
                ],
                "truncated": false,
            })),
        ])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        // 预置方案既有录制与持久开关（false）：验证既不被覆盖也不被翻转
        inner
            .lock()
            .unwrap()
            .profiles
            .iter_mut()
            .find(|p| p.id == "dorm")
            .unwrap()
            .recorded_actions = vec![crate::config::schema::RecordedAction {
            action_type: "click".into(),
            selector: "#real-btn".into(),
            strategy: "id".into(),
            ts: 42,
            value: None,
            frame: None,
        }];

        let resp =
            post_config_session(&app, "/api/profiles/dorm/config-session/stop?apply=false").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["recorded_count"], 2);
        assert_eq!(v["data"]["truncated"], false);
        assert_eq!(
            v["data"]["persistent_session_enabled"], false,
            "apply=false 不写回，回方案当前值（开关不被动翻转）"
        );
        let actions = v["data"]["recorded_actions"]
            .as_array()
            .expect("apply=false 响应必须携带动作数组");
        assert_eq!(actions.len(), 2, "动作数组与计数同源");
        assert_eq!(actions[0]["selector"], "#login-btn");
        assert_eq!(actions[1]["value"], "{{USERNAME}}", "值原样透传");

        // 方案断言：既有录制原样保留（尤其不被 Worker 新录制覆盖）、开关不动
        let guard = inner.lock().unwrap();
        let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.recorded_actions.len(), 1, "apply=false 不得写回录制");
        assert_eq!(dorm.recorded_actions[0].selector, "#real-btn");
        assert!(!dorm.persistent_session_enabled, "apply=false 不得打开开关");
        drop(guard);
        assert!(bridge.owner().is_none(), "结算后归属应清空");
    }

    /// P1-1b 等值归一矩阵（纯函数）：明文 / URL 编码命中替换、空凭据跳过、
    /// 占位符原样、click/navigate 不参与比对
    #[test]
    fn normalize_recorded_credential_values_归一矩阵() {
        use crate::config::schema::RecordedAction;

        fn action(kind: &str, value: Option<&str>) -> RecordedAction {
            RecordedAction {
                action_type: kind.into(),
                selector: "#x".into(),
                strategy: "id".into(),
                ts: 1,
                value: value.map(str::to_string),
                frame: None,
            }
        }
        let username = "stu001";
        let password = "p@ss w0rd";

        // 明文命中：fill 与 change 均参与，各替换为对应占位符
        let mut actions = vec![
            action("fill", Some("stu001")),
            action("change", Some("p@ss w0rd")),
        ];
        normalize_recorded_credential_values(&mut actions, username, password);
        assert_eq!(actions[0].value.as_deref(), Some("{{USERNAME}}"));
        assert_eq!(actions[1].value.as_deref(), Some("{{PASSWORD}}"));

        // URL 编码形态命中（form_urlencoded 口径：@ → %40、空格 → +）
        let mut actions = vec![action("fill", Some("p%40ss+w0rd"))];
        normalize_recorded_credential_values(&mut actions, username, password);
        assert_eq!(actions[0].value.as_deref(), Some("{{PASSWORD}}"));

        // 空凭据跳过：无凭据可比对，值原样保留
        let mut actions = vec![action("fill", Some("stu001")), action("fill", None)];
        normalize_recorded_credential_values(&mut actions, "", "");
        assert_eq!(actions[0].value.as_deref(), Some("stu001"));
        assert_eq!(actions[1].value, None);

        // 占位符值原样：不与真实凭据恒等，透传
        let mut actions = vec![
            action("fill", Some("{{PASSWORD}}")),
            action("fill", Some("{{USERNAME}}")),
        ];
        normalize_recorded_credential_values(&mut actions, username, password);
        assert_eq!(actions[0].value.as_deref(), Some("{{PASSWORD}}"));
        assert_eq!(actions[1].value.as_deref(), Some("{{USERNAME}}"));

        // click/navigate 不参与比对：navigate 值与凭据恒等也原样保留
        let mut actions = vec![
            action("click", None),
            action("navigate", Some(password)),
        ];
        normalize_recorded_credential_values(&mut actions, username, password);
        assert_eq!(actions[0].value, None);
        assert_eq!(actions[1].value.as_deref(), Some("p@ss w0rd"));

        // 用户名命中优先于密码判定（同值凭据不产生歧义替换）
        let mut actions = vec![action("fill", Some("stu001"))];
        normalize_recorded_credential_values(&mut actions, "stu001", "stu001");
        assert_eq!(actions[0].value.as_deref(), Some("{{USERNAME}}"));
    }

    /// P1-1b 等值归一（结算路径）：Worker 启发式漏判录下的明文凭据在 stop
    /// 结算处归一为占位符——响应回显与方案落盘同一次覆盖，两处均无明文
    #[tokio::test]
    async fn config_session_stop_明文凭据归一为占位符() {
        let (app, inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![
            MockReply::Ok(serde_json::json!({
                "recorded_actions": [
                    {"type": "fill", "selector": "#user", "strategy": "id", "ts": 1000, "value": "stu001"},
                    {"type": "fill", "selector": "#pwd", "strategy": "id", "ts": 2000, "value": "p@ss w0rd"},
                    {"type": "change", "selector": "#isp", "strategy": "name", "ts": 3000, "value": "p%40ss+w0rd"},
                ],
                "truncated": false,
            })),
        ])));
        // 方案快照凭据：用户名原文命中，密码原文 / URL 编码形态各命中一条
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.username = "stu001".into();
            dorm.password = "p@ss w0rd".into();
        }
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let actions = v["data"]["recorded_actions"].as_array().unwrap();
        assert_eq!(actions[0]["value"], "{{USERNAME}}", "回显：用户名原文命中");
        assert_eq!(actions[1]["value"], "{{PASSWORD}}", "回显：密码原文命中");
        assert_eq!(
            actions[2]["value"], "{{PASSWORD}}",
            "回显：密码 URL 编码形态命中"
        );

        // 落盘同一份归一结果：方案存储不留明文
        {
            let guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
            assert_eq!(dorm.recorded_actions.len(), 3);
            assert_eq!(
                dorm.recorded_actions[0].value.as_deref(),
                Some("{{USERNAME}}")
            );
            assert_eq!(
                dorm.recorded_actions[1].value.as_deref(),
                Some("{{PASSWORD}}")
            );
            assert_eq!(
                dorm.recorded_actions[2].value.as_deref(),
                Some("{{PASSWORD}}")
            );
        }
        assert!(bridge.owner().is_none(), "结算后归属应清空");
    }

    /// 契约镜像漂移守卫（P3-19）：`RecordedActionData` 是
    /// `config::schema::RecordedAction` 的线格式镜像声明，两侧 serde 序列化形状
    /// 必须逐键逐值一致（含 `frame` 的携带/缺省两形态与 `value` 的省略语义）——
    /// 源类型演化而镜像未跟随时本测试失败（口径与 system.rs 的
    /// `contract_mirror_dtos_match_source_wire_shapes` 先例一致）
    #[test]
    fn recorded_action_data_matches_source_wire_shape() {
        use crate::config::schema::RecordedAction;

        fn pair(
            kind: &str,
            value: Option<&str>,
            frame: Option<&str>,
        ) -> (RecordedAction, RecordedActionData) {
            let value = value.map(str::to_string);
            let frame = frame.map(str::to_string);
            (
                RecordedAction {
                    action_type: kind.into(),
                    selector: "#x".into(),
                    strategy: "id".into(),
                    ts: 42,
                    value: value.clone(),
                    frame: frame.clone(),
                },
                RecordedActionData {
                    action_type: kind.into(),
                    selector: "#x".into(),
                    strategy: "id".into(),
                    ts: 42,
                    value,
                    frame,
                },
            )
        }

        // 形态矩阵：click 无值；navigate 恒携带 value（落地页 URL）；
        // fill 占位符值；frame 携带 / 缺省各覆盖一档
        for (kind, value, frame) in [
            ("click", None, None),
            ("navigate", Some("http://portal/success"), None),
            ("fill", Some("{{USERNAME}}"), None),
            ("fill", Some("{{PASSWORD}}"), Some("#host >> iframe#portal")),
        ] {
            let (source, mirror) = pair(kind, value, frame);
            assert_eq!(
                serde_json::to_value(&source).expect("源类型序列化"),
                serde_json::to_value(&mirror).expect("镜像序列化"),
                "{kind} 形态线格式漂移"
            );
        }

        // 缺省语义钉死：顶层 click 线格式不带 value / frame 键（不是 null）
        let (source, _mirror) = pair("click", None, None);
        let wire = serde_json::to_value(&source).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"type": "click", "selector": "#x", "strategy": "id", "ts": 42}),
            "{wire}"
        );
    }

    /// stop 无活跃会话（本地无归属）→ 409 且不派发
    #[tokio::test]
    async fn config_session_stop_无活跃会话409() {
        let (app, _inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["message"], "无活跃配置会话");
        assert!(bridge.sent().is_empty(), "归属缺失时不得派发 stop");
    }

    /// stop 归属指向别的方案 → 409（跨方案写盘在归属校验处拦下），归属保留
    #[tokio::test]
    async fn config_session_stop_跨方案归属409() {
        let (app, _inner, bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        let resp = post_config_session(&app, "/api/profiles/default/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let v = body_json(resp).await;
        assert!(
            v["error"]["message"].as_str().unwrap().contains("属于方案"),
            "{v}"
        );
        assert!(bridge.sent().is_empty());
        assert!(
            bridge.owner().is_some(),
            "归属校验失败时不得清空记录（会话仍在别的方案名下）"
        );
    }

    /// stop 陈旧归属（Worker 重启后已无会话）→ 409 自愈并清空归属
    #[tokio::test]
    async fn config_session_stop_陈旧归属自愈409() {
        let (app, _inner, bridge) =
            mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![MockReply::WorkerError(
                "当前没有活跃配置会话，请先启动".into(),
            )])));
        bridge.set_config_session_owner(ConfigSessionOwner {
            session_id: "sess-1".into(),
            profile_id: "dorm".into(),
            record: true,
        });
        let resp = post_config_session(&app, "/api/profiles/dorm/config-session/stop").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["message"], "无活跃配置会话");
        assert!(bridge.owner().is_none(), "结构化失败后归属应自愈清空");
    }

    /// A47 对齐守护：PUT 带未知 `recorded_actions` 字段不得覆盖方案录制
    /// （serde 静默忽略未知字段），显式 `persistent_session_enabled` 正常生效
    #[tokio::test]
    async fn put_未知recorded_actions字段不覆盖录制() {
        let (app, inner, _bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        {
            let mut guard = inner.lock().unwrap();
            let dorm = guard.profiles.iter_mut().find(|p| p.id == "dorm").unwrap();
            dorm.recorded_actions = vec![crate::config::schema::RecordedAction {
                action_type: "click".into(),
                selector: "#real-btn".into(),
                strategy: "id".into(),
                ts: 42,
                value: None,
                frame: None,
            }];
            dorm.persistent_session_enabled = true;
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/dorm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "改名",
                            "recorded_actions": [{"type": "click", "selector": "#bogus"}],
                            "persistent_session_enabled": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let guard = inner.lock().unwrap();
        let dorm = guard.profiles.iter().find(|p| p.id == "dorm").unwrap();
        assert_eq!(dorm.name, "改名");
        assert_eq!(dorm.recorded_actions.len(), 1, "录制必须原样保留");
        assert_eq!(dorm.recorded_actions[0].selector, "#real-btn");
        assert!(
            !dorm.persistent_session_enabled,
            "显式 persistent_session_enabled 字段应生效"
        );
    }

    /// 创建通道接受 persistent_session_enabled（缺省 false 不受影响）
    #[tokio::test]
    async fn create_profile_接受持久会话开关() {
        let (app, inner, _bridge) = mock_app_with_bridge(Arc::new(MockBridgeApi::new(vec![])));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/persist-on")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "持久方案",
                            "username": "u",
                            "password": "p",
                            "persistent_session_enabled": true
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
            .find(|p| p.id == "persist-on")
            .expect("应已创建");
        assert!(created.persistent_session_enabled);
    }
}
