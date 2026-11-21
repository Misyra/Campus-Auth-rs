//! 自启动路由：自启动状态、启用/禁用
//!
//! M1 细粒度 state（config 域）：handler 声明 `State<Arc<dyn ConfigApi>>` 依赖，
//! 不再触达 `state.container`。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

// ---- 契约 DTO（T5-1c autostart 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。状态查询的 Windows/macOS
// 系统观测字段经 `serde_json` merge **平铺**进响应顶层且随平台而异，不便收敛
// 单一 DTO，以 `DataEnvelope<Value>` + 响应说明诚实标注；开关响应形状本地可定。

/// POST /api/autostart/enable 与 /api/autostart/disable 响应载荷
#[derive(Debug, Serialize, ToSchema)]
pub struct AutostartToggleData {
    /// 操作结果提示
    pub message: String,
    /// 配置缓存保存失败时的告警（系统状态已生效，不反向覆盖用户选择）；
    /// 仅 Windows/macOS 携带该字段（其他平台缓存失败按 400 报错并回滚系统注册）
    pub warning: Option<String>,
}

/// GET /api/autostart/status — 获取自启动状态
#[utoipa::path(
    get,
    path = "/api/autostart/status",
    operation_id = "get_api_autostart_status",
    tag = "autostart",
    responses(
        // 平台条件字段经 merge 平铺进 data 顶层，形状因平台而异，暂以 Value +
        // 本说明承载（强类型化批次再收敛）。公共字段：platform / enabled /
        // method / location / runtime_mode；Windows/macOS 另并入 source("system")
        // 与 cached_enabled 及系统观测字段——Windows：registered / path_match /
        // base_path_match / target_exists / blocked_by_windows / expected_path /
        // expected_base_path / registry_path / registry_command / startup_approved /
        // state；macOS：registered / path_match / base_path_match / target_exists /
        // run_at_load / loaded / job_pid / expected_path / expected_base_path /
        // plist_path / state；其他平台 source 为 system（openwrt=procd）或 config。
        (status = 200, description = "自启动状态（Windows/macOS 的 enabled 以系统观测为准，配置位仅为 cached_enabled）", body = DataEnvelope<Value>),
        (status = 500, description = "系统自启动状态读取失败（Windows/macOS 观测路径）", body = ErrorResponse),
    )
)]
pub async fn get_autostart(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    // Windows/macOS：自启动有效状态以系统观测为准（注册表 / LaunchAgent plist + launchd）
    #[cfg(any(windows, target_os = "macos"))]
    let observed = {
        let base = config.base_path();
        tokio::task::spawn_blocking(move || crate::utils::platform::self_start_status(&base))
            .await
            .map_err(|e| ApiError::Internal(format!("查询自启动状态后台任务异常: {e}")))?
            .map_err(|error| ApiError::internal(format!("{error:#}")))?
    };
    #[cfg(any(windows, target_os = "macos"))]
    let enabled = observed.enabled;
    // Linux（非 openwrt）：XDG autostart 文件为系统事实（P3-38）——enabled
    // 取「文件存在且 Exec 指向当前可执行」的观测结论，配置位降为
    // cached_enabled；此前回显配置位，用户手删 .desktop 文件仍显示已启用
    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    let observed = tokio::task::spawn_blocking(crate::utils::platform::xdg_self_start_status)
        .await
        .map_err(|e| ApiError::Internal(format!("查询自启动状态后台任务异常: {e}")))?;
    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    let enabled = observed.enabled;
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let enabled = settings.global.app.autostart_enabled;
    #[cfg(all(target_os = "linux", feature = "openwrt"))]
    let enabled = tokio::task::spawn_blocking(crate::utils::platform::openwrt_self_start_enabled)
        .await
        .map_err(|e| ApiError::Internal(format!("查询自启动状态后台任务异常: {e}")))?
        .map_err(|error| ApiError::internal(format!("{error:#}")))?;
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
    // macOS：launchd 观测字段并入响应（形状兼容 Windows 口径，前端忽略未知字段）
    #[cfg(target_os = "macos")]
    {
        status
            .as_object_mut()
            .ok_or_else(|| ApiError::internal("自启动响应序列化失败"))?
            .extend(
                serde_json::to_value(&observed)
                    .map_err(ApiError::internal)?
                    .as_object()
                    .ok_or_else(|| ApiError::internal("自启动状态序列化失败"))?
                    .clone(),
            );
        status["source"] = "system".into();
        status["cached_enabled"] = settings.global.app.autostart_enabled.into();
        status["method"] = "LaunchAgent".into();
        status["location"] = observed.plist_path.clone().unwrap_or_default().into();
    }
    // Linux（非 openwrt）：XDG autostart 观测字段并入响应（P3-38，形状对齐
    // Windows/macOS：registered / path_match / state + cached_enabled）
    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    {
        status
            .as_object_mut()
            .ok_or_else(|| ApiError::internal("自启动响应序列化失败"))?
            .extend(
                serde_json::to_value(&observed)
                    .map_err(ApiError::internal)?
                    .as_object()
                    .ok_or_else(|| ApiError::internal("自启动状态序列化失败"))?
                    .clone(),
            );
        status["source"] = "system".into();
        status["cached_enabled"] = settings.global.app.autostart_enabled.into();
        status["method"] = "desktop file".into();
        status["location"] = observed.desktop_path.clone().into();
    }
    // 其余平台（openwrt/BSD 等）保留既有注册机制与兼容字段。
    #[cfg(not(any(
        windows,
        target_os = "macos",
        all(target_os = "linux", not(feature = "openwrt"))
    )))]
    {
        status["source"] = if cfg!(feature = "openwrt") {
            "system"
        } else {
            "config"
        }
        .into();
    }
    // 安卓端：系统自启由 APK 内 BootReceiver 声明（编译期固定），端内无注册机制
    // 可查询/改动，enabled 仅回显配置缓存位；enable/disable 路径在
    // register_self_start 显式拒绝
    if cfg!(feature = "android") {
        status["method"] = "APK BootReceiver".into();
        status["source"] = "apk".into();
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
    #[cfg(target_os = "macos")]
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
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        serde_json::json!({ "source": "unavailable" })
    }
}

