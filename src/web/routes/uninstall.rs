//! 卸载路由：卸载检测、系统残留清理与「真卸载」（A-5 自 system.rs 拆出）
//!
//! 卸载分两步，**职责不重叠**：
//!
//! 1. `POST /api/uninstall` —— 清理 `base_path` **之外**的系统残留：用户数据目录
//!    （`~/.campus_network_auth`）、开机自启动注册、Playwright 浏览器缓存。这些在
//!    进程内就能删，且删完程序仍然可用（"只清缓存不卸载"也是合法用法）。
//!    带可选 body `{ keep_user_data }`：勾选「保留配置与任务」时**保留密钥目录**——
//!    保留下来的 `config/` 里方案密码是密文，删掉密钥等于把那些密码一起废掉。
//! 2. `POST /api/uninstall/purge` —— 删程序本身：程序目录（含 `resources/` `docs/`
//!    `python_worker/` 与发布包附带的源码副本）与（可选的）`base_path` 下的用户数据。
//!    **运行中的 exe 无法删除自己**，故这一步交给 `campus-auth-helper --uninstall`：
//!    主进程先把待应用更新取消掉、spawn 助手、再优雅退出，助手等主进程退出后执行删除。
//!
//! 删除清单与守卫（含"拒绝把源码仓库当安装目录"）在 `crate::uninstall`（**单一事实源**）：
//! 本模块据此渲染界面清单，助手据此执行，两侧不会漂移。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};
use crate::web::state::AppState;

/// 用户数据目录（密码加密密钥等）：`~/.campus_network_auth`
fn user_data_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".campus_network_auth"))
}

/// Playwright 浏览器缓存目录：`PLAYWRIGHT_BROWSERS_PATH`（"0" 表示随包内联，
/// 无独立缓存）> 各 OS 默认 ms-playwright 目录。
/// 返回 (目录, 是否来自环境变量)；来自环境变量时目录可能承载用户其它内容，
/// 删除时只清理浏览器前缀子目录而非整个目录。
fn playwright_cache_dir() -> Option<(PathBuf, bool)> {
    if let Some(v) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        if v.is_empty() || v == *"0" {
            return None;
        }
        return Some((PathBuf::from(v), true));
    }
    let dir = default_playwright_cache_dir()?;
    Some((dir, false))
}

/// 各 OS 的 Playwright 默认缓存目录（与 environment::bootstrap 探测逻辑同源）
fn default_playwright_cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ms-playwright"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Caches")
                .join("ms-playwright")
        })
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("ms-playwright"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Playwright 管理产物的目录名前缀（浏览器 + ffmpeg）
const PW_BROWSER_PREFIXES: [&str; 4] = ["chromium-", "firefox-", "webkit-", "ffmpeg-"];

/// 删除目录（递归）；不存在视为成功（幂等）
fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("删除失败: {e}"))
}

/// 删除 Playwright 浏览器缓存
///
/// 默认缓存目录整体删除；`PLAYWRIGHT_BROWSERS_PATH` 指定的自定义目录只删除
/// 浏览器前缀子目录（目录本身可能被用户复用，不能整删）。
fn remove_playwright_cache(path: &Path, from_env: bool) -> Result<String, String> {
    if !path.exists() {
        return Ok("目录不存在，已跳过".to_string());
    }
    if !from_env {
        return remove_dir_if_exists(path).map(|_| "已删除".to_string());
    }
    let entries = std::fs::read_dir(path).map_err(|e| format!("读取目录失败: {e}"))?;
    let (mut removed, mut failed) = (0u32, 0u32);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if PW_BROWSER_PREFIXES.iter().any(|p| name_str.starts_with(p)) {
            if std::fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            } else {
                failed += 1;
            }
        }
    }
    if failed > 0 {
        Err(format!(
            "已删除 {removed} 项，{failed} 项删除失败（浏览器可能正在运行）"
        ))
    } else if removed == 0 {
        Ok("未发现浏览器缓存，已跳过".to_string())
    } else {
        Ok(format!("已删除 {removed} 项浏览器缓存"))
    }
}

