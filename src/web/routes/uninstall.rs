//! 卸载路由：卸载检测与执行（A-5 自 system.rs 拆出）
//!
//! M1 细粒度 state：经 `State<Arc<dyn ConfigApi>>` 提取，不触达 `state.container`。

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use crate::web::error::{ApiError, data};
use crate::web::routes::system::spawn_exit_watchdog;
use crate::web::state::AppState;

/// GET /api/uninstall/detect — 卸载检测
///
/// 返回卸载时将清理的目录与文件清单（不执行实际删除），每项为一个 UninstallItem。
pub async fn detect_uninstall(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let base = state.config.base_path();
    let mut items = Vec::new();
    for (key, label, sub) in [
        ("config", "配置目录", "config"),
        ("logs", "日志目录", "logs"),
        ("environment", "环境目录", "environment"),
        ("tasks", "任务目录", "tasks"),
        ("update", "更新目录", "update"),
    ] {
        let path = base.join(sub);
        items.push(serde_json::json!({
            "key": key,
            "label": label,
            "exists": path.exists(),
            "description": path.to_string_lossy(),
        }));
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    items.push(serde_json::json!({
        "key": "executable",
        "label": "可执行文件",
        "exists": !exe.is_empty() && std::path::Path::new(&exe).exists(),
        "description": exe,
    }));
    Ok(data(serde_json::json!(items)))
}

/// Batch 元字符：路径含这些字符时会被拼入 uninstall.bat 形成 cmd 注入
/// （如 `--base-path 'C:\x" & del C:\ /s /q "'`），必须整体拒绝
fn contains_batch_metachars(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '"' | '%' | '&' | '|' | '<' | '>' | '^'))
}

/// POST /api/uninstall — 执行卸载
///
/// 生成并写入卸载助手脚本（batch），然后退出程序。
/// 用户手动运行该脚本完成残留文件清理。
pub async fn uninstall(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let base = state.config.base_path();

    // 如果 helper 存在则直接写入卸载脚本并退出
    let uninstall_script = base.join("uninstall.bat");
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // 路径会被直接嵌入 batch 脚本，含元字符即可能注入任意 cmd 命令（A5）
    let base_str = base.display().to_string();
    if contains_batch_metachars(&base_str) || contains_batch_metachars(&exe) {
        return Err(ApiError::BadRequest(format!(
            "安装路径含 batch 元字符，拒绝生成卸载脚本以防止命令注入: {base_str}"
        )));
    }

    // 卸载脚本：首次运行时把自身副本复制到 %TEMP% 再从副本执行，避免
    // `rd /s /q "{base}"` 删除正在运行的 bat 自身所在目录时因文件被锁而残留
    // （7.3：原实现直接运行会残留 base/uninstall.bat）。
    let script = format!(
        "@echo off\r\n\
         chcp 65001 > nul\r\n\
         if \"%1\"==\"run_from_temp\" goto :run\r\n\
         copy /y \"%~f0\" \"%TEMP%\\campus-auth-uninstall.bat\" > nul\r\n\
         start \"\" \"%TEMP%\\campus-auth-uninstall.bat\" run_from_temp\r\n\
         exit /b 0\r\n\
         :run\r\n\
         echo Campus-Auth 卸载助手\r\n\
         echo =====================================\r\n\
         echo.\r\n\
         echo 即将删除 Campus-Auth 所有文件...\r\n\
         timeout /t 3 /nobreak > nul\r\n\
         echo.\r\n\
         taskkill /f /im campus-auth.exe 2>nul\r\n\
         taskkill /f /im campus-auth-helper.exe 2>nul\r\n\
         timeout /t 1 /nobreak > nul\r\n\
         rd /s /q \"{base}\" 2>nul\r\n\
         del /f /q \"{exe}\" 2>nul\r\n\
         del /f /q \"%TEMP%\\campus-auth-uninstall.bat\" 2>nul\r\n\
         echo.\r\n\
         echo 卸载完成。\r\n\
         pause\r\n",
        base = base.display(),
    );

    tokio::fs::write(&uninstall_script, script).await?;

    // 通知 launcher 优雅关闭
    let _ = state.shutdown_tx.send(());
    // watchdog：统一 30s，覆盖优雅关闭总预算，避免卸载时强杀过早晨残留浏览器/子进程（A4）
    spawn_exit_watchdog(30);

    Ok(data(serde_json::json!({
        "message": "卸载脚本已生成，程序即将退出。请手动运行 uninstall.bat 完成清理。",
        "script_path": uninstall_script.to_string_lossy(),
    })))
}
