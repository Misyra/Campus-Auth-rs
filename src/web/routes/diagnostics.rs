//! 一键网络诊断路由：POST /api/diagnostics/run + GET /api/diagnostics/bundle
//!
//! 诊断 runner 与诊断包收集核心均按需构造（见 `monitor::diagnostics` 模块
//! 说明）：handler 从 AppState 细粒度取出依赖传入，不进入容器拓扑、不新增
//! 服务字段。走标准鉴权中间件，无任何豁免。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::config::ConfigApi;
use crate::environment::EnvironmentApi;
use crate::login::HistoryStore;
use crate::monitor::diagnostics::{self, DiagnosticInputs};
use crate::network::egress::PreparedEgress;
use crate::utils::metrics::Metrics;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

// ---- 契约 DTO（T5-1 收口 diagnostics 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。诊断报告是
// `crate::monitor::diagnostics::DiagnosticReport`（Serialize-only 类型）的
// 线格式镜像，handler 仍走 `data(serde_json::to_value(..))` 不改返回类型；
// bundle 为 zip 二进制附件响应（范式同 background 域）。

/// POST /api/diagnostics/run 响应载荷（DiagnosticReport 线格式镜像）
#[derive(Debug, Serialize, ToSchema)]
pub struct DiagnosticReportData {
    /// 逐项检查结果（顺序固定：interfaces → gateway → dns → portal → history）
    pub checks: Vec<DiagnosticCheckData>,
    /// 总结论：healthy / degraded / faulty
    pub verdict: String,
    /// 总结论一句话（面向用户直接展示）
    pub conclusion: String,
    /// 整体耗时（毫秒）
    pub total_ms: u64,
}

/// 单项检查结果（DiagnosticCheck 线格式镜像）
#[derive(Debug, Serialize, ToSchema)]
pub struct DiagnosticCheckData {
    /// 检查项标识（机器可读）：interfaces / gateway / dns / portal / history
    pub key: String,
    /// 检查项中文名（前端直接展示）
    pub name: String,
    /// 单项结论：ok / warn / fail / skip
    pub status: String,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// 一句话摘要
    pub summary: String,
    /// 结构化细节（已脱敏，随检查项而异）
    #[schema(value_type = Object)]
    pub details: serde_json::Value,
}

/// GET /api/diagnostics/bundle 200 响应体 schema 载体
///
/// wire 上是 zip 字节流（`application/zip` + `attachment` 下载头），不是
/// JSON 包裹；本类型仅用于把 OpenAPI schema 声明为
/// `{"type": "string", "format": "binary"}`，不参与序列化。
#[derive(Debug, ToSchema)]
#[schema(value_type = String, format = Binary)]
// 纯 schema 载体：仅在 OpenAPI 生成时被 utoipa 引用，字段值不参与任何
// 运行时路径（handler 直接下发字节流），dead_code 属预期
#[allow(dead_code)]
pub struct DiagnosticsBundleBody(Vec<u8>);

/// POST /api/diagnostics/run — 一键网络诊断
///
/// 五项检查并发执行（本机网卡 / 默认网关 / DNS / 认证门户 / 近期登录失败
/// 归因），整体受 15s 总限时约束；结果已脱敏（不含凭据 / token / 带查询串的
/// URL），失败项以结构化明细返回而非 HTTP 错误——诊断失败是「数据」不是
/// 「请求出错」。
#[utoipa::path(
    post,
    path = "/api/diagnostics/run",
    operation_id = "post_api_diagnostics_run",
    tag = "diagnostics",
    responses(
        (status = 200, description = "诊断报告（单项 fail/warn 以结构化 details 返回，不改变 HTTP 状态码）", body = DataEnvelope<DiagnosticReportData>),
        (status = 500, description = "报告序列化失败", body = ErrorResponse),
    )
)]
pub async fn run_diagnostics(
    State(config): State<Arc<dyn ConfigApi>>,
    State(history): State<Arc<dyn HistoryStore>>,
) -> Result<Json<Value>, ApiError> {
    let rt = config.runtime_snapshot();
    // 门户地址取「登录导航有效地址」：auth_url 为空（重定向/兜底）时落到
    // 触发地址，与浏览器渠道实际导航目标保持一致
    let portal_source = if rt.profile.auth_url.trim().is_empty() {
        "fallback_trigger"
    } else {
        "auth_url"
    };
    let portal_url = rt.profile.effective_browser_login_url().to_string();
    // 出口租约与客户端：与登录/监测同一出口语义（直连/系统代理/静态代理）。
    // prepare 或 client 构建失败不拦截整个诊断——原因进门户检查项落 fail
    let (http_client, egress_lease) = match PreparedEgress::prepare(&rt.network).await {
        Ok(egress) => {
            let client = egress
                .client_builder()
                .and_then(|b| b.redirect(reqwest::redirect::Policy::none()).build())
                .map_err(|e| e.to_string());
            (client, Some(egress))
        }
        Err(e) => (Err(format!("出口配置无法生效：{e}")), None),
    };
    let inputs = DiagnosticInputs {
        portal_url,
        portal_source,
        http_client,
        egress: egress_lease,
    };
    let report = diagnostics::run(inputs, history.as_ref()).await;
    tracing::info!(
        verdict = ?report.verdict,
        total_ms = report.total_ms,
        "一键网络诊断完成"
    );
    Ok(data(serde_json::to_value(report)?))
}

