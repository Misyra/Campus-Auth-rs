//! Python 安装：uv sync + Playwright 浏览器安装

use crate::environment::{
    uv_exe_path, EnvironmentError, EnvironmentManager, PLAYWRIGHT_INSTALL_MAX_RETRIES,
    PLAYWRIGHT_INSTALL_RETRY_DELAY, PLAYWRIGHT_INSTALL_TIMEOUT,
};

use std::path::Path;
use std::time::Duration;

/// 实际启动 Python 并检查退出状态，避免仅凭 `python.exe` 存在误判损坏的 uv venv。
pub(crate) async fn python_executable_works(python_exe: &Path) -> bool {
    if !python_exe.is_file() {
        return false;
    }
    let mut cmd = tokio::process::Command::new(python_exe);
    cmd.arg("--version");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    matches!(
        tokio::time::timeout(Duration::from_secs(5), cmd.output()).await,
        Ok(Ok(output)) if output.status.success()
    )
}

/// 确保 Python 虚拟环境就绪
///
/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建（基础依赖，
/// 不含 ddddocr）。OCR 依赖（ddddocr）由前端的显式"安装/卸载"操作经
/// `uv add/remove ddddocr` 管理（见 environment::uv::install_ocr_dep /
/// remove_ocr_dep），此处不做自动补装，避免显式卸载后又被自动装回。
/// 返回 Python 解释器路径。
pub async fn ensure_venv(mgr: &EnvironmentManager) -> Result<std::path::PathBuf, EnvironmentError> {
    let python_exe = mgr.worker_project_path().join(crate::environment::PYTHON_EXE_RELATIVE);

    // 文件存在不代表 uv 管理的基础解释器仍存在，必须实际启动一次。
    if python_executable_works(&python_exe).await {
        return Ok(python_exe);
    }

    if python_exe.exists() {
        tracing::warn!("虚拟环境解释器无法启动，执行 uv sync 修复");
    }

    // 不存在则执行 uv sync 创建虚拟环境并安装依赖
    tracing::info!("虚拟环境不存在，执行 uv sync 创建...");
    crate::environment::uv::run_uv_sync(mgr).await?;

    // 验证创建成功
    if !python_executable_works(&python_exe).await {
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

/// OCR 依赖（ddddocr）是否在 `python_worker/pyproject.toml` 中声明
///
/// 作为 OCR 可用性的权威来源：仅当项目声明了该依赖，前端才展示「安装/卸载」
/// 入口与识别能力。文件缺失或读取失败时返回 false，避免把损坏的环境误报为可用。
pub(crate) fn ocr_declared(mgr: &EnvironmentManager) -> bool {
    let pyproject = mgr.worker_project_path().join("pyproject.toml");
    let content = match std::fs::read_to_string(&pyproject) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 简单稳健的判定：依赖列表中出现 ddddocr（>=1.6.1 之类约束），无需完整 TOML 解析
    let mut in_deps = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with("dependencies") {
            in_deps = true;
            continue;
        }
        // 进入其它顶层表头（如 [build-system] / [tool.*]）则退出依赖块
        if in_deps && line.starts_with('[') {
            in_deps = false;
        }
        if in_deps && line.contains("ddddocr") {
            return true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 不存在的解释器路径必须判定为不可用。
    #[tokio::test]
    async fn test_python_executable_works_rejects_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!python_executable_works(&dir.path().join("missing-python.exe")).await);
    }
}
