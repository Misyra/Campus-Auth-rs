//! OpenAPI 契约文档骨架与生成管线（阶段 5 契约战役 T5-0）
//!
//! 三层职责：
//! 1. [`schemas`]：跨域共享的 ToSchema 形状（错误信封、data 包装），后续域
//!    批次一律引用本模块，不得在各 route 文件内重复定义信封；
//! 2. [`ApiDoc`]：utoipa 文档骨架——根元数据（openapi/info/servers）由生成侧
//!    接管，版本号随 `CARGO_PKG_VERSION` 自动同步（三端同版少一处手工项），
//!    paths 逐批挂载已注解域（system 样板 + T5-1a 三域 + T5-1b 五域 + 批三九域）；
//! 3. [`merged_openapi_json`]：生成管线——utoipa 生成 spec 与手写 baseline
//!    （根目录 `openapi.json` 的未注解域条目）按路径合并，产出最终文档。
//!
//! T5-1 挂载进度：system 样板 + T5-1a 三域 + T5-1b 五域 + 批三九域 +
//! T5-1 收口 notification/diagnostics 两域均已接入生成源；2026-11-19
//! 契约批次补齐 system 域 update-state 与 mirror-probe / mirror-preference 三端点；system 域尚有
//! 日志导出 / 浏览器列表等零散 handler 与 events、browsers 等
//! 维持手写条目。
//!
//! 过渡期共存策略：**已注解域的路径条目以生成源为准**（整体替换手写条目），
//! 其余域维持手写条目；后续批次逐域补 `#[utoipa::path]` 注解，全量切换后
//! 手写 baseline 退役、本函数退化为纯生成。根目录 `openapi.json` 的重写由
//! 生成测试在 `UPDATE_OPENAPI=1` 时完成（见本模块 tests）。
//!
//! 确定性保障：utoipa 的 paths/components 为 BTreeMap（键序稳定），合并走
//! serde_json 的 BTreeMap（键恒排序），序列化固定 1 空格缩进 + 文件尾换行，
//! 与既有 baseline 版式一致——重复生成零 diff，避免无意义抖动。

use utoipa::OpenApi;

/// 跨域共享的契约形状（schema 模块）
///
/// 命名与引用规范（给后续域批次）：
/// - `Error*` 为错误信封：任何 `Result<_, ApiError>` 处理器的非 2xx 响应
///   一律 `body = ErrorResponse`（wire 形状 `{ "error": { code, message, details? } }`，
///   对齐 [`crate::web::error::ApiError`] 的 `IntoResponse` 实现）；
/// - `DataEnvelope<T>` 为成功响应包装：2xx 响应一律
///   `body = DataEnvelope<XxxData>`（wire 形状 `{ "data": <T> }`），
///   业务载荷类型以 `XxxData` 命名并定义在各自 route 文件内；
/// - 本模块类型同时派生 `Serialize`：供样板域强类型 handler 直接返回
///   （如 `health_check` 的 `Json<DataEnvelope<HealthData>>`）。
pub mod schemas {
    use serde::Serialize;
    use utoipa::ToSchema;

    /// 错误详情（`error.details`，因错误码而异，可省略）
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct ErrorDetail {
        /// 稳定的机器可读错误码（如 `BAD_REQUEST`，全表见
        /// [`crate::web::error::ApiError::code`]）
        pub code: String,
        /// 人类可读消息
        pub message: String,
        /// 补充详情（如 VALIDATION_ERROR 的字段级错误列表），可省略
        #[schema(value_type = Object, nullable = true)]
        pub details: Option<serde_json::Value>,
    }

    /// 统一错误响应信封 `{ "error": { ... } }`
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct ErrorResponse {
        /// 错误对象
        pub error: ErrorDetail,
    }

    /// 统一成功响应包装 `{ "data": T }`（禁止 `success` 字段）
    #[derive(Debug, Clone, Serialize, ToSchema)]
    pub struct DataEnvelope<T> {
        /// 响应数据
        pub data: T,
    }
}