/// POST /api/autostart/enable — 启用自启动
#[utoipa::path(
    post,
    path = "/api/autostart/enable",
    operation_id = "post_api_autostart_enable",
    tag = "autostart",
    responses(
        (status = 200, description = "已启用并通过系统状态复核（Windows/macOS 配置缓存保存失败时附 warning，仍为 200）", body = DataEnvelope<AutostartToggleData>),
        (status = 400, description = "配置落盘校验失败（非 Windows/macOS；系统注册已回滚）", body = ErrorResponse),
        (status = 500, description = "系统注册失败（配置未改动）或注册任务异常", body = ErrorResponse),
    )
)]
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
    #[cfg(any(windows, target_os = "macos"))]
    {
        mutation_result(res, true)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    if let Err(msg) = res? {
        // 落盘失败时回滚系统注册，保持一致
        let _ = register_self_start(false, config.base_path()).await;
        return Err(ApiError::BadRequest(msg));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        tracing::info!("已启用开机自启动");
        Ok(data(serde_json::json!({ "message": "已启用开机自启动" })))
    }
}

/// POST /api/autostart/disable — 禁用自启动
#[utoipa::path(
    post,
    path = "/api/autostart/disable",
    operation_id = "post_api_autostart_disable",
    tag = "autostart",
    responses(
        (status = 200, description = "已取消并通过系统状态复核（Windows/macOS 配置缓存保存失败时附 warning，仍为 200）", body = DataEnvelope<AutostartToggleData>),
        (status = 400, description = "配置落盘校验失败（非 Windows/macOS；系统注册已回滚）", body = ErrorResponse),
        (status = 500, description = "取消系统注册失败（配置未改动）或注册任务异常", body = ErrorResponse),
    )
)]
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
    #[cfg(any(windows, target_os = "macos"))]
    {
        mutation_result(res, false)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    if let Err(msg) = res? {
        let _ = register_self_start(true, config.base_path()).await;
        return Err(ApiError::BadRequest(msg));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        tracing::info!("已禁用开机自启动");
        Ok(data(serde_json::json!({ "message": "已禁用开机自启动" })))
    }
}

