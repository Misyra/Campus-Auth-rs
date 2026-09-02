//! 卸载路由：卸载检测与执行（A-5 自 system.rs 拆出）
//!
//! 卸载语义：只清理 base_path 之外的系统残留——用户数据目录
//! （`~/.campus_network_auth`）、开机自启动注册、Playwright 浏览器缓存；
//! 程序目录本身由用户在清理完成后手动删除（前端提示）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::web::error::{ApiError, data};

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

/// GET /api/uninstall/detect — 卸载检测
///
/// 返回卸载时将清理的项目清单（不执行实际删除）：用户数据目录、Playwright
/// 浏览器缓存是否存在，以及开机自启动注册状态。
pub async fn detect_uninstall(
    State(config): State<Arc<dyn ConfigApi>>,
) -> Result<Json<Value>, ApiError> {
    let settings = config.load_settings_async().await;
    let autostart_enabled = settings.global.app.autostart_enabled;

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
    Ok(data(serde_json::json!(items)))
}

/// POST /api/uninstall — 执行卸载清理
///
/// 依次执行：关闭开机自启动 → 删除用户数据目录 → 清理 Playwright 浏览器缓存。
/// 每一步尽力而为、互不阻断，逐项返回结果；完成后由用户手动删除程序所在
/// 文件夹完成卸载（不生成卸载脚本、不主动退出程序）。
pub async fn uninstall(State(config): State<Arc<dyn ConfigApi>>) -> Result<Json<Value>, ApiError> {
    // 卸载为破坏性操作（删用户数据/加密密钥/浏览器缓存/自启动注册），info 留痕各步骤
    tracing::info!("开始执行卸载清理（自启动 / 用户数据 / Playwright 缓存）");
    let mut results: Vec<Value> = Vec::new();

    // ---- 步骤 1：关闭开机自启动 ----
    let (ok, msg) = disable_autostart(&config).await;
    if ok {
        tracing::info!("卸载步骤 1/3（关闭开机自启动）完成: {msg}");
    } else {
        tracing::warn!("卸载步骤 1/3（关闭开机自启动）失败: {msg}");
    }
    results.push(step_result("autostart", "关闭开机自启动", ok, &msg));

    // ---- 步骤 2：删除用户数据目录 ----
    let (ok, msg) = match user_data_dir() {
        Some(p) => match tokio::task::spawn_blocking(move || remove_dir_if_exists(&p)).await {
            Ok(Ok(())) => (true, "已删除".to_string()),
            Ok(Err(e)) => (false, e),
            Err(e) => (false, format!("删除任务异常: {e}")),
        },
        None => (false, "无法确定用户主目录".to_string()),
    };
    if ok {
        tracing::info!("卸载步骤 2/3（删除用户数据目录）完成: {msg}");
    } else {
        tracing::warn!("卸载步骤 2/3（删除用户数据目录）失败: {msg}");
    }
    results.push(step_result("user_data", "删除用户数据目录", ok, &msg));

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
        "清理完成，删除程序所在文件夹即可完成卸载"
    } else {
        "部分清理项失败，可重试或手动处理；完成后删除程序所在文件夹即可完成卸载"
    };
    Ok(data(serde_json::json!({
        "results": results,
        "message": message,
    })))
}

/// 关闭开机自启动：配置标志置 false 并取消系统注册（均尽力而为）
async fn disable_autostart(config: &Arc<dyn ConfigApi>) -> (bool, String) {
    let mut settings = config.load_settings_async().await;
    settings.global.app.autostart_enabled = false;
    if let Err(e) = config.save_settings(&settings).await {
        return (false, format!("保存配置失败: {e}"));
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
}