/// 组装卸载清单：程序目录 + 用户数据目录（系统残留见 `detect` 的 `items`）
///
/// 抽成纯函数：单测可构造临时目录断言清单形状，不必真的去卸载。
/// 清单要**逐项**给出路径与存在性——用户拍板的口径是"删除时必须明确列出会删哪些内容"，
/// 一个笼统的"程序目录"远不够。
pub fn build_inventory(base_path: &Path, install_dir: &Path) -> Value {
    let data: Vec<Value> = crate::uninstall::DATA_DIR_NAMES
        .iter()
        .map(|name| {
            let path = base_path.join(name);
            serde_json::json!({
                "key": name,
                "label": crate::uninstall::data_dir_label(name),
                "path": path.to_string_lossy(),
                "exists": path.exists(),
            })
        })
        .collect();
    serde_json::json!({
        "program": {
            "label": "程序目录",
            "path": install_dir.to_string_lossy(),
            "exists": install_dir.is_dir(),
        },
        // 卸载助手是否在位：它不在时 `purge` 一定失败（spawn 不出来），而那时可能已经
        // 清掉了系统残留——界面据此先把按钮拦下，别让用户白删一轮
        "helper": {
            "label": "卸载助手",
            "path": install_dir
                .join(crate::uninstall::helper_exe_name())
                .to_string_lossy(),
            "exists": install_dir.join(crate::uninstall::helper_exe_name()).is_file(),
        },
        "data": data,
    })
}

/// 当前进程所在的程序目录（exe 的父目录）
fn current_install_dir() -> Result<PathBuf, ApiError> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .ok_or_else(|| ApiError::Internal("无法确定程序所在目录".into()))
}

/// GET /api/uninstall/detect — 卸载检测
///
/// 返回卸载时将清理的**全部**内容（不执行任何删除）：
/// - `items`：`base_path` 之外的系统残留（用户数据目录 / Playwright 缓存 / 自启动）；
/// - `program`：程序目录（`purge` 会整体删除）；
/// - `helper`：卸载助手是否在位（不在位时 `purge` 必失败，界面据此先拦下）；
/// - `data`：`base_path` 下的用户数据目录（勾选「保留配置与任务」则保留）；
/// - `blocked`：非 `null` 表示**拒绝卸载**及原因（如该目录是源码仓库、cargo 构建输出）。
///   界面据此禁用卸载按钮并显示原因——守卫在这里先跑一次，用户还在界面上能看到。
pub async fn detect_uninstall(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    let autostart_enabled = settings.global.app.autostart_enabled;
    let base_path = config.base_path();
    let install_dir = current_install_dir()?;

    let mut items = Vec::new();
    match user_data_dir() {
        Some(p) => items.push(serde_json::json!({
            "key": "user_data",
            "label": "用户数据目录（加密密钥等）",
            "exists": p.exists(),
            "description": p.to_string_lossy(),
        })),
        None => items.push(serde_json::json!({
            "key": "user_data",
            "label": "用户数据目录（加密密钥等）",
            "exists": false,
            "description": "无法确定用户主目录",
        })),
    }
    match playwright_cache_dir() {
        Some((p, _)) => items.push(serde_json::json!({
            "key": "playwright",
            "label": "Playwright 浏览器缓存",
            "exists": p.exists(),
            "description": p.to_string_lossy(),
        })),
        None => items.push(serde_json::json!({
            "key": "playwright",
            "label": "Playwright 浏览器缓存",
            "exists": false,
            "description": "无独立缓存目录（随程序目录一起删除）",
        })),
    }
    items.push(serde_json::json!({
        "key": "autostart",
        "label": "开机自启动",
        "exists": autostart_enabled,
        "description": if autostart_enabled { "已注册，卸载时将关闭" } else { "未注册" },
    }));

    let blocked = crate::uninstall::build_plan(&install_dir, &base_path, false)
        .err()
        .map(|e| e.to_string());

    let inventory = build_inventory(&base_path, &install_dir);
    Ok(data(serde_json::json!({
        "items": items,
        "program": inventory["program"].clone(),
        "helper": inventory["helper"].clone(),
        "data": inventory["data"].clone(),
        "blocked": blocked,
    })))
}

