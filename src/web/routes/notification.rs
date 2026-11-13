//! 推送通知路由：测试发送端点
//!
//! `POST /api/notification/test`：用请求体里的通道配置（草稿）或已保存通道 id
//! 实际发送一条测试消息，返回逐通道成功/失败与错误摘要。测试发送不受推送
//! 总开关（`notification.enabled`）限制——用户在开启总开关前就要能验证通道。
//!
//! # 脱敏口径
//!
//! 响应不回显通道凭据字段；错误信息经 [`crate::notification::sender::sanitize_error_text`]
//! 脱敏（URL 去 query/userinfo，reqwest 错误本就不透出 URL，见 notification::sender 模块 doc）。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::config::{ConfigApi, NotificationChannel, NotificationSettings};
use crate::notification::{sender, template};
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

/// 单次测试请求的通道数上限（通道间串行发送，限制端点最坏耗时）
const TEST_MAX_CHANNELS: usize = 3;
/// 测试发送的单通道超时上限（秒）：真实发送用通道配置值（上限 60s），
/// 测试要求快速反馈，超出部分截断——超时在测试里同样判失败
const TEST_TIMEOUT_CAP_SECONDS: u32 = 15;

// ---- 契约 DTO（T5-1 收口 notification 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。请求体 handler 仍按
// `Value` 解析——「二选一字段 + 逐字段中文 400 错误」的统一错误信封语义
// 依赖手工解析（typed 提取会把形状错误变成 422 文本拒绝），schema 以镜像
// DTO 声明；响应回执是 handler 内 `json!` 组装字段的线格式镜像。

/// POST /api/notification/test 请求体（schema 镜像，handler 按 Value 解析）
///
/// `channels`（草稿通道配置，优先）与 `channel_id`（已保存通道）二选一，
/// 两者均缺省时 400。
#[derive(Debug, Deserialize, ToSchema)]
// 纯 schema 载体：字段值仅被 utoipa 生成时引用（handler 保持 Value 解析以
// 维持统一错误信封），无运行时读点，dead_code 属预期
#[allow(dead_code)]
pub struct NotificationTestBody {
    /// 待测试的草稿通道配置数组（1..=3 个；数组项为 serde tagged 对象，`type`
    /// 为判别字段：webhook / bark / serverchan / telegram / os，字段级形状同
    /// GET /api/config 的 `notification.channels` 数组项——契约快照在
    /// `crate::config::schema` 且未接入 ToSchema 体系，故以对象数组标注）
    #[schema(value_type = Array<Object>)]
    pub channels: Option<Vec<serde_json::Value>>,
    /// 已保存通道唯一 id（`channels` 缺省时按 id 从已保存设置查找，不存在为
    /// 404；空串按未提供处理）
    pub channel_id: Option<String>,
}

/// POST /api/notification/test 响应载荷：逐通道测试回执
#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationTestData {
    /// 逐通道发送结果（顺序与请求通道顺序一致）
    pub results: Vec<NotificationTestChannelResult>,
}

/// 单通道测试发送回执（不回显任何凭据字段，错误摘要经脱敏）
#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationTestChannelResult {
    /// 通道唯一 id（草稿通道取配置内 id，已保存通道为其保存 id）
    pub id: String,
    /// 通道显示名（`name` 留空时回退类型中文名，如「Server酱」）
    pub name: String,
    /// 通道类型：webhook / bark / serverchan / telegram / os
    #[serde(rename = "type")]
    pub channel_kind: String,
    /// 本通道是否发送成功
    pub success: bool,
    /// 失败原因摘要（已脱敏：URL 去 query/userinfo 等）；成功时为 null
    pub error: Option<String>,
}

/// 解析测试请求：`channels`（草稿配置，优先）或 `channel_id`（已保存通道）二选一
async fn parse_test_body(
    config: &dyn ConfigApi,
    body: &Value,
) -> Result<Vec<NotificationChannel>, ApiError> {
    let Some(obj) = body.as_object() else {
        return Err(ApiError::BadRequest(
            "请求体必须是对象，包含 channels 数组或 channel_id 字段".into(),
        ));
    };
    if let Some(channels_value) = obj.get("channels") {
        let channels: Vec<NotificationChannel> = serde_json::from_value(channels_value.clone())
            .map_err(|e| {
                ApiError::BadRequest(format!(
                    "channels 配置无效（type 必须是 webhook/bark/serverchan/telegram 之一）: {e}"
                ))
            })?;
        if channels.is_empty() {
            return Err(ApiError::BadRequest("channels 不能为空".into()));
        }
        if channels.len() > TEST_MAX_CHANNELS {
            return Err(ApiError::BadRequest(format!(
                "单次测试最多 {TEST_MAX_CHANNELS} 个通道"
            )));
        }
        return Ok(channels);
    }
    if let Some(channel_id) = obj.get("channel_id").and_then(|v| v.as_str()) {
        // 空串按「未提供」处理（400），而非落进 404 查找
        if channel_id.trim().is_empty() {
            return Err(ApiError::BadRequest("channel_id 不能为空".into()));
        }
        let settings = config.load_settings_async().await.global.notification;
        let found = settings
            .channels
            .iter()
            .find(|c| c.id() == channel_id)
            .cloned();
        return match found {
            Some(channel) => Ok(vec![channel]),
            None => Err(ApiError::NotFound(format!(
                "指定的推送通道不存在: {channel_id}"
            ))),
        };
    }
    Err(ApiError::BadRequest(
        "请提供要测试的通道配置（channels）或已保存通道 id（channel_id）".into(),
    ))
}

