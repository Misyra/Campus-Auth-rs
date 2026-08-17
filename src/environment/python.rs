//! Python 安装：uv sync + Playwright 浏览器安装

use crate::environment::{
    uv_exe_path, EnvironmentError, EnvironmentManager, PLAYWRIGHT_INSTALL_MAX_RETRIES,
    PLAYWRIGHT_INSTALL_RETRY_DELAY, PLAYWRIGHT_INSTALL_TIMEOUT,
};

/// 确保 Python 虚拟环境就绪
///
/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建。
/// venv 已存在但 ddddocr（OCR 依赖，optional extra）缺失时补装
/// （历史 venv 由不带 `--extra ocr` 的 sync 创建，需增量补齐）。
/// 返回 Python 解释器路径。
pub async fn ensure_venv(mgr: &EnvironmentManager) -> Result<std::path::PathBuf, EnvironmentError> {
    let python_exe = mgr.worker_project_path().join(crate::environment::PYTHON_EXE_RELATIVE);

    // 如果 Python 解释器已存在且 OCR 依赖齐备，直接返回
    if python_exe.exists() {
        if ddddocr_installed(mgr) {
            return Ok(python_exe);
        }
        tracing::info!("检测到 OCR 依赖（ddddocr）缺失，执行 uv sync 补装...");
        if let Err(e) = crate::environment::uv::run_uv_sync(mgr).await {
            // OCR 为可选能力：补装失败不阻断核心浏览器自动化（playwright）就绪
            tracing::warn!("OCR 依赖补装失败（不影响核心浏览器能力）: {e}");
        }
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

/// 检查 venv 内 ddddocr（OCR 依赖）是否已安装
///
/// 通过 site-packages 下存在 `ddddocr` 包目录或 `ddddocr-*.dist-info` 判定，
/// 兼容 Windows（Lib/site-packages）与 Unix（lib/python3.x/site-packages）布局。
pub(crate) fn ddddocr_installed(mgr: &EnvironmentManager) -> bool {
    let venv = mgr.worker_project_path().join(crate::environment::VENV_DIR);
    let candidates = [
        // Windows venv 布局
        venv.join("Lib").join("site-packages"),
        // Unix venv 布局（python3.12 固定小版本约束下取 3.12）
        venv.join("lib").join("python3.12").join("site-packages"),
    ];
    for site in candidates {
        if !site.is_dir() {
            continue;
        }
        if site.join("ddddocr").is_dir() {
            return true;
        }
        // wheel 安装记录（egg-info/dists 目录名形如 ddddocr-1.5.x.dist-info）
        if let Ok(entries) = std::fs::read_dir(&site) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("ddddocr-") {
                    return true;
                }
            }
        }
    }
    false
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