/// POST /api/uninstall — 清理系统残留（卸载第一步）
///
/// 依次执行：关闭开机自启动 → 删除用户数据目录 → 清理 Playwright 浏览器缓存。
/// 每一步尽力而为、互不阻断，逐项返回结果。
///
/// POST /api/uninstall 的请求体（可选：不带 body 时按"不保留"处理，老调用方不受影响）
#[derive(Debug, Deserialize, Default)]
pub struct CleanupRequest {
    /// 与 [`PurgeRequest::keep_user_data`] 同一个开关
    ///
    /// 勾选时**不删**加密密钥目录（`~/.campus_network_auth`）：被保留的 `config/`
    /// 里方案密码是 `ENC:` 密文，密钥一删这些密码就再也解不开（下次启动会生成新密钥，
    /// 解密失败 → 用户得把所有方案密码重填一遍）。"保留配置与任务"必须真的能读。
    #[serde(default)]
    pub keep_user_data: bool,
}

/// POST /api/uninstall — 清理系统残留（卸载第一步）
///
/// 依次执行：关闭开机自启动 → 删除用户数据目录（勾选「保留配置与任务」时跳过）→
/// 清理 Playwright 浏览器缓存。每一步尽力而为、互不阻断，逐项返回结果。
///
/// **不删程序本身**（那需要主进程先退出，见 [`purge_uninstall`]）。保留这个只清残留的
/// 端点是有意的：它同时是"重置环境"的入口——清掉浏览器缓存与自启动注册后程序照常可用，
/// 不必卸载。前端「卸载」流程会紧接着调用 `purge`。
pub async fn uninstall(
    State(config): State<Arc<dyn ConfigApi>>,
    body: Option<Json<CleanupRequest>>,
) -> Result<Json<Value>, ApiError> {
    let keep_user_data = body.map(|b| b.0.keep_user_data).unwrap_or(false);
    // 卸载为破坏性操作（删用户数据/加密密钥/浏览器缓存/自启动注册），info 留痕各步骤
    tracing::info!(
        keep_user_data,
        "开始执行卸载清理（自启动 / 用户数据 / Playwright 缓存）"
    );
    let mut results: Vec<Value> = Vec::new();

    // ---- 步骤 1：关闭开机自启动 ----
    let (ok, msg) = disable_autostart(&config).await;
    if ok {
        tracing::info!("卸载步骤 1/3（关闭开机自启动）完成: {msg}");
    } else {
        tracing::warn!("卸载步骤 1/3（关闭开机自启动）失败: {msg}");
    }
    results.push(step_result("autostart", "关闭开机自启动", ok, &msg));

    // ---- 步骤 2：删除用户数据目录（勾了「保留配置与任务」就留着）----
    let (ok, msg) = if keep_user_data {
        tracing::info!("卸载步骤 2/3（用户数据目录）已按「保留配置与任务」跳过");
        (
            true,
            "已保留（保留的配置需要它才能解密方案密码）".to_string(),
        )
    } else {
        match user_data_dir() {
            Some(p) => match tokio::task::spawn_blocking(move || remove_dir_if_exists(&p)).await {
                Ok(Ok(())) => (true, "已删除".to_string()),
                Ok(Err(e)) => (false, e),
                Err(e) => (false, format!("删除任务异常: {e}")),
            },
            None => (false, "无法确定用户主目录".to_string()),
        }
    };
    if ok {
        tracing::info!("卸载步骤 2/3（用户数据目录）完成: {msg}");
    } else {
        tracing::warn!("卸载步骤 2/3（用户数据目录）失败: {msg}");
    }
    results.push(step_result(
        "user_data",
        "用户数据目录（加密密钥）",
        ok,
        &msg,
    ));

    // ---- 步骤 3：清理 Playwright 浏览器缓存 ----
    let (ok, msg) = match playwright_cache_dir() {
        Some((p, from_env)) => {
            match tokio::task::spawn_blocking(move || remove_playwright_cache(&p, from_env)).await {
                Ok(Ok(m)) => (true, m),
                Ok(Err(e)) => (false, e),
                Err(e) => (false, format!("清理任务异常: {e}")),
            }
        }
        None => (true, "无独立缓存目录，已跳过".to_string()),
    };
    if ok {
        tracing::info!("卸载步骤 3/3（清理 Playwright 浏览器缓存）完成: {msg}");
    } else {
        tracing::warn!("卸载步骤 3/3（清理 Playwright 浏览器缓存）失败: {msg}");
    }
    results.push(step_result(
        "playwright",
        "清理 Playwright 浏览器缓存",
        ok,
        &msg,
    ));

    let all_ok = results
        .iter()
        .all(|r| r["success"].as_bool().unwrap_or(false));
    tracing::info!(all_ok, "卸载清理执行完毕");
    let message = if all_ok {
        "系统残留已清理"
    } else {
        "部分清理项失败，可重试或手动处理"
    };
    Ok(data(serde_json::json!({
        "results": results,
        "message": message,
    })))
}

