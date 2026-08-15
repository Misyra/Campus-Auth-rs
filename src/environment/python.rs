//! Python 安装：uv sync + Playwright 浏览器安装

use crate::environment::{
    uv_exe_path, EnvironmentError, EnvironmentManager, PLAYWRIGHT_INSTALL_MAX_RETRIES,
    PLAYWRIGHT_INSTALL_RETRY_DELAY, PLAYWRIGHT_INSTALL_TIMEOUT,
};

/// 确保 Python 虚拟环境就绪
///
/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建。
/// 返回 Python 解释器路径。
pub async fn ensure_venv(mgr: &EnvironmentManager) -> Result<std::path::PathBuf, EnvironmentError> {
    let python_exe = mgr.worker_project_path().join(crate::environment::PYTHON_EXE_RELATIVE);

    // 如果 Python 解释器已存在，直接返回
    if python_exe.exists() {
        return Ok(python_exe);
    }

    // 不存在则执行 uv sync 创建虚拟环境并安装依赖
    tracing::info!("虚拟环境不存在，执行 uv sync 创建...");
    crate::environment::uv::run_uv_sync(mgr).await?;

    // 验证创建成功
    if !python_exe.exists() {
        return Err(EnvironmentError::VenvCorrupted);
    }

    Ok(python_exe)
}

/// 安装 Playwright Chromium 浏览器
///
/// 执行 `uv run playwright install chromium`，带超时和重试。
pub async fn install_playwright(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let uv_exe = uv_exe_path(mgr);
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut last_err_msg = String::new();

    for attempt in 0..PLAYWRIGHT_INSTALL_MAX_RETRIES {
        // 检查取消
        if mgr.cancel_token().is_cancelled() {
            return Err(EnvironmentError::Cancelled);
        }

        // 重试时更新进度消息
        if attempt > 0 {
            let msg = format!(
                "重试安装浏览器 ({}/{})...",
                attempt, PLAYWRIGHT_INSTALL_MAX_RETRIES
            );
            tracing::info!("{}", msg);
            mgr.report_progress("installing_playwright", 70, &msg);
            tokio::time::sleep(PLAYWRIGHT_INSTALL_RETRY_DELAY).await;
        }

        // 执行 uv run playwright install chromium
        let cmd_future = crate::environment::uv::uv_command(&uv_exe)
            .args([
                "run",
                "--project",
                &mgr.worker_project_path().to_string_lossy(),
                "playwright",
                "install",
                "chromium",
            ])
            .env("UV_PROJECT_ENVIRONMENT", &venv_path)
            .current_dir(mgr.base_path())
            .output();

        let result = tokio::time::timeout(PLAYWRIGHT_INSTALL_TIMEOUT, cmd_future).await;

        match result {
            Ok(Ok(output)) if output.status.success() => {
                tracing::info!("Playwright Chromium 安装成功");
                return Ok(());
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                last_err_msg = format!("exit code={:?}, stderr={}, stdout={}", output.status.code(), stderr, stdout);
                tracing::warn!(
                    "Playwright 安装失败 (尝试 {}/{}): {}",
                    attempt + 1,
                    PLAYWRIGHT_INSTALL_MAX_RETRIES,
                    last_err_msg
                );
            }
            Ok(Err(e)) => {
                last_err_msg = e.to_string();
                tracing::warn!(
                    "Playwright 安装 IO 错误 (尝试 {}/{}): {}",
                    attempt + 1,
                    PLAYWRIGHT_INSTALL_MAX_RETRIES,
                    last_err_msg
                );
            }
            Err(_elapsed) => {
                last_err_msg = format!("安装超时 (超过 {}s)", PLAYWRIGHT_INSTALL_TIMEOUT.as_secs());
                tracing::warn!(
                    "{} (尝试 {}/{})",
                    last_err_msg,
                    attempt + 1,
                    PLAYWRIGHT_INSTALL_MAX_RETRIES
                );
            }
        }
    }

    // 所有重试均失败
    Err(EnvironmentError::PlaywrightInstallFailed {
        retries: PLAYWRIGHT_INSTALL_MAX_RETRIES,
        message: last_err_msg,
    })
}