/// OpenAPI 文档骨架（utoipa 生成源）
///
/// `info`/`servers` 由本骨架权威持有；`paths` 随域批次逐个挂载
/// `#[utoipa::path]` 注解处理器（已挂载：system 样板、config/profiles/login
/// （T5-1a）、tasks/http_tasks/scheduler/scripts/repo（T5-1b）、批三九域
/// monitor/debug/history/uninstall/autostart/ocr/ai/background/tools、
/// T5-1 收口 notification/diagnostics，见 `routes` 各文件注解；
/// events/browsers 等剩余域与 system 域零散 handler 随后续批次接入）。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Campus-Auth Web API",
        version = env!("CARGO_PKG_VERSION"),
        description = "认证喵本地管理界面 API。字段级契约以前端 frontend/src/api/types.ts 为准；\
            手写 baseline 中未注解域的响应 schema 仍为 `{}` 占位，已注解域（生成源条目）\
            携带真实 schema——两者按路径合并产出最终文档，全量切换后本说明随 baseline 退役。",
        contact(name = "Misyra", url = "https://github.com/Misyra/Campus-Auth-rs"),
    ),
    servers(
        (url = "http://127.0.0.1:50721", description = "本机管理界面默认端口"),
    ),
    paths(
        crate::web::routes::system::system_info,
        crate::web::routes::system::restart_app,
        crate::web::routes::system::shutdown_app,
        crate::web::routes::system::agree_terms,
        crate::web::routes::system::health_check,
        crate::web::routes::system::init_status,
        crate::web::routes::system::stop_worker,
        crate::web::routes::system::install_playwright,
        crate::web::routes::system::update_state,
        crate::web::routes::system::probe_environment_mirrors,
        crate::web::routes::system::set_mirror_preference,
        // ---- config 域（T5-1a）----
        crate::web::routes::config::get_settings,
        crate::web::routes::config::patch_settings,
        crate::web::routes::config::patch_settings_section,
        crate::web::routes::config::reload_settings,
        crate::web::routes::config::get_log_levels,
        crate::web::routes::config::set_log_level,
        crate::web::routes::config::get_default_stealth_script,
        crate::web::routes::config::get_pure_mode,
        crate::web::routes::config::set_pure_mode,
        // ---- profiles 域（T5-1a）----
        crate::web::routes::profiles::list_profiles,
        crate::web::routes::profiles::get_profile,
        crate::web::routes::profiles::export_profile,
        crate::web::routes::profiles::import_profile,
        crate::web::routes::profiles::create_profile,
        crate::web::routes::profiles::update_profile,
        crate::web::routes::profiles::delete_profile,
        crate::web::routes::profiles::switch_profile,
        crate::web::routes::profiles::detect_profile,
        crate::web::routes::profiles::auto_switch,
        // ---- profiles 域：配置会话（T3-3c）----
        crate::web::routes::profiles::start_config_session,
        crate::web::routes::profiles::stop_config_session,
        // ---- login 域（T5-1a）----
        crate::web::routes::login::trigger_login,
        crate::web::routes::login::cancel_login,
        crate::web::routes::login::get_login_status,
        crate::web::routes::login::login_once,
        // ---- tasks 域（T5-1b）----
        crate::web::routes::tasks::list_tasks,
        crate::web::routes::tasks::create_task,
        crate::web::routes::tasks::get_task,
        crate::web::routes::tasks::update_task,
        crate::web::routes::tasks::delete_task,
        crate::web::routes::tasks::order_tasks,
        crate::web::routes::tasks::import_tasks,
        crate::web::routes::tasks::export_task,
        crate::web::routes::tasks::execute_task,
        // ---- http_tasks 域（T5-1b）----
        crate::web::routes::http_tasks::test_http_task,
        // ---- scheduler 域（T5-1b）----
        crate::web::routes::scheduler::list_jobs,
        crate::web::routes::scheduler::create_job,
        crate::web::routes::scheduler::update_job,
        crate::web::routes::scheduler::delete_job,
        crate::web::routes::scheduler::toggle_job,
        crate::web::routes::scheduler::run_job,
        crate::web::routes::scheduler::job_history,
        // ---- scripts 域（T5-1b）----
        crate::web::routes::scripts::run_script,
        crate::web::routes::scripts::list_binaries,
        crate::web::routes::scripts::get_script,
        crate::web::routes::scripts::update_script,
        crate::web::routes::scripts::delete_script,
        // ---- repo 域（T5-1b）----
        crate::web::routes::repo::repo_fetch_index,
        crate::web::routes::repo::repo_fetch_task,
        crate::web::routes::repo::repo_image,
        // ---- monitor 域（批三）----
        crate::web::routes::monitor::get_status,
        crate::web::routes::monitor::test_network,
        crate::web::routes::monitor::start_monitor,
        crate::web::routes::monitor::stop_monitor,
        crate::web::routes::monitor::test_redirect,
        crate::web::routes::monitor::get_network_interfaces,
        // ---- debug 域（批三）----
        crate::web::routes::debug::start_debug,
        crate::web::routes::debug::step_debug,
        crate::web::routes::debug::stop_debug,
        crate::web::routes::debug::debug_status,
        crate::web::routes::debug::run_all,
        crate::web::routes::debug::debug_screenshot,
        crate::web::routes::debug::feedback_bundle,
        // ---- history 域（批三）----
        crate::web::routes::history::get_history,
        crate::web::routes::history::clear_history,
        // ---- uninstall 域（批三）----
        crate::web::routes::uninstall::detect_uninstall,
        crate::web::routes::uninstall::uninstall,
        crate::web::routes::uninstall::purge_uninstall,
        // ---- autostart 域（批三）----
        crate::web::routes::autostart::get_autostart,
        crate::web::routes::autostart::enable_autostart,
        crate::web::routes::autostart::disable_autostart,
        // ---- ocr 域（批三）----
        crate::web::routes::ocr::ocr_recognize,
        crate::web::routes::ocr::ocr_status,
        crate::web::routes::ocr::ocr_uninstall,
        crate::web::routes::ocr::ocr_install,
        // ---- ai 域（批三）----
        crate::web::routes::ai::get_llm_config,
        crate::web::routes::ai::put_llm_config,
        crate::web::routes::ai::test_llm_connection,
        crate::web::routes::ai::list_models,
        crate::web::routes::ai::capture,
        crate::web::routes::ai::capture_screenshot,
        crate::web::routes::ai::capture_status,
        crate::web::routes::ai::generate_stream,
        crate::web::routes::ai::capture_bundle,
        // ---- background 域（批三）----
        crate::web::routes::background::get_background,
        crate::web::routes::background::upload_background,
        crate::web::routes::background::fetch_url_background,
        crate::web::routes::background::delete_background,
        // ---- tools 域（批三）----
        crate::web::routes::tools::task_recorder,
        // ---- notification 域（T5-1 收口）----
        crate::web::routes::notification::test_notification,
        // ---- diagnostics 域（T5-1 收口）----
        crate::web::routes::diagnostics::run_diagnostics,
        crate::web::routes::diagnostics::export_bundle,
    ),
    components(schemas(
        schemas::ErrorDetail,
        schemas::ErrorResponse,
        schemas::DataEnvelope<String>,
        schemas::DataEnvelope<crate::web::routes::system::HealthData>,
        schemas::DataEnvelope<crate::web::routes::system::InitStatusData>,
        schemas::DataEnvelope<crate::web::routes::system::SystemInfoData>,
        schemas::DataEnvelope<crate::web::routes::system::WorkerStopData>,
        schemas::DataEnvelope<crate::web::routes::system::InstallPlaywrightData>,
        crate::web::routes::system::HealthData,
        crate::web::routes::system::InitStatusData,
        crate::web::routes::system::EnvReadiness,
        crate::web::routes::system::EnvProgress,
        crate::web::routes::system::SystemInfoData,
        crate::web::routes::system::MetricsSummary,
        crate::web::routes::system::WorkerStopData,
        crate::web::routes::system::InstallPlaywrightData,
        schemas::DataEnvelope<crate::web::routes::system::UpdateStateData>,
        schemas::DataEnvelope<crate::web::routes::system::MirrorProbeData>,
        schemas::DataEnvelope<crate::web::routes::system::MirrorPreferenceData>,
        crate::web::routes::system::UpdateStateData,
        crate::web::routes::system::MirrorSelectionData,
        crate::web::routes::system::MirrorProbeEntryData,
        crate::web::routes::system::MirrorProbeData,
        crate::web::routes::system::MirrorPreferenceData,
        crate::web::routes::system::MirrorPreferencePayload,
        // ---- config 域（T5-1a）----
        schemas::DataEnvelope<crate::web::routes::config::LogLevelData>,
        schemas::DataEnvelope<crate::web::routes::config::DefaultStealthScriptData>,
        schemas::DataEnvelope<crate::web::routes::config::PureModeData>,
        schemas::DataEnvelope<crate::web::routes::config::PureModeToggleData>,
        crate::web::routes::config::LogLevelData,
        crate::web::routes::config::DefaultStealthScriptData,
        crate::web::routes::config::PureModeData,
        crate::web::routes::config::PureModeToggleData,
        crate::web::routes::config::SetLogLevelBody,
        // ---- profiles 域（T5-1a）----
        schemas::DataEnvelope<crate::web::routes::profiles::ProfileListData>,
        schemas::DataEnvelope<crate::web::routes::profiles::ProfileDetailData>,
        schemas::DataEnvelope<crate::web::routes::profiles::ProfileShareData>,
        schemas::DataEnvelope<crate::web::routes::profiles::ProfileImportData>,
        schemas::DataEnvelope<crate::web::routes::profiles::ProfileDetectData>,
        schemas::DataEnvelope<crate::web::routes::profiles::AutoSwitchData>,
        crate::web::routes::profiles::ProfileSummaryData,
        crate::web::routes::profiles::ProfileListData,
        crate::web::routes::profiles::ProfileDetailData,
        crate::web::routes::profiles::ProfileShareProfileData,
        crate::web::routes::profiles::ProfileShareData,
        crate::web::routes::profiles::ProfileImportData,
        crate::web::routes::profiles::ProfileDetectData,
        crate::web::routes::profiles::AutoSwitchData,
        crate::web::routes::profiles::ProfileCreateBody,
        crate::web::routes::profiles::ProfileUpdateBody,
        crate::web::routes::profiles::SwitchBody,
        crate::web::routes::profiles::AutoSwitchBody,
        // ---- profiles 域：配置会话（T3-3c）----
        schemas::DataEnvelope<crate::web::routes::profiles::ConfigSessionStartData>,
        schemas::DataEnvelope<crate::web::routes::profiles::ConfigSessionStopData>,
        crate::web::routes::profiles::ConfigSessionStartData,
        crate::web::routes::profiles::ConfigSessionStopData,
        crate::web::routes::profiles::ConfigSessionStartQuery,
        crate::web::routes::profiles::ConfigSessionStopQuery,
        // ---- login 域（T5-1a）----
        schemas::DataEnvelope<crate::web::routes::login::LoginTriggerData>,
        schemas::DataEnvelope<crate::web::routes::login::LoginOnceData>,
        crate::web::routes::login::LoginTriggerData,
        crate::web::routes::login::LoginOnceData,
        // ---- tasks 域（T5-1b）----
        schemas::DataEnvelope<Vec<crate::web::routes::tasks::TaskSummaryData>>,
        schemas::DataEnvelope<crate::web::routes::tasks::TaskDetailData>,
        schemas::DataEnvelope<crate::web::routes::tasks::TaskImportData>,
        schemas::DataEnvelope<crate::web::routes::tasks::TaskExecuteData>,
        crate::web::routes::tasks::TaskSummaryData,
        crate::web::routes::tasks::TaskDetailData,
        crate::web::routes::tasks::TaskImportData,
        crate::web::routes::tasks::TaskImportFailureData,
        crate::web::routes::tasks::TaskExecuteData,
        crate::web::routes::tasks::TaskCreateBody,
        crate::web::routes::tasks::OrderBody,
        // ---- http_tasks 域（T5-1b）----
        schemas::DataEnvelope<crate::web::routes::http_tasks::HttpTaskTestData>,
        crate::web::routes::http_tasks::HttpTaskTestData,
        crate::web::routes::http_tasks::HttpStepReportData,
        crate::web::routes::http_tasks::HttpTaskTestBody,
        // ---- scheduler 域（T5-1b）----
        schemas::DataEnvelope<Vec<crate::web::routes::scheduler::SchedulerJobData>>,
        schemas::DataEnvelope<crate::web::routes::scheduler::SchedulerToggleData>,
        schemas::DataEnvelope<Vec<crate::web::routes::scheduler::SchedulerHistoryData>>,
        crate::web::routes::scheduler::SchedulerJobData,
        crate::web::routes::scheduler::SchedulerDailySuccessData,
        crate::web::routes::scheduler::SchedulerToggleData,
        crate::web::routes::scheduler::SchedulerHistoryData,
        crate::web::routes::scheduler::JobCreateBody,
        crate::web::routes::scheduler::JobUpdateBody,
        // ---- scripts 域（T5-1b；run_script 结果复用 tasks 域 TaskExecuteData）----
        schemas::DataEnvelope<crate::web::routes::scripts::ScriptDetailData>,
        schemas::DataEnvelope<Vec<crate::web::routes::scripts::ScriptBinaryData>>,
        crate::web::routes::scripts::ScriptDetailData,
        crate::web::routes::scripts::ScriptBinaryData,
        crate::web::routes::scripts::RunScriptBody,
        // ---- repo 域（T5-1b；索引/任务为远程 JSON 透传，data 为任意值。
        //      注意 serde_json::Value 不能直接进 components（AnyValue 无名，
        //      生成代码会碎），只能作为泛型实参随信封注册）----
        schemas::DataEnvelope<serde_json::Value>,
        crate::web::routes::repo::RepoImageBody,
        // ---- monitor 域（批三；快照/探测结果是 Serialize 类型的线格式镜像）----
        schemas::DataEnvelope<crate::web::routes::monitor::MonitorStatusData>,
        schemas::DataEnvelope<crate::web::routes::monitor::TestNetworkResultData>,
        schemas::DataEnvelope<crate::web::routes::monitor::RedirectTestResultData>,
        schemas::DataEnvelope<Vec<crate::web::routes::monitor::NetworkInterfaceData>>,
        crate::web::routes::monitor::MonitorStatusData,
        crate::web::routes::monitor::MonitorConnectivityData,
        crate::web::routes::monitor::ProbeEvidenceData,
        crate::web::routes::monitor::TestNetworkResultData,
        crate::web::routes::monitor::ProbeDetailsData,
        crate::web::routes::monitor::RedirectTestResultData,
        crate::web::routes::monitor::RedirectTestStatus,
        crate::web::routes::monitor::NetworkInterfaceData,
        crate::web::routes::monitor::RedirectTestRequest,
        // ---- debug 域（批三；start/step/stop/run-all 为 Worker 协议透传，
        //      data 复用 Value 信封）----
        schemas::DataEnvelope<crate::web::routes::debug::DebugStatusData>,
        crate::web::routes::debug::DebugStatusData,
        crate::web::routes::debug::DebugScreenshotBody,
        crate::web::routes::debug::FeedbackBundleBody,
        // ---- history 域（批三；200 载体为 untagged 枚举 oneOf：裸数组 ⊕ 分页对象）----
        schemas::DataEnvelope<crate::web::routes::history::HistoryResponseData>,
        crate::web::routes::history::HistoryResponseData,
        crate::web::routes::history::HistoryPageData,
        crate::web::routes::history::HistoryItemData,
        // ---- uninstall 域（批三）----
        schemas::DataEnvelope<crate::web::routes::uninstall::UninstallDetectData>,
        schemas::DataEnvelope<crate::web::routes::uninstall::UninstallCleanupData>,
        schemas::DataEnvelope<crate::web::routes::uninstall::PurgeUninstallData>,
        crate::web::routes::uninstall::UninstallDetectData,
        crate::web::routes::uninstall::UninstallDetectItem,
        crate::web::routes::uninstall::UninstallEntryData,
        crate::web::routes::uninstall::UninstallDataDirData,
        crate::web::routes::uninstall::UninstallCleanupData,
        crate::web::routes::uninstall::UninstallStepData,
        crate::web::routes::uninstall::PurgeUninstallData,
        crate::web::routes::uninstall::UninstallPurgeDirData,
        crate::web::routes::uninstall::CleanupRequest,
        crate::web::routes::uninstall::PurgeRequest,
        // ---- autostart 域（批三；status 载荷因平台平铺字段而异，data 复用 Value 信封）----
        schemas::DataEnvelope<crate::web::routes::autostart::AutostartToggleData>,
        crate::web::routes::autostart::AutostartToggleData,
        // ---- ocr 域（批三）----
        schemas::DataEnvelope<crate::web::routes::ocr::OcrRecognizeData>,
        schemas::DataEnvelope<crate::web::routes::ocr::OcrStatusData>,
        schemas::DataEnvelope<crate::web::routes::ocr::OcrInstallData>,
        crate::web::routes::ocr::OcrRecognizeBody,
        crate::web::routes::ocr::OcrRecognizeData,
        crate::web::routes::ocr::OcrStatusData,
        crate::web::routes::ocr::OcrInstallData,
        // ---- ai 域（批三；捕获响应 data 复用 Value 信封，SSE 流帧只声明文本形态）----
        schemas::DataEnvelope<crate::web::routes::ai::LlmConfigData>,
        schemas::DataEnvelope<crate::web::routes::ai::LlmConnectionTestData>,
        schemas::DataEnvelope<crate::web::routes::ai::LlmModelsData>,
        schemas::DataEnvelope<crate::web::routes::ai::CaptureStatusData>,
        crate::web::routes::ai::LlmConfigData,
        crate::web::routes::ai::LlmConfigUpdateBody,
        crate::web::routes::ai::LlmConnectionTestData,
        crate::web::routes::ai::LlmModelsBody,
        crate::web::routes::ai::LlmModelsData,
        crate::web::routes::ai::CaptureBody,
        crate::web::routes::ai::CaptureStatusData,
        crate::web::routes::ai::GenerateStreamBody,
        crate::web::routes::ai::GenerateStreamFrame,
        crate::web::routes::ai::CaptureScreenshotBody,
        crate::web::routes::ai::CaptureBundleBody,
        // ---- background 域（批三；GET 200 与 multipart file 为 binary 载体）----
        schemas::DataEnvelope<crate::web::routes::background::BackgroundUploadData>,
        crate::web::routes::background::BackgroundImageBody,
        crate::web::routes::background::BackgroundUploadFile,
        crate::web::routes::background::BackgroundUploadData,
        crate::web::routes::background::BackgroundFetchBody,
        // ---- tools 域（批三；200 为 application/javascript 文本原文，无本地 DTO）----
        // ---- notification 域（T5-1 收口；通道配置草稿数组项为 config 持久化树
        //      形状（serde tagged），未接入 ToSchema 体系，以对象数组标注）----
        schemas::DataEnvelope<crate::web::routes::notification::NotificationTestData>,
        crate::web::routes::notification::NotificationTestData,
        crate::web::routes::notification::NotificationTestChannelResult,
        crate::web::routes::notification::NotificationTestBody,
        // ---- diagnostics 域（T5-1 收口；报告为 DiagnosticReport 线格式镜像，
        //      bundle 为 zip 二进制附件）----
        schemas::DataEnvelope<crate::web::routes::diagnostics::DiagnosticReportData>,
        crate::web::routes::diagnostics::DiagnosticReportData,
        crate::web::routes::diagnostics::DiagnosticCheckData,
        crate::web::routes::diagnostics::DiagnosticsBundleBody,
        // ---- ws 域（T5-4；/ws/logs 推送信封。WebSocket 升级端点无 #[utoipa::path]
        //      注解，路径条目维持手写 baseline；本节仅注册推送消息契约。
        //      log/status 载荷为既有 Serialize 类型的线格式镜像——status 复用
        //      monitor 域 MonitorStatusData，log 镜像定义在 ws.rs 并有漂移守卫测试；
        //      Bridge 事件（screenshot/step_progress 等）为动态 JSON，不经信封）----
        crate::web::ws::WsMessage,
        crate::web::ws::WsLogEntryData,
    )),
)]
pub struct ApiDoc;

