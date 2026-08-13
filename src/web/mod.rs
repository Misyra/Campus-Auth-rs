//! Web 模块：Axum Router 构建
//!
//! 负责组装所有 HTTP 路由、WebSocket 端点与静态文件服务。
//! 路由路径严格对齐 `rust-rewrite/architecture/data-models.md` 附录 A。

pub mod error;
mod routes;
mod static_files;
pub mod state;
mod ws;

use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

use self::state::AppState;

/// 构建完整的 Axum Router（含嵌入前端回退）
pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        // ---- 监控（monitor）----
        .route("/api/monitor/status", get(routes::monitor::get_status))
        .route("/api/monitor/test", post(routes::monitor::test_network))
        .route("/api/monitor/start", post(routes::monitor::start_monitor))
        .route("/api/monitor/stop", post(routes::monitor::stop_monitor))
        // ---- 配置（config）----
        .route("/api/config", get(routes::config::get_settings))
        .route("/api/config", put(routes::config::put_settings))
        .route("/api/config", patch(routes::config::patch_settings))
        .route("/api/config/reload", post(routes::config::reload_settings))
        .route(
            "/api/config/defaults",
            get(routes::config::get_config_defaults),
        )
        .route(
            "/api/config/log-levels",
            get(routes::config::get_log_levels),
        )
        .route(
            "/api/config/log-level",
            put(routes::config::set_log_level),
        )
        .route(
            "/api/config/default-stealth-script",
            get(routes::config::get_default_stealth_script),
        )
        // ---- 纯净模式（pure-mode，读写 config.browser.pure_mode）----
        .route("/api/pure-mode", get(routes::config::get_pure_mode))
        .route("/api/pure-mode", post(routes::config::set_pure_mode))
        // ---- Profile ----
        .route("/api/profiles", get(routes::profiles::list_profiles))
        .route("/api/profiles/{id}", get(routes::profiles::get_profile))
        .route("/api/profiles/{id}", post(routes::profiles::create_profile))
        .route("/api/profiles/{id}", put(routes::profiles::update_profile))
        .route(
            "/api/profiles/{id}",
            delete(routes::profiles::delete_profile),
        )
        .route(
            "/api/profiles/switch",
            post(routes::profiles::switch_profile),
        )
        .route(
            "/api/profiles/detect",
            post(routes::profiles::detect_profile),
        )
        .route(
            "/api/profiles/auto-switch",
            post(routes::profiles::auto_switch),
        )
        // ---- 任务（tasks）----
        .route("/api/tasks", get(routes::tasks::list_tasks))
        .route("/api/tasks", post(routes::tasks::create_task))
        .route("/api/tasks/active", get(routes::tasks::get_active_task))
        .route(
            "/api/tasks/active/{task_id}",
            post(routes::tasks::set_active_task),
        )
        .route("/api/tasks/import", post(routes::tasks::import_tasks))
        .route("/api/tasks/order", post(routes::tasks::order_tasks))
        .route("/api/tasks/export/{id}", get(routes::tasks::export_task))
        .route("/api/tasks/{id}", get(routes::tasks::get_task))
        .route("/api/tasks/{id}", put(routes::tasks::update_task))
        .route("/api/tasks/{id}", delete(routes::tasks::delete_task))
        .route("/api/tasks/{id}/execute", post(routes::tasks::execute_task))
        // ---- 仓库（repo）----
        .route("/api/repo/fetch", get(routes::repo::repo_fetch_index))
        .route("/api/repo/task", get(routes::repo::repo_fetch_task))
        // ---- 登录（login）----
        .route("/api/login", post(routes::login::trigger_login))
        .route("/api/login/cancel", post(routes::login::cancel_login))
        .route("/api/login/status", get(routes::login::get_login_status))
        .route("/api/login/once", post(routes::login::login_once))
        // ---- 调试（debug）----
        .route("/api/debug/start", post(routes::debug::start_debug))
        .route("/api/debug/step", post(routes::debug::step_debug))
        .route("/api/debug/stop", post(routes::debug::stop_debug))
        .route("/api/debug/run-all", post(routes::debug::run_all))
        // ---- 调度（scheduler）----
        .route("/api/scheduler/jobs", get(routes::scheduler::list_jobs))
        .route("/api/scheduler/jobs", post(routes::scheduler::create_job))
        .route("/api/scheduler/jobs/{id}", get(routes::scheduler::get_job))
        .route(
            "/api/scheduler/jobs/{id}",
            put(routes::scheduler::update_job),
        )
        .route(
            "/api/scheduler/jobs/{id}",
            delete(routes::scheduler::delete_job),
        )
        .route(
            "/api/scheduler/jobs/{id}/toggle",
            post(routes::scheduler::toggle_job),
        )
        .route(
            "/api/scheduler/jobs/{id}/run",
            post(routes::scheduler::run_job),
        )
        .route(
            "/api/scheduler/jobs/{id}/history",
            get(routes::scheduler::job_history),
        )
        // ---- 历史（history）----
        .route("/api/history", get(routes::history::get_history))
        .route("/api/history", delete(routes::history::clear_history))
        // ---- 系统（system）----
        .route("/api/system/info", get(routes::system::system_info))
        .route("/api/system/shutdown", post(routes::system::shutdown_app))
        .route("/api/system/restart", post(routes::system::restart_app))
        .route("/api/system/update", post(routes::system::apply_update))
        .route("/api/check-update", get(routes::system::check_update))
        .route("/api/health", get(routes::system::health_check))
        .route("/api/init-status", get(routes::system::init_status))
        .route("/api/agree", post(routes::system::agree_terms))
        .route("/api/logs", get(routes::system::fetch_logs))
        // ---- 浏览器与安装（browsers / install / worker）----
        .route("/api/browsers", get(routes::system::list_browsers))
        .route("/api/worker/stop", post(routes::system::stop_worker))
        .route(
            "/api/install/playwright",
            post(routes::system::install_playwright),
        )
        // ---- 图标（icons）----
        .route("/api/icons", get(routes::system::list_icons))
        // ---- 卸载（uninstall）----
        .route(
            "/api/uninstall/detect",
            get(routes::system::detect_uninstall),
        )
        .route("/api/uninstall", post(routes::system::uninstall))
        // ---- 背景图（background）----
        .route(
            "/api/background/{filename}",
            get(routes::system::get_background),
        )
        .route(
            "/api/background/upload",
            post(routes::system::upload_background),
        )
        .route(
            "/api/background/fetch-url",
            post(routes::system::fetch_url_background),
        )
        .route(
            "/api/background/{filename}",
            delete(routes::system::delete_background),
        )
        // ---- 文档（docs）----
        .route(
            "/api/docs/task-writing-guide",
            get(routes::system::task_writing_guide),
        )
        .route("/api/docs/task-manual", get(routes::system::task_manual))
        // ---- 自启动（autostart）----
        .route(
            "/api/autostart/status",
            get(routes::autostart::get_autostart),
        )
        .route(
            "/api/autostart/enable",
            post(routes::autostart::enable_autostart),
        )
        .route(
            "/api/autostart/disable",
            post(routes::autostart::disable_autostart),
        )
        .route(
            "/api/autostart/mode",
            post(routes::autostart::set_autostart_mode),
        )
        // ---- OCR ----
        .route("/api/ocr/recognize", post(routes::ocr::ocr_recognize))
        .route("/api/ocr/status", get(routes::ocr::ocr_status))
        .route("/api/ocr/install", post(routes::ocr::ocr_install))
        .route("/api/ocr/uninstall", post(routes::ocr::ocr_uninstall))
        // ---- 脚本（scripts）----
        .route("/api/scripts", get(routes::scripts::list_scripts))
        .route("/api/scripts/run", post(routes::scripts::run_script))
        .route(
            "/api/scripts/binaries",
            get(routes::scripts::list_binaries),
        )
        .route("/api/scripts/{task_id}", get(routes::scripts::get_script))
        .route(
            "/api/scripts/{task_id}",
            put(routes::scripts::update_script),
        )
        .route(
            "/api/scripts/{task_id}",
            delete(routes::scripts::delete_script),
        )
        .route("/api/shells", get(routes::scripts::list_shells))
        // ---- 工具（tools）----
        .route(
            "/api/tools/network-interfaces",
            get(routes::monitor::list_network_interfaces),
        )
        .route(
            "/api/tools/task-recorder.user.js",
            get(routes::tools::task_recorder),
        );

    // CORS：镜像请求 Origin，彻底解除端口耦合（本地前端任意端口均可访问）
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    // Gzip 压缩
    let compression = CompressionLayer::new();

    Router::new()
        .merge(api)
        // WebSocket
        .route("/ws/logs", get(ws::logs_handler))
        // 静态文件（所有未匹配路由 → SPA 回退）
        .fallback(static_files::handler)
        .layer(compression)
        .layer(cors)
        .with_state(state)
}
