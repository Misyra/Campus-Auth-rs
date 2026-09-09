//! 自启动路由：自启动状态、启用/禁用
//!
//! M1 细粒度 state（config 域）：handler 声明 `State<Arc<dyn ConfigApi>>` 依赖，
//! 不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};

/// GET /api/autostart/status — 获取自启动状态
pub async fn get_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    let enabled = settings.global.app.autostart_enabled;
    let runtime_mode = serde_json::to_value(&settings.global.app.startup_action)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "monitor".to_string());
    let method = if enabled {
        match std::env::consts::OS {
            "windows" => "Registry",
            "macos" => "LaunchAgent",
            _ => "desktop file",
        }
    } else {
        "-"
    };
    Ok(data(serde_json::json!({
        "platform": std::env::consts::OS,
        "enabled": enabled,
        "method": method,
        "location": "",
        "runtime_mode": runtime_mode,
    })))
}

/// POST /api/autostart/enable — 启用自启动
pub async fn enable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // 先系统注册，成功后再落盘（防配置与系统分叉）
    if let Err(e) = register_self_start(true).await {
        tracing::warn!("启用自启动：系统注册失败（配置未改）: {e}");
        return Err(e);
    }
    // 原子读-改-写：与 `PATCH /api/config` 的 `modify_settings_tx` 同锁，
    // 避免并发保存不同字段时互相覆盖（丢更新）
    let res = config
        .modify_settings_tx(Box::new(|mut s| {
            s.global.app.autostart_enabled = true;
            Ok(s)
        }))
        .await?;
    if let Err(msg) = res {
        // 落盘失败时回滚系统注册，保持一致
        let _ = register_self_start(false).await;
        return Err(ApiError::BadRequest(msg));
    }
    tracing::info!("已启用开机自启动");
    Ok(data(serde_json::json!({ "message": "已启用开机自启动" })))
}

/// POST /api/autostart/disable — 禁用自启动
pub async fn disable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // 先取消系统注册，成功后再落盘
    if let Err(e) = register_self_start(false).await {
        tracing::warn!("禁用自启动：取消系统注册失败（配置未改）: {e}");
        return Err(e);
    }
    let res = config
        .modify_settings_tx(Box::new(|mut s| {
            s.global.app.autostart_enabled = false;
            Ok(s)
        }))
        .await?;
    if let Err(msg) = res {
        let _ = register_self_start(true).await;
        return Err(ApiError::BadRequest(msg));
    }
    tracing::info!("已禁用开机自启动");
    Ok(data(serde_json::json!({ "message": "已禁用开机自启动" })))
}

/// 注册/取消系统自启动（同步阻塞 I/O，置于 spawn_blocking 中执行）
///
/// 仅在配置标志变更后调用，确保“开关状态”与系统实际注册一致。
async fn register_self_start(enabled: bool) -> Result<(), ApiError> {
    let join = tokio::task::spawn_blocking(move || crate::utils::platform::set_self_start(enabled));
    match join.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ApiError::Internal(format!("注册自启动失败: {e}"))),
        Err(e) => Err(ApiError::Internal(format!("自启动注册任务异常: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use super::super::test_support::{MockConfigApi, body_json};

    fn mock_app() -> (
        axum::Router,
        Arc<std::sync::Mutex<super::super::test_support::MockConfigInner>>,
    ) {
        let (config, inner) = MockConfigApi::mocked();
        let app = axum::Router::new()
            .route("/api/autostart/status", get(get_autostart))
            .with_state(config);
        (app, inner)
    }

    /// 默认关闭：enabled=false，method 占位 "-"，platform 与编译目标一致
    #[tokio::test]
    async fn status_reports_disabled_by_default() {
        let (app, _) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/autostart/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["enabled"], false);
        assert_eq!(v["data"]["method"], "-");
        assert_eq!(v["data"]["platform"], std::env::consts::OS);
    }

    /// 开启后 method 不再是占位符（Windows=Registry，其余见实现）
    #[tokio::test]
    async fn status_reports_method_when_enabled() {
        let (app, inner) = mock_app();
        inner.lock().unwrap().settings.global.app.autostart_enabled = true;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/autostart/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["enabled"], true);
        assert_ne!(v["data"]["method"], "-");
    }
}
