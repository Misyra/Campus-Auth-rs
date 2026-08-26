//! Web 模块：Axum Router 构建
//!
//! 负责组装所有 HTTP 路由、WebSocket 端点与静态文件服务。
//! `/api/*` 路由以 [`route_table`] 为单一来源声明式注册，
//! 契约测试据此与根目录 `openapi.json` 做双向一致性校验。

pub mod auth;
pub mod error;
mod routes;
mod ssrf;
pub mod state;
mod static_files;
mod ws;

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, Request, header};
use axum::middleware;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::MethodRouter;
use axum::routing::{delete, get, patch, post, put};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

use self::state::AppState;

/// 路由处理器构造器（非捕获闭包，可强转为 fn pointer）
type RouteBuilder = fn() -> MethodRouter<AppState>;

/// `/api/*` 路由表：method + path + handler 构造器
///
/// 单一来源，两处消费：
/// - [`build_router`] 遍历注册到 Router；
/// - 契约测试（本模块 tests）遍历与 `openapi.json` 双向比对，
///   新增/改名路由而漏改 openapi.json 时测试即失败。
///
/// 注意：同一路径多 method 拆为多行（axum 对同 path 不同 method 的
/// 重复 `route()` 调用自动合并）。
fn route_table() -> Vec<(&'static str, &'static str, RouteBuilder)> {
    vec![
        // ---- 监控（monitor）----
        ("GET", "/api/monitor/status", || {
            get(routes::monitor::get_status)
        }),
        ("POST", "/api/monitor/test", || {
            post(routes::monitor::test_network)
        }),
        ("POST", "/api/monitor/start", || {
            post(routes::monitor::start_monitor)
        }),
        ("POST", "/api/monitor/stop", || {
            post(routes::monitor::stop_monitor)
        }),
        // ---- 配置（config）----
        ("GET", "/api/config", || get(routes::config::get_settings)),
        ("PUT", "/api/config", || put(routes::config::put_settings)),
        ("PATCH", "/api/config", || {
            patch(routes::config::patch_settings)
        }),
        ("POST", "/api/config/reload", || {
            post(routes::config::reload_settings)
        }),
        ("GET", "/api/config/defaults", || {
            get(routes::config::get_config_defaults)
        }),
        ("GET", "/api/config/log-levels", || {
            get(routes::config::get_log_levels)
        }),
        ("PUT", "/api/config/log-level", || {
            put(routes::config::set_log_level)
        }),
        ("GET", "/api/config/default-stealth-script", || {
            get(routes::config::get_default_stealth_script)
        }),
        // ---- 纯净模式（pure-mode，读写 config.browser.pure_mode）----
        ("GET", "/api/pure-mode", || {
            get(routes::config::get_pure_mode)
        }),
        ("POST", "/api/pure-mode", || {
            post(routes::config::set_pure_mode)
        }),
        // ---- Profile ----
        ("GET", "/api/profiles", || {
            get(routes::profiles::list_profiles)
        }),
        ("GET", "/api/profiles/{id}", || {
            get(routes::profiles::get_profile)
        }),
        ("POST", "/api/profiles/{id}", || {
            post(routes::profiles::create_profile)
        }),
        ("PUT", "/api/profiles/{id}", || {
            put(routes::profiles::update_profile)
        }),
        ("DELETE", "/api/profiles/{id}", || {
            delete(routes::profiles::delete_profile)
        }),
        ("POST", "/api/profiles/switch", || {
            post(routes::profiles::switch_profile)
        }),
        ("POST", "/api/profiles/detect", || {
            post(routes::profiles::detect_profile)
        }),
        ("POST", "/api/profiles/auto-switch", || {
            post(routes::profiles::auto_switch)
        }),
        // ---- 任务（tasks）----
        ("GET", "/api/tasks", || get(routes::tasks::list_tasks)),
        ("POST", "/api/tasks", || post(routes::tasks::create_task)),
        ("GET", "/api/tasks/active", || {
            get(routes::tasks::get_active_task)
        }),
        ("POST", "/api/tasks/active/{task_id}", || {
            post(routes::tasks::set_active_task)
        }),
        ("POST", "/api/tasks/import", || {
            post(routes::tasks::import_tasks)
        }),
        ("POST", "/api/tasks/order", || {
            post(routes::tasks::order_tasks)
        }),
        ("GET", "/api/tasks/export/{id}", || {
            get(routes::tasks::export_task)
        }),
        ("GET", "/api/tasks/{id}", || get(routes::tasks::get_task)),
        ("PUT", "/api/tasks/{id}", || put(routes::tasks::update_task)),
        ("DELETE", "/api/tasks/{id}", || {
            delete(routes::tasks::delete_task)
        }),
        ("POST", "/api/tasks/{id}/execute", || {
            post(routes::tasks::execute_task)
        }),
        // ---- 仓库（repo）----
        ("GET", "/api/repo/fetch", || {
            get(routes::repo::repo_fetch_index)
        }),
        ("GET", "/api/repo/task", || {
            get(routes::repo::repo_fetch_task)
        }),
        // ---- 登录（login）----
        ("POST", "/api/login", || post(routes::login::trigger_login)),
        ("POST", "/api/login/cancel", || {
            post(routes::login::cancel_login)
        }),
        ("GET", "/api/login/status", || {
            get(routes::login::get_login_status)
        }),
        ("POST", "/api/login/once", || {
            post(routes::login::login_once)
        }),
        // ---- 调试（debug）----
        ("POST", "/api/debug/start", || {
            post(routes::debug::start_debug)
        }),
        ("POST", "/api/debug/step", || {
            post(routes::debug::step_debug)
        }),
        ("POST", "/api/debug/stop", || {
            post(routes::debug::stop_debug)
        }),
        ("POST", "/api/debug/run-all", || {
            post(routes::debug::run_all)
        }),
        // ---- 调度（scheduler）----
        ("GET", "/api/scheduler/jobs", || {
            get(routes::scheduler::list_jobs)
        }),
        ("POST", "/api/scheduler/jobs", || {
            post(routes::scheduler::create_job)
        }),
        ("GET", "/api/scheduler/jobs/{id}", || {
            get(routes::scheduler::get_job)
        }),
        ("PUT", "/api/scheduler/jobs/{id}", || {
            put(routes::scheduler::update_job)
        }),
        ("DELETE", "/api/scheduler/jobs/{id}", || {
            delete(routes::scheduler::delete_job)
        }),
        ("POST", "/api/scheduler/jobs/{id}/toggle", || {
            post(routes::scheduler::toggle_job)
        }),
        ("POST", "/api/scheduler/jobs/{id}/run", || {
            post(routes::scheduler::run_job)
        }),
        ("GET", "/api/scheduler/jobs/{id}/history", || {
            get(routes::scheduler::job_history)
        }),
        // ---- 历史（history）----
        ("GET", "/api/history", || get(routes::history::get_history)),
        ("DELETE", "/api/history", || {
            delete(routes::history::clear_history)
        }),
        // ---- 系统（system）----
        ("GET", "/api/system/info", || {
            get(routes::system::system_info)
        }),
        ("POST", "/api/system/shutdown", || {
            post(routes::system::shutdown_app)
        }),
        ("POST", "/api/system/restart", || {
            post(routes::system::restart_app)
        }),
        ("POST", "/api/system/update", || {
            post(routes::system::apply_update)
        }),
        ("GET", "/api/check-update", || {
            get(routes::system::check_update)
        }),
        ("GET", "/api/health", || get(routes::system::health_check)),
        ("GET", "/api/init-status", || {
            get(routes::system::init_status)
        }),
        ("POST", "/api/agree", || post(routes::system::agree_terms)),
        ("GET", "/api/logs", || get(routes::system::fetch_logs)),
        // ---- 浏览器与安装（browsers / install / worker）----
        ("GET", "/api/browsers", || {
            get(routes::system::list_browsers)
        }),
        ("POST", "/api/worker/stop", || {
            post(routes::system::stop_worker)
        }),
        ("POST", "/api/install/playwright", || {
            post(routes::system::install_playwright)
        }),
        // ---- 图标（icons）----
        ("GET", "/api/icons", || get(routes::system::list_icons)),
        // ---- 卸载（uninstall）----
        ("GET", "/api/uninstall/detect", || {
            get(routes::uninstall::detect_uninstall)
        }),
        ("POST", "/api/uninstall", || post(routes::uninstall::uninstall)),
        // ---- 背景图（background）----
        ("GET", "/api/background/{filename}", || {
            get(routes::background::get_background)
        }),
        ("POST", "/api/background/upload", || {
            post(routes::background::upload_background).layer(DefaultBodyLimit::max(
                routes::background::BACKGROUND_UPLOAD_BODY_LIMIT,
            ))
        }),
        ("POST", "/api/background/fetch-url", || {
            post(routes::background::fetch_url_background)
        }),
        ("DELETE", "/api/background/{filename}", || {
            delete(routes::background::delete_background)
        }),
        // ---- 文档（docs）----
        ("GET", "/api/docs/task-writing-guide", || {
            get(routes::system::task_writing_guide)
        }),
        ("GET", "/api/docs/task-manual", || {
            get(routes::system::task_manual)
        }),
        // ---- 自启动（autostart）----
        ("GET", "/api/autostart/status", || {
            get(routes::autostart::get_autostart)
        }),
        ("POST", "/api/autostart/enable", || {
            post(routes::autostart::enable_autostart)
        }),
        ("POST", "/api/autostart/disable", || {
            post(routes::autostart::disable_autostart)
        }),
        ("POST", "/api/autostart/mode", || {
            post(routes::autostart::set_autostart_mode)
        }),
        // ---- OCR ----
        // recognize 单独放宽请求体限制（见 routes::ocr::RECOGNIZE_BODY_LIMIT），
        // 避免 >1.5MB 原图 base64 后触发 axum 默认 2MB 上限的 413
        ("POST", "/api/ocr/recognize", || {
            post(routes::ocr::ocr_recognize)
                .layer(DefaultBodyLimit::max(routes::ocr::RECOGNIZE_BODY_LIMIT))
        }),
        ("GET", "/api/ocr/status", || get(routes::ocr::ocr_status)),
        ("POST", "/api/ocr/install", || {
            post(routes::ocr::ocr_install)
        }),
        ("POST", "/api/ocr/uninstall", || {
            post(routes::ocr::ocr_uninstall)
        }),
        // ---- 脚本（scripts）----
        ("GET", "/api/scripts", || get(routes::scripts::list_scripts)),
        ("POST", "/api/scripts/run", || {
            post(routes::scripts::run_script)
        }),
        ("GET", "/api/scripts/binaries", || {
            get(routes::scripts::list_binaries)
        }),
        ("GET", "/api/scripts/{task_id}", || {
            get(routes::scripts::get_script)
        }),
        ("PUT", "/api/scripts/{task_id}", || {
            put(routes::scripts::update_script)
        }),
        ("DELETE", "/api/scripts/{task_id}", || {
            delete(routes::scripts::delete_script)
        }),
        ("GET", "/api/shells", || get(routes::scripts::list_shells)),
        // ---- 工具（tools）----
        ("GET", "/api/tools/network-interfaces", || {
            get(routes::monitor::list_network_interfaces)
        }),
        ("GET", "/api/tools/task-recorder.user.js", || {
            get(routes::tools::task_recorder)
        }),
        // ---- 鉴权（auth）----
        // token 发放端点：响应读取受 CORS 保护（仅 localhost Origin 可读），
        // 跨域恶意网页无法获取 token，中间件对此路径豁免
        ("GET", "/api/auth/token", || get(auth::token_handler)),
    ]
}