/// 生成并合并最终 OpenAPI 文档：utoipa 生成 spec ⊕ 手写 baseline
///
/// 合并规则：
/// - 根字段（`openapi`/`info`/`servers`/`components`）整体由生成侧接管；
/// - `paths` 逐路径合并：生成侧条目（已注解域）**整体替换**手写条目，
///   未注解域保留手写条目原样。
pub fn merged_openapi_json() -> serde_json::Value {
    let generated = serde_json::to_value(ApiDoc::openapi()).expect("utoipa 文档应可序列化为 JSON");
    let mut merged: serde_json::Value = serde_json::from_str(include_str!("../../openapi.json"))
        .expect("根目录 openapi.json 应为合法 JSON");
    let root = merged
        .as_object_mut()
        .expect("openapi.json 顶层应为 JSON 对象");

    // 根字段：生成侧接管（生成侧缺省的键不强插，保留 baseline 原值兜底）
    for key in ["openapi", "info", "servers", "components"] {
        if let Some(value) = generated.get(key) {
            root.insert(key.to_string(), value.clone());
        }
    }
    // paths：逐路径覆盖，未注解域条目保留手写原文
    if let Some(generated_paths) = generated.get("paths").and_then(|p| p.as_object()) {
        let paths = root
            .entry("paths")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("paths 应为 JSON 对象");
        for (path, item) in generated_paths {
            paths.insert(path.clone(), item.clone());
        }
    }
    merged
}

