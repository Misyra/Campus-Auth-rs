//! ApiError → HTTP 响应映射 + 统一响应包装
//!
//! 成功响应统一为 `{ "data": <payload> }`，错误响应统一为
//! `{ "error": { "code": "...", "message": "...", "details": {...} } }`。
//! 禁止 `success` 字段。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};

/// 字段级校验错误
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    /// 出错字段名
    pub field: String,
    /// 错误信息
    pub message: String,
}

/// 统一成功响应包装 `{ "data": T }`
///
/// 序列化为 `{"data": <T>}`，不含 `success` 字段。
#[derive(Debug, Clone, Serialize)]
pub struct ApiResponse<T> {
    /// 响应数据（序列化键为 `data`）
    pub data: T,
}

impl<T> ApiResponse<T> {
    /// 构造成功响应
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T: Serialize> IntoResponse for ApiResponse<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

/// 构造成功响应 `Json({"data": payload})`，供返回 `Json<Value>` 的 handler 使用
pub fn data<T: Serialize>(payload: T) -> Json<Value> {
    Json(json!({ "data": payload }))
}

/// API 统一错误
///
/// 变体与 `code()` 错误码一一对应；仅保留实际被构造或属于文档化契约的
/// 变体（历史死变体 BadCredential/AuthUrlUnreachable/NotImplemented/
/// OperationCancelled/PortInUse/RateLimited 已删除：全仓零构造，
/// openapi.json 与 docs/ 均无对应错误码契约）。
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 请求参数错误（400）
    #[error("{0}")]
    BadRequest(String),
    /// 资源不存在（404）
    #[error("{0}")]
    NotFound(String),
    /// 资源冲突（409）
    #[error("{0}")]
    Conflict(String),
    /// 校验失败（422）
    #[error("{}", .0.iter().map(|f| format!("{}: {}", f.field, f.message)).collect::<Vec<_>>().join("; "))]
    Validation(Vec<FieldError>),
    /// 内部错误（500）
    #[error("{0}")]
    Internal(String),
    /// 服务不可用（503）
    #[error("{0}")]
    ServiceUnavailable(String),
    /// Worker 未安装（503）
    #[error("{0}")]
    WorkerNotInstalled(String),
    /// Worker 忙（409）
    #[error("{0}")]
    WorkerBusy(String),
}

impl ApiError {
    /// 错误码（稳定的机器可读字符串，参见 openapi.json / docs 错误码表）
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "BAD_REQUEST",
            ApiError::NotFound(_) => "NOT_FOUND",
            ApiError::Conflict(_) => "CONFLICT",
            ApiError::Validation(_) => "VALIDATION_ERROR",
            ApiError::Internal(_) => "INTERNAL_ERROR",
            ApiError::ServiceUnavailable(_) => "SERVICE_UNAVAILABLE",
            ApiError::WorkerNotInstalled(_) => "WORKER_NOT_INSTALLED",
            ApiError::WorkerBusy(_) => "WORKER_BUSY",
        }
    }

    /// HTTP 状态码
    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::WorkerNotInstalled(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::WorkerBusy(_) => StatusCode::CONFLICT,
        }
    }

    /// 人类可读消息（由 thiserror 生成的 Display 提供）
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// 补充详情（因错误码而异，可能为 None）
    pub fn details(&self) -> Option<Value> {
        match self {
            ApiError::Validation(fields) => Some(json!({ "fields": fields })),
            _ => None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 5xx 代表服务端故障必须留痕（error 级）；4xx 多为参数/权限问题，
        // 用 debug 级避免每个参数错刷屏。仅记录状态码、错误码与消息，不记录请求体内容。
        if self.status().is_server_error() {
            tracing::error!(
                status = self.status().as_u16(),
                code = self.code(),
                "API 服务端错误: {}",
                self.message()
            );
        } else {
            tracing::debug!(
                status = self.status().as_u16(),
                code = self.code(),
                "API 请求错误: {}",
                self.message()
            );
        }
        let mut err = serde_json::Map::new();
        err.insert("code".into(), json!(self.code()));
        err.insert("message".into(), json!(self.message()));
        if let Some(d) = self.details() {
            err.insert("details".into(), d);
        }
        let body = json!({ "error": Value::Object(err) });
        (self.status(), Json(body)).into_response()
    }
}

