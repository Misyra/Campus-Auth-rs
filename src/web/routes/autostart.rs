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
    #[cfg(windows)]
    let observed = {
        let base = config.base_path();
        tokio::task::spawn_blocking(move || crate::utils::platform::self_start_status(&base))
            .await
            .map_err(ApiError::internal)?
            .map_err(|error| ApiError::internal(format!("{error:#}")))?
    };
    #[cfg(windows)]
    let enabled = observed.enabled;
    #[cfg(not(any(windows, all(target_os = "linux", feature = "openwrt"))))]
    let enabled = settings.global.app.autostart_enabled;
    #[cfg(all(target_os = "linux", feature = "openwrt"))]
    let enabled = tokio::task::spawn_blocking(crate::utils::platform::openwrt_self_start_enabled)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let runtime_mode = serde_json::to_value(&settings.global.app.startup_action)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "monitor".to_string());
    let method = if enabled {
        match std::env::consts::OS {
            "windows" => "Registry",
            "macos" => "LaunchAgent",
            "linux" if cfg!(feature = "openwrt") => "procd",
            _ => "desktop file",
        }
    } else {
        "-"
    };
    let mut status = serde_json::json!({
        "platform": std::env::consts::OS,
        "enabled": enabled,
        "method": method,
        "location": "",
        "runtime_mode": runtime_mode,
    });
    #[cfg(windows)]
    {
        status
            .as_object_mut()
            .ok_or_else(|| ApiError::internal("自启动响应序列化失败"))?
            .extend(
                serde_json::to_value(observed)
                    .map_err(ApiError::internal)?
                    .as_object()
                    .ok_or_else(|| ApiError::internal("自启动状态序列化失败"))?
                    .clone(),
            );
        status["source"] = "system".into();
        status["cached_enabled"] = settings.global.app.autostart_enabled.into();
        status["method"] = "Registry".into();
        status["location"] =
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Campus-Auth".into();
    }
    // 其它平台保留既有注册机制与兼容字段。
    #[cfg(not(windows))]
    {
        status["source"] = if cfg!(feature = "openwrt") {
            "system"
        } else {
            "config"
        }
        .into();
    }
    Ok(data(status))
}

/// 导出和反馈包共用系统诊断口径；读取失败仍保留错误，不能阻断整个导出。
pub(super) async fn diagnostic_status(_base: std::path::PathBuf) -> Value {
    #[cfg(windows)]
    {
        match tokio::task::spawn_blocking(move || crate::utils::platform::self_start_status(&_base))
            .await
        {
            Ok(Ok(status)) => serde_json::json!({ "source": "system", "status": status }),
            Ok(Err(error)) => {
                serde_json::json!({ "source": "system", "error": format!("{error:#}") })
            }
            Err(error) => serde_json::json!({ "source": "system", "error": error.to_string() }),
        }
    }
    #[cfg(not(windows))]
    {
        serde_json::json!({ "source": "unavailable" })
    }
}

/// POST /api/autostart/enable — 启用自启动
pub async fn enable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // 先系统注册，成功后再落盘（防配置与系统分叉）
    if let Err(e) = register_self_start(true, config.base_path()).await {
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
        .await;
    #[cfg(windows)]
    {
        mutation_result(res, true)
    }
    #[cfg(not(windows))]
    if let Err(msg) = res? {
        // 落盘失败时回滚系统注册，保持一致
        let _ = register_self_start(false, config.base_path()).await;
        return Err(ApiError::BadRequest(msg));
    }
    #[cfg(not(windows))]
    {
        tracing::info!("已启用开机自启动");
        Ok(data(serde_json::json!({ "message": "已启用开机自启动" })))
    }
}

/// POST /api/autostart/disable — 禁用自启动
pub async fn disable_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    // 先取消系统注册，成功后再落盘
    if let Err(e) = register_self_start(false, config.base_path()).await {
        tracing::warn!("禁用自启动：取消系统注册失败（配置未改）: {e}");
        return Err(e);
    }
    let res = config
        .modify_settings_tx(Box::new(|mut s| {
            s.global.app.autostart_enabled = false;
            Ok(s)
        }))
        .await;
    #[cfg(windows)]
    {
        mutation_result(res, false)
    }
    #[cfg(not(windows))]
    if let Err(msg) = res? {
        let _ = register_self_start(true, config.base_path()).await;
        return Err(ApiError::BadRequest(msg));
    }
    #[cfg(not(windows))]
    {
        tracing::info!("已禁用开机自启动");
        Ok(data(serde_json::json!({ "message": "已禁用开机自启动" })))
    }
}

/// 系统操作已完成时，缓存失败只告警，不能通过反向系统操作覆盖用户选择。
#[cfg(windows)]
fn mutation_result(
    result: Result<Result<(), String>, crate::config::ConfigError>,
    enabled: bool,
) -> Result<Json<Value>, ApiError> {
    let warning = match result {
        Ok(Ok(())) => None,
        Ok(Err(message)) => Some(message),
        Err(error) => Some(error.to_string()),
    };
    let message = if enabled {
        "已启用开机自启动并通过系统状态复核"
    } else {
        "已取消开机自启动并通过系统状态复核"
    };
    if let Some(ref warning) = warning {
        tracing::warn!(%warning, enabled, "自启动系统设置已生效，但配置缓存保存失败");
    }
    tracing::info!(enabled, "自启动系统设置已复核");
    Ok(data(
        serde_json::json!({ "message": message, "warning": warning }),
    ))
}

/// 注册/取消系统自启动（同步阻塞 I/O，置于 spawn_blocking 中执行）
///
/// 显式用户操作触发；Windows 将当前数据目录一同登记并回读系统状态。
async fn register_self_start(enabled: bool, _base: std::path::PathBuf) -> Result<(), ApiError> {
    let join = tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            crate::utils::platform::set_self_start_for_base(enabled, &_base)
        }
        #[cfg(not(windows))]
        {
            crate::utils::platform::set_self_start(enabled)
        }
    });
    match join.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ApiError::Internal(format!("注册自启动失败: {e:#}"))),
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
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
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

    /// 配置缓存翻转不能改变系统状态，查询本身不得写入注册表。
    #[cfg(windows)]
    #[tokio::test]
    async fn autostart_status_ignores_cached_flag() {
        let (app, inner) = mock_app();
        let request = || {
            Request::builder()
                .uri("/api/autostart/status")
                .body(Body::empty())
                .unwrap()
        };
        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first = body_json(first).await;
        inner.lock().unwrap().settings.global.app.autostart_enabled = true;
        let second = body_json(app.oneshot(request()).await.unwrap()).await;
        assert_eq!(first["data"]["enabled"], second["data"]["enabled"]);
        assert_eq!(second["data"]["source"], "system");
        assert_eq!(second["data"]["cached_enabled"], true);
        assert!(second["data"]["registered"].is_boolean());
    }

    #[cfg(windows)]
    #[test]
    fn autostart_cache_failure_does_not_undo_system_change() {
        let result = mutation_result(Ok(Err("模拟缓存写入失败".into())), false).unwrap();
        assert_eq!(result.0["data"]["warning"], "模拟缓存写入失败");
        assert!(
            result.0["data"]["message"]
                .as_str()
                .unwrap()
                .contains("已取消")
        );
        let result = mutation_result(
            Err(crate::config::ConfigError::ConfigWriteError {
                reason: "模拟磁盘故障".into(),
            }),
            true,
        )
        .unwrap();
        assert!(
            result.0["data"]["warning"]
                .as_str()
                .unwrap()
                .contains("模拟磁盘故障")
        );
    }
}