/// 以与 baseline 一致的版式渲染快照：1 空格缩进 + 文件尾换行
///
/// serde_json 默认 pretty 是 2 空格；用自定义 formatter 对齐历史版式，
/// 避免重生成时产生全文件缩进噪音 diff。当前仅生成测试使用（快照落盘
/// 由 `UPDATE_OPENAPI=1` 驱动），故随测试编译。
#[cfg(test)]
fn render_snapshot(spec: &serde_json::Value) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    spec.serialize(&mut serializer).expect("Value 序列化不失败");
    buf.push(b'\n');
    String::from_utf8(buf).expect("快照应为 UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 spec（合并后）必须覆盖全部路由表 method+path
    ///
    /// 与 `web::mod` 的 `openapi_json_matches_route_table`（文件级）互补：
    /// 本测试校验**生成管线**的不变量——即便后续批次把更多域改为生成源，
    /// 合并结果也必须完整覆盖路由表；文件只在其被重写（UPDATE_OPENAPI=1）
    /// 时才反映最新生成结果。
    #[test]
    fn merged_spec_covers_route_table() {
        let spec = merged_openapi_json();
        const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

        let mut spec_set = std::collections::BTreeSet::new();
        let paths = spec
            .get("paths")
            .and_then(|p| p.as_object())
            .expect("合并 spec 应含 paths");
        for (path, ops) in paths {
            if !path.starts_with("/api/") && path != "/api" {
                continue;
            }
            for method in ops
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
                .filter(|k| METHODS.contains(&k.as_str()))
            {
                spec_set.insert(format!("{} {}", method.to_uppercase(), path));
            }
        }

        for (method, path, _) in super::super::route_table() {
            assert!(
                spec_set.contains(&format!("{method} {path}")),
                "路由表条目 {method} {path} 未被合并 spec 覆盖"
            );
        }
    }

    /// 生成结果确定性：同版本代码重复生成必须逐字节一致（防抖动 diff）
    #[test]
    fn generation_is_deterministic() {
        let first = merged_openapi_json();
        let second = merged_openapi_json();
        assert_eq!(
            render_snapshot(&first),
            render_snapshot(&second),
            "两次生成的快照必须逐字节一致"
        );
    }

    /// 样板域条目必须是生成源，未注解域保留手写条目
    ///
    /// 生成源判据：响应携带真实 schema（`$ref` / `properties`，非 `{}` 空占位）；
    /// 反例取 browsers 域（尚未注解），其条目应原样保留 baseline 的 `{}` 占位。
    /// 该测试钉住「合并真的发生了」——若 paths 覆盖逻辑退化为纯 baseline，
    /// 样板域 schema 会消失、测试即失败。
    #[test]
    fn sample_domain_entries_come_from_generated_source() {
        let spec = merged_openapi_json();

        let health_schema = &spec["paths"]["/api/health"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert!(
            health_schema.is_object() && health_schema.as_object().is_some_and(|o| !o.is_empty()),
            "样板域 /api/health 应携带生成 schema，实际 {health_schema}"
        );

        let browsers_schema = &spec["paths"]["/api/browsers"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        assert!(
            browsers_schema.is_object()
                && browsers_schema.as_object().is_some_and(|o| o.is_empty()),
            "未注解域 /api/browsers 应保留手写占位 schema，实际 {browsers_schema}"
        );

        // 错误信封与 data 包装必须进入 components（共享 schema 模块的出口）
        let schemas = spec
            .pointer("/components/schemas")
            .and_then(|s| s.as_object())
            .expect("合并 spec 应含 components.schemas");
        assert!(
            schemas.keys().any(|k| k.starts_with("ErrorResponse")),
            "错误信封应注册到 components，实际键: {:?}",
            schemas.keys().collect::<Vec<_>>()
        );
        assert!(
            schemas.keys().any(|k| k.starts_with("DataEnvelope")),
            "data 包装应注册到 components，实际键: {:?}",
            schemas.keys().collect::<Vec<_>>()
        );
    }

    /// 生成快照保护测试：committed 的 openapi.json 必须与生成产物逐字节一致
    ///
    /// 日常 `cargo test`（未设环境变量）校验文件与生成产物一致，不一致即失败
    /// 并提示重写命令；`UPDATE_OPENAPI=1 cargo test` 时重写文件并失败退出，
    /// 防止环境变量残留导致 CI 静默改文件。重写后请人工核对 diff 再提交。
    #[test]
    fn openapi_json_snapshot_matches_generated() {
        let rendered = render_snapshot(&merged_openapi_json());
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
        let current = std::fs::read_to_string(&file).unwrap_or_else(|e| {
            panic!("读取 {} 失败: {e}", file.display());
        });
        if std::env::var("UPDATE_OPENAPI").as_deref() == Ok("1") {
            std::fs::write(&file, &rendered).expect("写入 openapi.json 失败");
            panic!(
                "openapi.json 已按生成产物重写（{rendered} 字节），请核对 diff 后提交；\
                 未设 UPDATE_OPENAPI 时本测试应直接通过"
            );
        }
        assert_eq!(
            current, rendered,
            "openapi.json 与生成产物不一致（路由或注解变更后请运行 \
             UPDATE_OPENAPI=1 cargo test 重新生成并核对 diff）"
        );
    }
}