/// POST /api/notification/test — 用测试消息实际发送一次，返回逐通道结果
///
/// 测试发送不受推送总开关（`notification.enabled`）限制——用户在开启总开关
/// 前就要能验证通道；单次最多 3 个通道、单通道超时截断到 15s（快速反馈）。
#[utoipa::path(
    post,
    path = "/api/notification/test",
    operation_id = "post_api_notification_test",
    tag = "notification",
    request_body(
        content = NotificationTestBody,
        description = "草稿通道配置数组（channels，优先）或已保存通道 id（channel_id）二选一",
    ),
    responses(
        (status = 200, description = "逐通道测试回执（success=false 的通道 error 携带脱敏摘要）", body = DataEnvelope<NotificationTestData>),
        (status = 400, description = "请求体不是对象 / 缺少二选一字段 / channels 为空或超过 3 个 / 通道结构非法（如 URL 非 http/https）", body = ErrorResponse),
        (status = 404, description = "channel_id 指定的已保存通道不存在", body = ErrorResponse),
    )
)]
pub async fn test_notification(
    State(config): State<Arc<dyn ConfigApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let channels = parse_test_body(config.as_ref(), &body).await?;
    // 结构校验（URL scheme / 凭据非空等）：结构非法的通道直接 400，给出中文错误，
    // 不真发（避免把配置错误伪装成网络失败）
    let mut probe = NotificationSettings::default();
    for channel in &channels {
        probe.channels.push(channel.clone());
        if let Err(msg) = probe.validate() {
            return Err(ApiError::BadRequest(msg));
        }
    }

    let message = template::MessageContent::test_message();
    let mut results = Vec::with_capacity(channels.len());
    for channel in &channels {
        // 测试发送：单次尝试、超时截断到 TEST_TIMEOUT_CAP_SECONDS（快速反馈）
        let timeout =
            Duration::from_secs(channel.timeout_seconds().min(TEST_TIMEOUT_CAP_SECONDS) as u64);
        let outcome = sender::send_once(channel, &message, timeout).await;
        let (success, error) = match outcome {
            Ok(()) => (true, None),
            Err(err) => (false, Some(sender::sanitize_error_text(&err))),
        };
        results.push(json!({
            "id": channel.id(),
            "name": channel.display_name(),
            "type": channel.kind(),
            "success": success,
            "error": error,
        }));
    }
    Ok(data(json!({ "results": results })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use std::sync::Mutex;
    use tower::ServiceExt;

    use crate::config::{ConfigError, ProfileData, SettingsData};
    use crate::web::routes::test_support::body_json;

    /// 内存 ConfigApi：仅 load_settings_async 有意义（端点只读设置），
    /// settings 经共享句柄由测试用例写入
    struct MockConfigApi(Arc<Mutex<SettingsData>>);

    #[async_trait::async_trait]
    impl ConfigApi for MockConfigApi {
        async fn load_settings_async(&self) -> SettingsData {
            self.0.lock().unwrap().clone()
        }
        async fn save_settings(&self, _data: &SettingsData) -> Result<(), ConfigError> {
            Err(ConfigError::ProfileNotFound {
                id: "unused".into(),
            })
        }
        async fn modify_settings_tx(
            &self,
            _f: Box<dyn FnOnce(SettingsData) -> Result<SettingsData, String> + Send>,
        ) -> Result<Result<(), String>, ConfigError> {
            Ok(Err("unused".into()))
        }
        fn load_profile(&self, id: &str) -> Result<ProfileData, ConfigError> {
            Err(ConfigError::ProfileNotFound { id: id.to_string() })
        }
        async fn save_profile(&self, _profile: &ProfileData) -> Result<(), ConfigError> {
            Ok(())
        }
        async fn reload(&self) -> Result<(), ConfigError> {
            Ok(())
        }
        fn can_decrypt_password(&self, _ciphertext: &str) -> bool {
            false
        }
        fn has_decryption_error(&self) -> bool {
            false
        }
        fn base_path(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
        fn runtime_snapshot(&self) -> std::sync::Arc<crate::config::RuntimeConfig> {
            unreachable!("通知测试端点不消费运行时快照")
        }
        fn encrypt_password(&self, _raw: &str) -> Result<String, ConfigError> {
            Ok(String::new())
        }
    }

    /// 测试夹具：共享 settings + 装配好的单路由 app
    struct Fixture {
        settings: Arc<Mutex<SettingsData>>,
        app: axum::Router,
    }

    fn fixture() -> Fixture {
        let settings = Arc::new(Mutex::new(SettingsData::default()));
        let app = axum::Router::new()
            .route("/api/notification/test", post(test_notification))
            .with_state(Arc::new(MockConfigApi(settings.clone())) as Arc<dyn ConfigApi>);
        Fixture { settings, app }
    }

    async fn post_test(fixture: &Fixture, body: Value) -> (StatusCode, Value) {
        let resp = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/notification/test")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    fn webhook_json(url: &str) -> Value {
        json!({
            "type": "webhook",
            "id": "ch-1",
            "name": "本地接收器",
            "enabled": true,
            "url": url,
            "headers": [],
            "body_template": "",
            "timeout_seconds": 3
        })
    }

    /// 通道配置齐全 + 可达目标：200 + success=true（走真实本机回环）
    #[tokio::test]
    async fn test_send_success_with_reachable_local_receiver() {
        let fixture = fixture();
        // 起一个一次性接收器
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = axum::Router::new().route("/hook", post(|| async { StatusCode::OK }));
            axum::serve(listener, app).await.unwrap();
        });
        let (status, value) = post_test(
            &fixture,
            json!({ "channels": [webhook_json(&format!("http://{addr}/hook"))] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let result = &value["data"]["results"][0];
        assert_eq!(result["success"], true, "{value}");
        assert_eq!(result["type"], "webhook");
        assert_eq!(result["id"], "ch-1");
        // 响应不回显任何凭据字段
        assert!(result.get("url").is_none());
        assert!(result.get("headers").is_none());
    }

    /// 草稿通道结构非法（非 http scheme）→ 400 + 中文错误，不真发
    #[tokio::test]
    async fn test_send_rejects_invalid_channel_shape() {
        let fixture = fixture();
        let (status, value) = post_test(
            &fixture,
            json!({ "channels": [webhook_json("ftp://example.com/hook")] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("http/https"), "{value}");
    }

    /// 通道 id 查找：存在 → 测试发送；不存在 → 404
    #[tokio::test]
    async fn test_send_by_channel_id() {
        let fixture = fixture();
        fixture
            .settings
            .lock()
            .unwrap()
            .global
            .notification
            .channels = vec![NotificationChannel::Serverchan {
            id: "saved-1".into(),
            name: "已保存".into(),
            enabled: false,
            send_key: "sct-fake".into(),
            timeout_seconds: 3,
        }];
        // 已保存通道：即使 disabled 也可测试（测试发送不受启用状态限制）
        let (status, value) = post_test(&fixture, json!({ "channel_id": "saved-1" })).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let result = &value["data"]["results"][0];
        assert_eq!(result["id"], "saved-1");
        // send_key 无效 → Server酱返回非 2xx，错误信息含状态码且不回显 key
        assert_eq!(result["success"], false);
        let error = result["error"].as_str().unwrap_or_default();
        assert!(!error.contains("sct-fake"), "{error}");

        // 不存在的 id → 404
        let (status, value) = post_test(&fixture, json!({ "channel_id": "ghost" })).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    }

    /// 空请求体 / 缺字段 → 400；超量通道 → 400
    #[tokio::test]
    async fn test_send_bad_requests() {
        let fixture = fixture();
        for body in [
            json!([]),
            json!({}),
            json!({ "channels": [] }),
            json!({ "channel_id": "" }),
        ] {
            let (status, value) = post_test(&fixture, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        }
        // 4 个通道超限
        let channels: Vec<Value> = (0..4)
            .map(|i| {
                let mut c = webhook_json("https://example.com/hook");
                c["id"] = json!(format!("ch-{i}"));
                c
            })
            .collect();
        let (status, value) = post_test(&fixture, json!({ "channels": channels })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("最多 3 个"),
            "{value}"
        );
    }

    /// 未知通道 type → 400（serde tagged 判别错误转中文提示）
    #[tokio::test]
    async fn test_send_unknown_channel_type_rejected() {
        let fixture = fixture();
        let (status, value) = post_test(
            &fixture,
            json!({ "channels": [json!({
                "type": "pigeon", "id": "x", "name": "", "enabled": true
            })] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("channels 配置无效"), "{value}");
    }

    /// 测试发送不受全局总开关限制（enabled=false 时仍可测试）
    #[tokio::test]
    async fn test_send_ignores_global_enabled_switch() {
        let fixture = fixture();
        let mut settings = SettingsData::default();
        settings.global.notification.enabled = false;
        settings.global.notification.channels = vec![NotificationChannel::Serverchan {
            id: "saved-1".into(),
            name: String::new(),
            enabled: true,
            send_key: "sct-fake".into(),
            timeout_seconds: 3,
        }];
        *fixture.settings.lock().unwrap() = settings;
        let (status, value) = post_test(&fixture, json!({ "channel_id": "saved-1" })).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert!(value["data"]["results"][0]["success"].is_boolean());
    }
}