/// 为本地管理界面统一附加浏览器安全响应头。
async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws://127.0.0.1:* ws://localhost:*; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// 构建完整的 Axum Router（含嵌入前端回退）
pub fn build_router(state: AppState) -> Router {
    let mut api = Router::new();
    for (_method, path, build) in route_table() {
        api = api.route(path, build());
    }

    // CORS：仅放行本机 Origin（开发期 Vite dev server 与生产同源均覆盖）。
    // 此前使用 mirror_request 镜像任意 Origin，等于允许任意网站跨域读写
    // 本地 API（配合无鉴权可触发删除任务、关闭应用等危险操作）。
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let s = origin.to_str().unwrap_or("");
            s.starts_with("http://127.0.0.1:")
                || s.starts_with("http://localhost:")
                || s.starts_with("http://[::1]:")
        }))
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    // Gzip 压缩
    let compression = CompressionLayer::new();

    Router::new()
        .merge(api)
        // WebSocket
        .route("/ws/logs", get(ws::logs_handler))
        // openapi.json（嵌入，生产可用）
        .route("/openapi.json", get(static_files::openapi_handler))
        // 静态文件（所有未匹配路由 → SPA 回退）
        .fallback(static_files::handler)
        // 本地 API 鉴权：所有 /api/* 与 /ws/* 请求必须携带有效 token，
        // 防止本地恶意网页（CSRF）与其他进程调用危险接口
        .layer(middleware::from_fn_with_state(
            state.auth_token.clone(),
            auth::auth_middleware,
        ))
        .layer(middleware::from_fn(security_headers))
        .layer(compression)
        .layer(cors)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径参数名归一化：`/api/tasks/{id}` 与 `/api/tasks/{task_id}` 等价
    ///（openapi.json 沿用历史参数名，路由侧参数名可独立演化）
    fn normalize_path(path: &str) -> String {
        let mut out = String::with_capacity(path.len());
        let mut in_brace = false;
        for c in path.chars() {
            match c {
                '{' => {
                    out.push('{');
                    in_brace = true;
                }
                '}' => {
                    out.push('}');
                    in_brace = false;
                }
                _ if !in_brace => out.push(c),
                _ => {}
            }
        }
        out
    }

    /// 契约校验：route_table 与根目录 openapi.json 的 /api/* 路径双向一致
    ///
    /// 任一侧新增/改名/遗漏路由时此测试失败，消除"改代码漏改文档"的
    /// 三处手工同步问题（路由 ↔ openapi.json ↔ 前端 types）
    #[test]
    fn openapi_json_matches_route_table() {
        let spec: serde_json::Value = serde_json::from_str(include_str!("../../openapi.json"))
            .expect("openapi.json 应为合法 JSON");
        const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

        let mut spec_set = std::collections::BTreeSet::new();
        let paths = spec
            .get("paths")
            .and_then(|p| p.as_object())
            .expect("openapi.json 应含 paths 对象");
        for (path, ops) in paths {
            // 只校验 /api/*（/ws、/openapi.json 等非本表管辖）
            if !path.starts_with("/api/") && path != "/api" {
                continue;
            }
            for method in ops
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
                .filter(|k| METHODS.contains(&k.as_str()))
            {
                spec_set.insert(format!(
                    "{} {}",
                    method.to_uppercase(),
                    normalize_path(path)
                ));
            }
        }

        let mut table_set = std::collections::BTreeSet::new();
        for (method, path, _) in route_table() {
            table_set.insert(format!("{} {}", method, normalize_path(path)));
        }

        let spec_only: Vec<_> = spec_set.difference(&table_set).collect();
        let table_only: Vec<_> = table_set.difference(&spec_set).collect();
        assert!(
            spec_only.is_empty(),
            "openapi.json 声明但路由表缺失（文档过期或路由遗漏）: {spec_only:?}"
        );
        assert!(
            table_only.is_empty(),
            "路由表存在但 openapi.json 未声明（需同步 openapi.json）: {table_only:?}"
        );
    }
}