/// 系统操作已完成时，缓存失败只告警，不能通过反向系统操作覆盖用户选择。
#[cfg(any(windows, target_os = "macos"))]
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
/// 显式用户操作触发；Windows/macOS 将当前数据目录一同登记并回读系统状态。
async fn register_self_start(enabled: bool, _base: std::path::PathBuf) -> Result<(), ApiError> {
    // 安卓端自启由 APK 内 BootReceiver 声明（编译期固定），端内无注册机制可改——
    // 显式 400 比走平台 fallback 的笼统报错更明确
    if cfg!(feature = "android") {
        return Err(ApiError::BadRequest(
            "安卓端系统自启由 APK 内 BootReceiver 管理，无需在应用内设置".into(),
        ));
    }
    let join = tokio::task::spawn_blocking(move || {
        #[cfg(any(windows, target_os = "macos"))]
        {
            crate::utils::platform::set_self_start_for_base(enabled, &_base)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
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

    /// Linux（非 openwrt）测试前置：独占锁 + XDG_CONFIG_HOME 指向空临时目录，
    /// 隔离 status 观测对真实用户 autostart 目录的读取（同文件各 linux 测试
    /// 经锁串行，互设环境变量会竞态）；守卫 drop 时恢复原值
    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    struct XdgEnvGuard {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    impl Drop for XdgEnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
                None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
            }
        }
    }

    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    async fn isolate_xdg_config_home() -> XdgEnvGuard {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let guard = LOCK.lock().await;
        let dir = tempfile::tempdir().expect("创建临时 XDG 配置目录");
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        // edition 2024 起 set_var 为 unsafe（并发可见性无法静态证明）
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        XdgEnvGuard {
            _lock: guard,
            _dir: dir,
            previous,
        }
    }

    /// 默认关闭：enabled=false，method 占位 "-"，platform 与编译目标一致
    /// （macOS 走系统观测，enabled 取决于真机登记状态，不适用配置缓存口径；
    /// Linux 走 XDG 文件观测，须隔离用户配置目录防环境残留）
    #[cfg(not(any(windows, target_os = "macos")))]
    #[tokio::test]
    async fn status_reports_disabled_by_default() {
        #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
        let _xdg = isolate_xdg_config_home().await;
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

    /// P3-38（Linux 非 openwrt）：状态以 XDG autostart 文件为系统事实——写入
    /// Exec 指向当前测试进程的 desktop 文件后 enabled=true、method/观测字段
    /// 齐全；配置位仅作 cached_enabled 回显。此后改写 Exec 指向别处，
    /// enabled 如实回落（path_mismatch），手删文件回落 not_registered
    #[cfg(all(target_os = "linux", not(feature = "openwrt")))]
    #[tokio::test]
    async fn status_observes_xdg_desktop_file_on_linux() {
        let _xdg = isolate_xdg_config_home().await;
        let (app, inner) = mock_app();
        let request = || {
            Request::builder()
                .uri("/api/autostart/status")
                .body(Body::empty())
                .unwrap()
        };
        let write_desktop = |exec: &str| {
            let config_home = std::env::var("XDG_CONFIG_HOME").expect("隔离守卫已设置");
            let autostart_dir = std::path::Path::new(&config_home).join("autostart");
            std::fs::create_dir_all(&autostart_dir).unwrap();
            // 与 enable 注册路径同构的 desktop 文件（Exec 用规范转义构造）
            let content = format!(
                "[Desktop Entry]\nType=Application\nName=Campus-Auth\nExec={exec}\n\
                 Terminal=false\nX-GNOME-Autostart-enabled=true\n"
            );
            std::fs::write(autostart_dir.join("campus-auth.desktop"), content).unwrap();
        };
        // 配置位开启：观测未登记时 enabled 仍为 false（配置位降为 cached_enabled）
        inner.lock().unwrap().settings.global.app.autostart_enabled = true;
        let v = body_json(app.clone().oneshot(request()).await.unwrap()).await;
        assert_eq!(v["data"]["enabled"], false, "无文件时不得显示已启用");
        assert_eq!(v["data"]["cached_enabled"], true);
        assert_eq!(v["data"]["source"], "system");
        assert_eq!(v["data"]["state"], "not_registered");
        assert_eq!(v["data"]["registered"], false);
        // 写入指向当前测试进程的 desktop 文件：观测口径翻转为已启用
        let exe = std::env::current_exe().unwrap().display().to_string();
        write_desktop(&crate::utils::platform::quote_desktop_exec_arg(&exe));
        let v = body_json(app.clone().oneshot(request()).await.unwrap()).await;
        assert_eq!(v["data"]["enabled"], true);
        assert_eq!(v["data"]["method"], "desktop file");
        assert_eq!(v["data"]["registered"], true);
        assert_eq!(v["data"]["path_match"], true);
        assert_eq!(v["data"]["state"], "enabled");
        assert_eq!(v["data"]["expected_path"], serde_json::json!(exe));
        // Exec 指向别处（便携目录搬移未重注册）：path_mismatch 如实呈现
        write_desktop("\"/moved/elsewhere/campus-auth\"");
        let v = body_json(app.oneshot(request()).await.unwrap()).await;
        assert_eq!(v["data"]["enabled"], false);
        assert_eq!(v["data"]["registered"], true);
        assert_eq!(v["data"]["path_match"], false);
        assert_eq!(v["data"]["state"], "path_mismatch");
    }

    /// macOS：enabled 来自 launchd 系统观测，配置缓存翻转不影响系统口径，
    /// 观测字段（registered/state 等）并入响应且形状与 Windows 兼容。
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn autostart_status_uses_launchd_observation_on_macos() {
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
        assert_eq!(first["data"]["platform"], "macos");
        assert_eq!(first["data"]["source"], "system");
        // method 与 Windows 块同口径：无条件标注机制名（LaunchAgent），enabled 决定开关语义
        assert_eq!(first["data"]["method"], "LaunchAgent");
        assert!(first["data"]["enabled"].is_boolean());
        assert!(first["data"]["registered"].is_boolean());
        assert!(first["data"]["state"].is_string());
        inner.lock().unwrap().settings.global.app.autostart_enabled = true;
        let second = body_json(app.oneshot(request()).await.unwrap()).await;
        assert_eq!(first["data"]["enabled"], second["data"]["enabled"]);
        assert_eq!(second["data"]["cached_enabled"], true);
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