/// GET /api/diagnostics/bundle — 一键导出诊断包（zip 附件下载）
///
/// 包内容与信任边界见 `monitor::diagnostics::build_diagnostics_bundle`：
/// manifest + 深度脱敏配置快照 + 事件流 JSONL（30 天）+ 运行日志（7 天）+
/// 环境观测文本，白名单式收集、总量 20MB 软上限。收集为阻塞 I/O，整体放
/// spawn_blocking；分项失败写 manifest 说明而非整包报错。
#[utoipa::path(
    get,
    path = "/api/diagnostics/bundle",
    operation_id = "get_api_diagnostics_bundle",
    tag = "diagnostics",
    responses(
        (status = 200, description = "诊断包 zip 附件（Content-Disposition 指定下载文件名 campus-auth-diagnostics-<时间戳>.zip）",
            content((DiagnosticsBundleBody = "application/zip")),
        ),
        (status = 500, description = "诊断包收集失败（阻塞任务异常或打包错误）", body = ErrorResponse),
    )
)]
pub async fn export_bundle(
    State(config): State<Arc<dyn ConfigApi>>,
    State(environment): State<Arc<dyn EnvironmentApi>>,
    State(metrics): State<Arc<Metrics>>,
) -> Result<impl IntoResponse, ApiError> {
    let base = config.base_path();
    let settings = config.load_settings_async().await;
    let profile = match config.load_profile(&settings.active_profile_id) {
        Ok(p) => p,
        Err(e) => {
            // 活跃 Profile 加载失败时回退空凭据（与 GET /api/config 同语义），warn 留痕
            tracing::warn!(
                profile_id = %settings.active_profile_id,
                "活跃 Profile 加载失败，诊断包内方案节按默认值脱敏导出: {e}"
            );
            crate::config::ProfileData::default()
        }
    };
    let env_status = environment.status();
    let python_path = environment.python_path();
    let uptime_seconds = metrics.uptime_seconds.load(Ordering::Relaxed);
    let llm = crate::ai::load_llm_settings(&base);
    let bundle = tokio::task::spawn_blocking(move || {
        let environment_txt =
            diagnostics::render_environment_text(&env_status, &python_path, &base);
        diagnostics::build_diagnostics_bundle(
            &base,
            &settings,
            &profile,
            &llm,
            environment_txt,
            uptime_seconds,
        )
    })
    .await
    .map_err(|e| ApiError::Internal(format!("诊断包收集任务失败: {e}")))?
    .map_err(ApiError::internal)?;
    tracing::info!(filename = %bundle.filename, "诊断包已导出");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", bundle.filename),
            ),
        ],
        bundle.bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    use crate::environment::{BootstrapStage, EnvironmentError, EnvironmentStatus};
    use crate::web::routes::test_support::MockConfigApi;

    /// 精简 EnvironmentApi 测试替身（status 可配，其余方法不触达）
    struct StubEnvironmentApi(EnvironmentStatus);

    #[async_trait::async_trait]
    impl EnvironmentApi for StubEnvironmentApi {
        fn status(&self) -> EnvironmentStatus {
            self.0.clone()
        }
        fn python_path(&self) -> std::path::PathBuf {
            std::path::PathBuf::from("/venv/python")
        }
        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_playwright_browser(&self, _browser: &str) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        fn ocr_ready(&self) -> bool {
            false
        }
        fn ocr_declared(&self) -> bool {
            true
        }
        fn browser_engine_ready(&self, _engine: &str) -> bool {
            false
        }
    }

    /// 双域 + metrics 的测试 state：config / environment 经 FromRef 委派提取，
    /// metrics 直字段（与 system.rs 日志导出测试同一装配范式）
    #[derive(Clone)]
    struct BundleTestState {
        config: Arc<dyn ConfigApi>,
        environment: Arc<dyn EnvironmentApi>,
        metrics: Arc<Metrics>,
    }

    impl axum::extract::FromRef<BundleTestState> for Arc<dyn ConfigApi> {
        fn from_ref(state: &BundleTestState) -> Self {
            state.config.clone()
        }
    }

    impl axum::extract::FromRef<BundleTestState> for Arc<dyn EnvironmentApi> {
        fn from_ref(state: &BundleTestState) -> Self {
            state.environment.clone()
        }
    }

    impl axum::extract::FromRef<BundleTestState> for Arc<Metrics> {
        fn from_ref(state: &BundleTestState) -> Self {
            state.metrics.clone()
        }
    }

    fn ready_env_status() -> EnvironmentStatus {
        EnvironmentStatus {
            uv_ready: true,
            python_ready: true,
            worker_ready: true,
            manifest_current: true,
            playwright_ready: true,
            system_browser_ready: false,
            ocr_enabled: false,
            ocr_ready: false,
            capability_ready: true,
            stage: BootstrapStage::Idle,
            progress: None,
            last_error: None,
        }
    }

    fn bundle_app(base: std::path::PathBuf) -> axum::Router {
        let (config, cfg) = MockConfigApi::mocked();
        cfg.lock().unwrap().base_path = base;
        let metrics = Metrics::new();
        metrics.uptime_seconds.store(1234, Ordering::Relaxed);
        axum::Router::new()
            .route("/api/diagnostics/bundle", get(export_bundle))
            .with_state(BundleTestState {
                config,
                environment: Arc::new(StubEnvironmentApi(ready_env_status())),
                metrics,
            })
    }

    fn zip_names(bytes: &[u8]) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// 端到端：200 + zip 头 + 附件命名 + 条目齐全 + settings 域敏感值不泄漏 +
    /// manifest 携带运行时长
    #[tokio::test]
    async fn bundle_returns_zip_attachment_with_manifest_and_sanitized_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("events")).unwrap();
        std::fs::write(
            tmp.path()
                .join("events")
                .join(format!("{}.jsonl", chrono::Utc::now().date_naive())),
            b"{\"id\":\"e1\"}\n",
        )
        .unwrap();
        let (config, cfg) = MockConfigApi::mocked();
        cfg.lock().unwrap().base_path = tmp.path().to_path_buf();
        // settings 域塞敏感值：端到端验证不泄漏
        {
            let mut g = cfg.lock().unwrap();
            g.settings.global.app.management_password = "mgmt-secret-9".into();
            g.settings.global.updater.proxy_url = "http://upduser:updpass@127.0.0.1:7890".into();
        }
        let metrics = Metrics::new();
        metrics.uptime_seconds.store(777, Ordering::Relaxed);
        let app = axum::Router::new()
            .route("/api/diagnostics/bundle", get(export_bundle))
            .with_state(BundleTestState {
                config,
                environment: Arc::new(StubEnvironmentApi(ready_env_status())),
                metrics,
            });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/diagnostics/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "application/zip");
        let disposition = resp.headers()["content-disposition"].to_str().unwrap();
        assert!(
            disposition.starts_with("attachment; filename=\"campus-auth-diagnostics-")
                && disposition.ends_with(".zip\""),
            "{disposition}"
        );

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let names = zip_names(&bytes);
        assert!(names.contains(&"manifest.json".to_string()), "{names:?}");
        assert!(
            names.contains(&"config.sanitized.json".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"environment.txt".to_string()), "{names:?}");
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("events/") && n.ends_with(".jsonl")),
            "{names:?}"
        );

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut config_json = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("config.sanitized.json").unwrap(),
            &mut config_json,
        )
        .unwrap();
        for forbidden in ["mgmt-secret-9", "upduser", "updpass"] {
            assert!(
                !config_json.contains(forbidden),
                "诊断包泄漏 {forbidden}: {config_json}"
            );
        }
        assert!(config_json.contains("\"[已脱敏]\""), "{config_json}");

        let mut manifest = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("manifest.json").unwrap(), &mut manifest)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["uptime_seconds"], 777);
        assert_eq!(manifest["sections"]["events"]["files"], 1);
    }

    /// events/logs 目录全缺失：仍返回有效包（manifest + config + environment），不 500
    #[tokio::test]
    async fn bundle_without_data_dirs_still_returns_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let resp = bundle_app(tmp.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/api/diagnostics/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        // zip_names 保持写入顺序：manifest → config → environment
        let names = zip_names(&bytes);
        assert_eq!(
            names,
            vec![
                "manifest.json".to_string(),
                "config.sanitized.json".to_string(),
                "environment.txt".to_string(),
            ]
        );
    }
}
