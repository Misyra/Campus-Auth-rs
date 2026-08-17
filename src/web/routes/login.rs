//! 登录路由：触发登录、取消登录、查询状态、一次性登录
//!
//! M1 细粒度 state：登录编排经 `State<Arc<dyn LoginApi>>` 提取（mock 可换），
//! 状态快照经 `State<Arc<StatusManager>>` 提取（内存实现，测试直接构造）。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::login::LoginApi;
use crate::status::{LoginSource, StatusManager};
use crate::web::error::{data, ApiError};

/// POST /api/login — 触发手动登录
///
/// 前端可能发送 `null` 或 `{source, task_id}`，用 `Json<Value>` 宽松接收。
pub async fn trigger_login(
    State(login): State<Arc<dyn LoginApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some("browser") => LoginSource::Browser,
        _ => LoginSource::Manual,
    };
    let task_id = body
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let profile_id = body
        .get("profile_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let handle = login.submit(source, task_id, profile_id).await;
    let result = handle.await_result().await;
    // 登录失败（配置缺失、auth_url 不可达、凭证无效等）是预期业务结果而非服务端错误，
    // 统一以 200 + {success, message, duration} 返回，避免前端把环境性失败当作 500 异常。
    Ok(data(serde_json::json!({
        "success": result.success,
        "message": result.message,
        "duration": result.duration.as_secs_f64(),
    })))
}

/// POST /api/login/cancel — 取消当前登录流程
pub async fn cancel_login(
    State(login): State<Arc<dyn LoginApi>>,
) -> Result<Json<Value>, ApiError> {
    // await 等待状态锁，避免撞上 submit 持锁窗口时取消被静默丢弃（B2）
    login.cancel_current().await;
    Ok(data(Value::String("已取消".into())))
}

/// GET /api/login/status — 查询当前登录状态
pub async fn get_login_status(
    State(status): State<Arc<StatusManager>>,
) -> Result<Json<Value>, ApiError> {
    let snapshot = status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
}

/// POST /api/login/once — login_once 模式（执行一次登录后退出）
///
/// 与 trigger_login 保持一致：登录失败返回 200 + {success: false}，而非 500。
pub async fn login_once(
    State(login): State<Arc<dyn LoginApi>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some("browser") => LoginSource::Browser,
        _ => LoginSource::LoginOnce,
    };
    let task_id = body
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let profile_id = body
        .get("profile_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let handle = login.submit(source, task_id, profile_id).await;
    let result = handle.await_result().await;
    Ok(data(serde_json::json!({
        "once": true,
        "success": result.success,
        "message": result.message,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use tower::ServiceExt;

    use crate::login::{LoginHandle, LoginResult};
    use std::time::Duration;

    /// mock 记录的提交参数
    #[derive(Clone, PartialEq, Eq, Debug)]
    struct SubmitCall {
        source: LoginSource,
        task_id: Option<String>,
        profile_id: Option<String>,
    }

    #[derive(Default)]
    struct MockLoginInner {
        submits: std::sync::Mutex<Vec<SubmitCall>>,
        cancels: std::sync::Mutex<usize>,
    }

    struct MockLogin {
        inner: Arc<MockLoginInner>,
        result: LoginResult,
    }

    #[async_trait::async_trait]
    impl LoginApi for MockLogin {
        async fn submit(
            &self,
            source: LoginSource,
            task_id: Option<String>,
            profile_id: Option<String>,
        ) -> LoginHandle {
            self.inner
                .submits
                .lock()
                .unwrap()
                .push(SubmitCall {
                    source,
                    task_id,
                    profile_id,
                });
            LoginHandle::immediate(self.result.clone())
        }

        async fn cancel_current(&self) {
            *self.inner.cancels.lock().unwrap() += 1;
        }
    }

    fn result(success: bool) -> LoginResult {
        LoginResult {
            success,
            message: if success { "登录成功" } else { "凭证缺失" }.into(),
            source: LoginSource::Manual,
            duration: Duration::from_millis(1500),
            attempts: 1,
        }
    }

    /// 测试路由：登录路由 + 真实 StatusManager（内存实现）
    fn mock_app(
        result: LoginResult,
    ) -> (Router, Arc<MockLoginInner>, Arc<StatusManager>) {
        let inner = Arc::new(MockLoginInner::default());
        let status = Arc::new(StatusManager::new());
        let login: Arc<dyn LoginApi> = Arc::new(MockLogin {
            inner: inner.clone(),
            result,
        });
        #[derive(Clone)]
        struct TestState {
            login: Arc<dyn LoginApi>,
            status: Arc<StatusManager>,
        }
        impl axum::extract::FromRef<TestState> for Arc<dyn LoginApi> {
            fn from_ref(s: &TestState) -> Self {
                s.login.clone()
            }
        }
        impl axum::extract::FromRef<TestState> for Arc<StatusManager> {
            fn from_ref(s: &TestState) -> Self {
                s.status.clone()
            }
        }
        let state = TestState {
            login,
            status: status.clone(),
        };
        let app = Router::new()
            .route("/api/login", post(trigger_login))
            .route("/api/login/cancel", post(cancel_login))
            .route("/api/login/status", get(get_login_status))
            .route("/api/login/once", post(login_once))
            .with_state(state);
        (app, inner, status)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// body 缺省 source → Manual；登录失败以 200 + success:false 返回（业务结果非 500）
    #[tokio::test]
    async fn test_trigger_login_defaults_to_manual_and_maps_failure_to_200() {
        let (app, inner, _status) = mock_app(result(false));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"task_id":"t1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["success"], false);
        assert_eq!(v["data"]["message"], "凭证缺失");
        assert_eq!(v["data"]["duration"], 1.5);
        // 提交参数：source 默认 Manual、task_id 透传、profile_id 缺省 None
        assert_eq!(
            inner.submits.lock().unwrap().as_slice(),
            &[SubmitCall {
                source: LoginSource::Manual,
                task_id: Some("t1".into()),
                profile_id: None,
            }]
        );
    }

    /// source=browser 显式映射；成功路径响应字段齐全
    #[tokio::test]
    async fn test_trigger_login_browser_source_success() {
        let (app, inner, _status) = mock_app(result(true));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"source":"browser","task_id":"b1","profile_id":"p9"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["success"], true);
        assert_eq!(
            inner.submits.lock().unwrap().as_slice(),
            &[SubmitCall {
                source: LoginSource::Browser,
                task_id: Some("b1".into()),
                profile_id: Some("p9".into()),
            }]
        );
    }

    /// 取消接口调用 cancel_current 恰好一次
    #[tokio::test]
    async fn test_cancel_login_calls_api_once() {
        let (app, inner, _status) = mock_app(result(true));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(*inner.cancels.lock().unwrap(), 1);
    }

    /// 状态接口返回 StatusManager 快照（真实内存实现，验证序列化通路）
    #[tokio::test]
    async fn test_login_status_returns_snapshot() {
        let (app, _inner, _status) = mock_app(result(true));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/login/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        // 默认快照可序列化为对象（字段存在性由 status 模块测试覆盖）
        assert!(v["data"].is_object());
    }

    /// login_once 缺省 source → LoginOnce
    #[tokio::test]
    async fn test_login_once_defaults_to_login_once_source() {
        let (app, inner, _status) = mock_app(result(true));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login/once")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["once"], true);
        assert_eq!(
            inner.submits.lock().unwrap().as_slice(),
            &[SubmitCall {
                source: LoginSource::LoginOnce,
                task_id: None,
                profile_id: None,
            }]
        );
    }
}