/// POST /api/uninstall/purge 的请求体
#[derive(Debug, Deserialize)]
pub struct PurgeRequest {
    /// 保留用户数据（`config` / `tasks` / `logs` / `environment` / `update`）
    ///
    /// 默认 `false` = 真卸载。勾选它只为"把便携版换个目录再装一次"的场景服务：
    /// 程序文件照样删掉，但方案/任务/日志留着，重装后直接可用。
    #[serde(default)]
    pub keep_user_data: bool,
}

/// POST /api/uninstall/purge — 删除程序本身并退出（卸载第二步）
///
/// 顺序是**刻意的**，每一步都有具体理由：
///
/// 1. **守卫先跑**（`build_plan`）：用户还在界面上，拒绝原因（如"这是源码仓库，不是
///    安装目录"）能当场看到；助手侧还会再校验一次，防 CLI 参数被绕过。
/// 2. **取消待应用更新**：不取消的话，退出时 `graceful_shutdown` 的
///    `ensure_helper_for_shutdown` 见到 pending 存在就会唤醒**更新**助手，把用户刚卸载
///    的程序又"更新"回来并重启——卸载直接失效。
/// 3. **spawn 卸载助手**：它等待本进程退出后才动手（Windows 上运行中的 exe 删不掉自己）。
/// 4. **优雅关闭本进程**：走完整清理流程，而不是 `exit(0)`。看门狗兜底防挂死。
///
/// 响应在关闭信号发出前构造：watch 的 `send` 只做通知，Axum 的优雅关闭会把这一个
/// 响应发完再停（与 `restart_app` 同一模式）。
pub async fn purge_uninstall(
    State(state): State<AppState>,
    Json(body): Json<PurgeRequest>,
) -> Result<Json<Value>, ApiError> {
    let base_path = state.config.base_path();
    let install_dir = current_install_dir()?;

    let plan = crate::uninstall::build_plan(&install_dir, &base_path, body.keep_user_data)
        .map_err(ApiError::BadRequest)?;

    // 助手不在位就别往下走了：这一步之后是"清系统残留 → 让本进程退出"，而助手缺失意味着
    // 删除永远不会发生（程序还在、凭据密钥却没了）。400 的文案是"用户能自己纠正"这类
    // （重新解压发布包），与 `spawn_helper` 的失败分开报，界面才能给出可执行的建议。
    let helper_path = install_dir.join(crate::uninstall::helper_exe_name());
    if !helper_path.is_file() {
        return Err(ApiError::BadRequest(format!(
            "卸载助手缺失：{}。请重新解压完整发布包后再卸载（程序文件未被删除）",
            helper_path.display()
        )));
    }

    let cancelled_pending = state.updater.cancel_pending_update().await;
    // 复查：取消失败（pending.json / staging 被占用）时，退出后更新助手仍可能把程序装回来。
    // 这种事必须出声——用户以为卸载完成了，下次开机却看到程序还在。
    let pending_update_left = state.updater.has_pending_update();
    if pending_update_left {
        tracing::warn!("待应用更新未被取消，卸载后仍可能被更新助手重新安装");
    }

    if let Err(e) = crate::uninstall::spawn_helper(&install_dir, &base_path, body.keep_user_data) {
        // spawn 失败意味着卸载并未发生（程序文件未被删除）：恢复更新入口，
        // 否则 cancel_pending_update 落下的取消标记会**永久**拒绝此后所有更新，
        // 用户只能重启进程才能再次尝试更新
        state.updater.restore_after_failed_uninstall();
        return Err(ApiError::Internal(e));
    }

    // 破坏性操作：把"删什么"写进日志（事后追溯的唯一依据——程序目录本身即将消失）
    tracing::info!(
        install_dir = %install_dir.display(),
        base_path = %base_path.display(),
        keep_user_data = body.keep_user_data,
        cancelled_pending_update = cancelled_pending,
        "已启动卸载助手，本进程即将退出以放行删除"
    );

    // 通知 launcher 优雅关闭：助手据此等到本进程退出后执行删除
    let _ = state.shutdown_tx.send(());
    crate::launcher::spawn_exit_watchdog(30);

    Ok(data(serde_json::json!({
        "message": if pending_update_left {
            "正在卸载，程序即将退出（注意：待应用的更新未能取消，程序可能被重新安装）"
        } else {
            "正在卸载，程序即将退出"
        },
        "kept_user_data": body.keep_user_data,
        "install_dir": plan.install_dir.to_string_lossy(),
        "data_dirs": plan
            .data_dirs
            .iter()
            .map(|(name, path)| serde_json::json!({
                "key": name,
                "label": crate::uninstall::data_dir_label(name),
                "path": path.to_string_lossy(),
            }))
            .collect::<Vec<_>>(),
        "cancelled_pending_update": cancelled_pending,
        "pending_update_left": pending_update_left,
    })))
}

