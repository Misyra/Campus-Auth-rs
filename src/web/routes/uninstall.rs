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

/// unix shell 元字符：路径会以单引号形式嵌入 uninstall.sh，
/// 含 `'` / `$` / 反引号 / 反斜杠 / 换行时可逃逸引用或触发命令替换
#[cfg(not(windows))]
fn contains_shell_metachars(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\'' | '"' | '$' | '`' | '\\' | '\n' | ';' | '|' | '&'))
}

/// POST /api/uninstall — 执行卸载
///
/// 生成并写入卸载助手脚本（Windows batch / unix shell），然后退出程序。
/// 用户手动运行该脚本完成残留文件清理。
pub async fn uninstall(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let base = state.config.base_path();
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // 路径会被直接嵌入卸载脚本，含元字符即可能注入任意命令（A5）
    let base_str = base.display().to_string();
    #[allow(unused_mut)]
    let mut injection_risk = contains_batch_metachars(&base_str) || contains_batch_metachars(&exe);
    #[cfg(not(windows))]
    {
        injection_risk =
            injection_risk || contains_shell_metachars(&base_str) || contains_shell_metachars(&exe);
    }
    if injection_risk {
        return Err(ApiError::BadRequest(format!(
            "安装路径含脚本元字符，拒绝生成卸载脚本以防止命令注入: {base_str}"
        )));
    }

    let (script, script_path, message) = render_uninstall_script(&base, &exe);
    tokio::fs::write(&script_path, &script).await?;
    // unix：脚本需可执行权限才能 `./uninstall.sh` 直接运行
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            tokio::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).await;
    }

    // 通知 launcher 优雅关闭
    let _ = state.shutdown_tx.send(());
    // watchdog：统一 30s，覆盖优雅关闭总预算，避免卸载时强杀过早晨残留浏览器/子进程（A4）
    spawn_exit_watchdog(30);

    Ok(data(serde_json::json!({
        "message": message,
        "script_path": script_path.to_string_lossy(),
    })))
}

/// 按平台渲染卸载脚本：返回 (脚本内容, 脚本路径, 用户提示文案)
fn render_uninstall_script(
    base: &std::path::Path,
    exe: &str,
) -> (String, std::path::PathBuf, &'static str) {
    #[cfg(windows)]
    let result = {
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
        (
            script,
            base.join("uninstall.bat"),
            "卸载脚本已生成，程序即将退出。请手动运行 uninstall.bat 完成清理。",
        )
    };
    #[cfg(not(windows))]
    let result = {
        // unix：同样先把脚本复制到 /tmp 再 exec 副本执行，避免 `rm -rf {base}`
        // 删到正在运行的脚本自身。pkill 用 -x 精确匹配可执行名（不匹配命令行，
        // 防止安装路径含 "campus-auth" 时误杀脚本自身）；comm 名上限 15 字符，
        // campus-auth-helper 会被截断为 campus-auth-hel，需一并匹配。
        let script = format!(
            "#!/bin/sh\n\
             # Campus-Auth 卸载助手（macOS / Linux）\n\
             if [ \"$1\" != \"run_from_tmp\" ]; then\n\
             \x20 cp \"$0\" /tmp/campus-auth-uninstall.sh\n\
             \x20 chmod +x /tmp/campus-auth-uninstall.sh\n\
             \x20 exec /tmp/campus-auth-uninstall.sh run_from_tmp\n\
             fi\n\
             echo \"Campus-Auth 卸载助手\"\n\
             echo \"=====================================\"\n\
             echo\n\
             echo \"即将删除 Campus-Auth 所有文件...\"\n\
             sleep 3\n\
             pkill -9 -x campus-auth 2>/dev/null\n\
             pkill -9 -x campus-auth-helper 2>/dev/null\n\
             pkill -9 -x campus-auth-hel 2>/dev/null\n\
             sleep 1\n\
             rm -rf '{base}'\n\
             rm -f '{exe}'\n\
             rm -f /tmp/campus-auth-uninstall.sh\n\
             echo\n\
             echo \"卸载完成。\"\n",
            base = base.display(),
            exe = exe,
        );
        (
            script,
            base.join("uninstall.sh"),
            "卸载脚本已生成，程序即将退出。请手动运行 uninstall.sh 完成清理。",
        )
    };
    result
}