// ---- 各服务错误 → ApiError 自动转换 ----

impl From<crate::config::ConfigError> for ApiError {
    fn from(e: crate::config::ConfigError) -> Self {
        match e {
            crate::config::ConfigError::ConfigNotFound { .. }
            | crate::config::ConfigError::ProfileNotFound { .. } => {
                ApiError::NotFound(e.to_string())
            }
            crate::config::ConfigError::ProfileIdConflict { .. }
            | crate::config::ConfigError::CannotDeleteDefault => ApiError::Conflict(e.to_string()),
            crate::config::ConfigError::InvalidProfileId { .. } => {
                ApiError::BadRequest(e.to_string())
            }
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<crate::tasks::TaskError> for ApiError {
    fn from(e: crate::tasks::TaskError) -> Self {
        match e {
            crate::tasks::TaskError::TaskNotFound(_)
            | crate::tasks::TaskError::InvalidTaskId(_) => ApiError::NotFound(e.to_string()),
            crate::tasks::TaskError::DuplicateTaskId(_)
            | crate::tasks::TaskError::DeleteDefaultTask => ApiError::Conflict(e.to_string()),
            crate::tasks::TaskError::ValidationFailed(msgs) => ApiError::Validation(
                msgs.into_iter()
                    .map(|m| FieldError {
                        field: "task".into(),
                        message: m,
                    })
                    .collect(),
            ),
            // Bridge 错误保留变体：WorkerBusy → 409，其余透传为内部错误
            crate::tasks::TaskError::Bridge(crate::bridge::BridgeError::WorkerBusy) => {
                ApiError::WorkerBusy("Worker 忙: 调试会话进行中".into())
            }
            crate::tasks::TaskError::Bridge(inner) => ApiError::Internal(inner.to_string()),
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<crate::scheduler::SchedulerError> for ApiError {
    fn from(e: crate::scheduler::SchedulerError) -> Self {
        match e {
            crate::scheduler::SchedulerError::TaskNotFound(_) => ApiError::NotFound(e.to_string()),
            crate::scheduler::SchedulerError::InvalidCronExpr(_, _)
            | crate::scheduler::SchedulerError::InvalidTaskId(_)
            | crate::scheduler::SchedulerError::TargetNotFound(_) => {
                ApiError::BadRequest(e.to_string())
            }
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<crate::bridge::BridgeError> for ApiError {
    fn from(e: crate::bridge::BridgeError) -> Self {
        match e {
            crate::bridge::BridgeError::WorkerNotInstalled => {
                ApiError::WorkerNotInstalled(e.to_string())
            }
            crate::bridge::BridgeError::WorkerStartupTimeout => {
                ApiError::ServiceUnavailable(e.to_string())
            }
            crate::bridge::BridgeError::WorkerSpawnBlocked => {
                ApiError::ServiceUnavailable(e.to_string())
            }
            crate::bridge::BridgeError::WorkerBusy => ApiError::WorkerBusy(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<crate::engine::EngineError> for ApiError {
    fn from(e: crate::engine::EngineError) -> Self {
        match e {
            crate::engine::EngineError::ChannelFull => ApiError::ServiceUnavailable(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<crate::updater::UpdaterError> for ApiError {
    fn from(e: crate::updater::UpdaterError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<crate::environment::EnvironmentError> for ApiError {
    fn from(e: crate::environment::EnvironmentError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        // serde_json::Error 在 `?` 路径中多用于内部序列化/反序列化（不含请求体解析，
        // 后者由各 route 显式映射为 BadRequest）。内部序列化失败应映射 500，而非 400。
        ApiError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部变体 → HTTP 状态码映射正确性
    #[test]
    fn test_status_code_mapping() {
        assert_eq!(
            ApiError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::NotFound("x".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::Conflict("x".into()).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError::Validation(vec![]).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ApiError::Internal("x".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::ServiceUnavailable("x".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError::WorkerNotInstalled("x".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError::WorkerBusy("x".into()).status(),
            StatusCode::CONFLICT
        );
    }

    /// 错误码稳定且互不相同（前端按 code 分支）
    #[test]
    fn test_error_codes_stable_and_unique() {
        let codes: Vec<&str> = [
            ApiError::BadRequest("".into()),
            ApiError::NotFound("".into()),
            ApiError::Conflict("".into()),
            ApiError::Validation(vec![]),
            ApiError::Internal("".into()),
            ApiError::ServiceUnavailable("".into()),
            ApiError::WorkerNotInstalled("".into()),
            ApiError::WorkerBusy("".into()),
        ]
        .iter()
        .map(|e| e.code())
        .collect();
        let unique: std::collections::HashSet<&&str> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "错误码必须唯一");
    }

    /// Validation 变体携带字段级 details 载荷
    #[test]
    fn test_validation_details() {
        let e = ApiError::Validation(vec![
            FieldError {
                field: "name".into(),
                message: "必填".into(),
            },
            FieldError {
                field: "url".into(),
                message: "非法".into(),
            },
        ]);
        let details = e.details().expect("Validation 应有 details");
        assert_eq!(details["fields"][0]["field"], "name");
        assert_eq!(details["fields"][1]["message"], "非法");
        // 非 Validation 变体无 details
        assert!(ApiError::BadRequest("x".into()).details().is_none());
    }

    /// 错误响应体结构：{ "error": { "code", "message", ... } }
    #[tokio::test]
    async fn test_into_response_body_shape() {
        let resp = ApiError::BadRequest("参数错误".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读取响应体");
        let body: Value = serde_json::from_slice(&bytes).expect("响应体应为 JSON");
        assert_eq!(body["error"]["code"], "BAD_REQUEST");
        assert_eq!(body["error"]["message"], "参数错误");
        assert!(body.get("success").is_none(), "禁止 success 字段");
    }

    /// 服务错误自动转换：ConfigError 代表性分支
    #[test]
    fn test_from_config_error() {
        let e: ApiError = crate::config::ConfigError::ProfileNotFound { id: "x".into() }.into();
        assert!(matches!(e, ApiError::NotFound(_)));
        let e: ApiError = crate::config::ConfigError::ProfileIdConflict { id: "x".into() }.into();
        assert!(matches!(e, ApiError::Conflict(_)));
        let e: ApiError = crate::config::ConfigError::CannotDeleteDefault.into();
        assert!(matches!(e, ApiError::Conflict(_)));
        let e: ApiError = crate::config::ConfigError::InvalidProfileId {
            id: "../settings".into(),
        }
        .into();
        assert!(matches!(e, ApiError::BadRequest(_)));
    }

    /// 服务错误自动转换：TaskError / BridgeError 代表性分支
    #[test]
    fn test_from_task_and_bridge_error() {
        let e: ApiError = crate::tasks::TaskError::TaskNotFound("x".into()).into();
        assert!(matches!(e, ApiError::NotFound(_)));
        let e: ApiError = crate::tasks::TaskError::DuplicateTaskId("x".into()).into();
        assert!(matches!(e, ApiError::Conflict(_)));
        let e: ApiError = crate::tasks::TaskError::ValidationFailed(vec!["a".into()]).into();
        assert!(matches!(e, ApiError::Validation(_)));
        // Bridge 类型化错误：WorkerBusy → 409 WorkerBusy
        let e: ApiError =
            crate::tasks::TaskError::Bridge(crate::bridge::BridgeError::WorkerBusy).into();
        assert!(
            matches!(e, ApiError::WorkerBusy(_)),
            "Bridge::WorkerBusy 应映射为 409，实际 {e:?}"
        );
        // 其他 Bridge 错误 → 500 内部错误
        let e: ApiError =
            crate::tasks::TaskError::Bridge(crate::bridge::BridgeError::Timeout).into();
        assert!(matches!(e, ApiError::Internal(_)));

        let e: ApiError = crate::bridge::BridgeError::WorkerNotInstalled.into();
        assert!(matches!(e, ApiError::WorkerNotInstalled(_)));
        let e: ApiError = crate::bridge::BridgeError::WorkerBusy.into();
        assert!(matches!(e, ApiError::WorkerBusy(_)));
    }

    /// 成功响应包装：{ "data": payload }
    #[tokio::test]
    async fn test_data_wrapper() {
        let json = data(json!({ "ok": true }));
        let bytes = axum::body::to_bytes(json.into_response().into_body(), 4096)
            .await
            .expect("读取响应体");
        let body: Value = serde_json::from_slice(&bytes).expect("响应体应为 JSON");
        assert_eq!(body["data"]["ok"], true);
    }
}