/// 关闭开机自启动：配置标志置 false 并取消系统注册（均尽力而为）
///
/// 配置改写走 `modify_settings_tx`（持锁读-改-写）：锁外的 load→改→save 会
/// 覆盖并发写入的其他设置项（与 PATCH/PUT 的合并保存同一约束）。
async fn disable_autostart(config: &Arc<dyn ConfigApi>) -> (bool, String) {
    let modify = |mut settings: crate::config::SettingsData| {
        settings.global.app.autostart_enabled = false;
        Ok(settings)
    };
    match config.modify_settings_tx(Box::new(modify)).await {
        // 外层 Err 为 IO/隔离态错误；内层 Err(String) 为闭包校验失败（设置未落盘）
        Err(e) => return (false, format!("保存配置失败: {e}")),
        Ok(Err(reason)) => return (false, format!("保存配置失败: {reason}")),
        Ok(Ok(())) => {}
    }
    match tokio::task::spawn_blocking(|| crate::utils::platform::set_self_start(false)).await {
        Ok(Ok(())) => (true, "已关闭".to_string()),
        Ok(Err(e)) => (false, format!("取消注册失败: {e}")),
        Err(e) => (false, format!("任务异常: {e}")),
    }
}

/// 构造单步结果 JSON
fn step_result(key: &str, label: &str, success: bool, message: &str) -> Value {
    serde_json::json!({
        "key": key,
        "label": label,
        "success": success,
        "message": message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// remove_dir_if_exists：不存在时幂等成功
    #[test]
    fn remove_dir_if_exists_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("absent");
        assert!(remove_dir_if_exists(&absent).is_ok());
    }

    /// remove_dir_if_exists：存在时递归删除
    #[test]
    fn remove_dir_if_exists_removes_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("campus");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("f.txt"), b"x").unwrap();
        remove_dir_if_exists(&dir).unwrap();
        assert!(!dir.exists());
    }

    /// 自定义 PLAYWRIGHT_BROWSERS_PATH：只删浏览器前缀子目录，目录本身保留
    #[test]
    fn remove_playwright_cache_keeps_custom_dir_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pw-cache");
        std::fs::create_dir_all(dir.join("chromium-1234")).unwrap();
        std::fs::create_dir_all(dir.join("ffmpeg-1005")).unwrap();
        std::fs::create_dir_all(dir.join("user-stuff")).unwrap();

        let msg = remove_playwright_cache(&dir, true).unwrap();
        assert!(msg.contains("2"), "应删除 2 项: {msg}");
        assert!(!dir.join("chromium-1234").exists());
        assert!(!dir.join("ffmpeg-1005").exists());
        assert!(dir.join("user-stuff").exists(), "非浏览器子目录必须保留");
        assert!(dir.exists(), "自定义缓存目录本身不应被删除");
    }

    /// 自定义目录中无浏览器缓存时跳过
    #[test]
    fn remove_playwright_cache_skips_when_no_browsers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pw-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let msg = remove_playwright_cache(&dir, true).unwrap();
        assert!(msg.contains("跳过"));
    }

    /// 默认缓存目录：整体删除
    #[test]
    fn remove_playwright_cache_removes_default_dir_entirely() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ms-playwright");
        std::fs::create_dir_all(dir.join("chromium-1234")).unwrap();
        remove_playwright_cache(&dir, false).unwrap();
        assert!(!dir.exists(), "默认缓存目录应被整体删除");
    }

    /// step_result 形状
    #[test]
    fn step_result_shape() {
        let v = step_result("k", "标签", true, "ok");
        assert_eq!(v["key"], "k");
        assert_eq!(v["label"], "标签");
        assert_eq!(v["success"], true);
        assert_eq!(v["message"], "ok");
    }

    /// 卸载清单：程序目录 + 五个数据目录，逐项给出路径与存在性
    ///
    /// 「删除时必须明确列出会删哪些内容」是用户拍板的口径，故清单必须逐项可读；
    /// 存在性也要真实（不存在的目录不该被列成"将删除"）。
    #[test]
    fn test_build_inventory_lists_program_and_data() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let install = tmp.path().join("install");
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::create_dir_all(base.join("tasks")).unwrap();
        std::fs::create_dir_all(&install).unwrap();

        let inv = build_inventory(&base, &install);

        assert_eq!(inv["program"]["path"], install.to_string_lossy().as_ref());
        assert_eq!(inv["program"]["exists"], true);
        assert_eq!(inv["program"]["label"], "程序目录");

        let data = inv["data"].as_array().unwrap();
        assert_eq!(data.len(), 5, "五个用户数据目录都要列出");
        let by_key = |k: &str| {
            data.iter()
                .find(|d| d["key"] == k)
                .unwrap_or_else(|| panic!("缺少数据目录 {k}"))
                .clone()
        };
        assert_eq!(by_key("config")["label"], "配置与方案");
        assert_eq!(by_key("config")["exists"], true);
        assert_eq!(by_key("tasks")["label"], "任务与脚本");
        assert_eq!(by_key("logs")["exists"], false, "不存在的目录如实标注");
        assert_eq!(
            by_key("config")["path"],
            base.join("config").to_string_lossy().as_ref()
        );
    }

    /// 程序目录不存在时如实标注（比如被手动删过一半的现场）
    #[test]
    fn test_build_inventory_marks_missing_program_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let inv = build_inventory(tmp.path(), &tmp.path().join("gone"));
        assert_eq!(inv["program"]["exists"], false);
    }
}
