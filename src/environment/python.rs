//! Python 安装：uv sync + Playwright 浏览器安装

use crate::environment::{
    EnvironmentError, EnvironmentManager, PLAYWRIGHT_INSTALL_MAX_RETRIES,
    PLAYWRIGHT_INSTALL_RETRY_DELAY, PLAYWRIGHT_INSTALL_TIMEOUT, uv_exe_path,
};

use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// 取字符串末尾至多 `max_chars` 个字符；超出时按字符边界截断并注明省略长度
///（Playwright 安装输出可达数 MB，全量进错误消息会撑爆日志与状态快照）
fn tail_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let skipped = total - max_chars;
    let tail: String = s.chars().skip(skipped).collect();
    format!("…（前 {skipped} 字符已截断）{tail}")
}

/// 实际启动 Python 并检查退出状态，避免仅凭 `python.exe` 存在误判损坏的 uv venv。
pub(crate) async fn python_executable_works(python_exe: &Path) -> bool {
    python_executable_status(python_exe).await.is_ok()
}

/// 同 [`python_executable_works`]，但携带失败原因（缺失 / 启动失败 / 超时），供日志定位。
async fn python_executable_status(python_exe: &Path) -> Result<(), String> {
    if !python_exe.is_file() {
        return Err("解释器文件不存在".to_string());
    }
    let mut cmd = tokio::process::Command::new(python_exe);
    cmd.kill_on_drop(true);
    cmd.arg("--version");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match tokio::time::timeout(Duration::from_secs(5), cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(format!("退出码 {:?}", output.status.code())),
        Ok(Err(e)) => Err(format!("启动失败: {e}")),
        Err(_) => Err("执行超时（5s）".to_string()),
    }
}

/// 确保 Python 虚拟环境就绪
///
/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建。OCR 依赖
/// （ddddocr）属于 `ocr` extra；是否随环境修复安装由 environment/ocr.enabled
/// 持久标记决定，显式安装/卸载不会再修改 pyproject.toml。
/// 返回 Python 解释器路径。
pub async fn ensure_venv(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<std::path::PathBuf, EnvironmentError> {
    let python_exe = mgr
        .worker_project_path()
        .join(crate::environment::PYTHON_EXE_RELATIVE);

    // 文件存在不代表 uv 管理的基础解释器仍存在，必须实际启动一次。
    if let Err(reason) = python_executable_status(&python_exe).await {
        // 补充探测失败的具体原因（缺失 / 启动失败 / 超时），便于定位 venv 损坏
        tracing::debug!(reason = %reason, "Python 解释器探测未通过，虚拟环境需要修复");
    } else {
        return Ok(python_exe);
    }

    if python_exe.exists() {
        tracing::warn!("虚拟环境不可用（缺失或损坏），执行 uv sync 修复");
    } else {
        // 不存在则执行 uv sync 创建虚拟环境并安装依赖
        tracing::info!("虚拟环境不存在，执行 uv sync 创建...");
    }
    crate::environment::uv::run_uv_sync(mgr, cancel).await?;

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

/// OCR extra 是否在 `python_worker/pyproject.toml` 中声明 ddddocr。
///
/// `declared` 表示该构建支持 OCR 可选能力，并不等于用户已安装 OCR。
/// 文件缺失或声明损坏时返回 false，避免前端误报能力。
pub(crate) fn ocr_declared(mgr: &EnvironmentManager) -> bool {
    let pyproject = mgr.worker_project_path().join("pyproject.toml");
    let content = match std::fs::read_to_string(&pyproject) {
        Ok(c) => c,
        Err(_) => return false,
    };
    ocr_declared_in_pyproject(&content)
}

fn ocr_declared_in_pyproject(content: &str) -> bool {
    let mut in_optional_dependencies = false;
    let mut in_ocr_extra = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_optional_dependencies = line == "[project.optional-dependencies]";
            in_ocr_extra = false;
            continue;
        }
        if !in_optional_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }

        if !in_ocr_extra {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if name.trim() != "ocr" {
                continue;
            }
            if value.contains("ddddocr") {
                return true;
            }
            in_ocr_extra = !value.contains(']');
            continue;
        }

        if line.contains("ddddocr") {
            return true;
        }
        if line.contains(']') {
            in_ocr_extra = false;
        }
    }

    false
}

/// 安装核心 Playwright Chromium 浏览器。
///
/// 核心引导继续只安装 Chromium；Firefox/WebKit 由设置页显式按需安装。
pub async fn install_playwright(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    install_playwright_browser(mgr, "chromium", cancel).await
}

/// 安装指定的 Playwright 管理浏览器。
///
/// 仅允许 Chromium / Firefox / WebKit，执行 `uv run playwright install <browser>`，
/// 带统一超时和重试。调用方负责通过 BootstrapGate 串行化显式安装。
pub async fn install_playwright_browser(
    mgr: &EnvironmentManager,
    browser: &str,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    if !matches!(browser, "chromium" | "firefox" | "webkit") {
        return Err(EnvironmentError::UnsupportedPlaywrightBrowser {
            browser: browser.to_string(),
        });
    }

    let uv_exe = uv_exe_path(mgr);
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut last_err_msg = String::new();

    for attempt in 0..PLAYWRIGHT_INSTALL_MAX_RETRIES {
        // 检查取消
        if cancel.is_cancelled() {
            return Err(EnvironmentError::Cancelled);
        }

        // 重试时更新进度消息
        if attempt > 0 {
            let msg = format!(
                "重试安装 {browser} ({}/{})...",
                attempt, PLAYWRIGHT_INSTALL_MAX_RETRIES
            );
            tracing::info!("{}", msg);
            mgr.report_progress("installing_playwright", 70, &msg);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(EnvironmentError::Cancelled),
                _ = tokio::time::sleep(PLAYWRIGHT_INSTALL_RETRY_DELAY) => {}
            }
        }

        // 执行 uv run playwright install <browser>；取消/超时都会终止子进程。
        let mut cmd = crate::environment::uv::uv_command(&uv_exe);
        cmd.args([
            "run",
            "--project",
            &mgr.worker_project_path().to_string_lossy(),
            "playwright",
            "install",
            browser,
        ])
        .env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path());

        let result = crate::environment::uv::command_output_with_cancel(
            cmd,
            PLAYWRIGHT_INSTALL_TIMEOUT,
            cancel,
        )
        .await;

        match result {
            Ok(output) if output.status.success() => {
                tracing::info!("Playwright {browser} 安装成功");
                return Ok(());
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                // stdout/stderr 各只保留末尾 500 字符（安装失败的报错通常在结尾），
                // 避免超长输出撑爆错误消息
                last_err_msg = format!(
                    "exit code={:?}, stderr={}, stdout={}（stdout/stderr 各保留末尾 500 字符）",
                    output.status.code(),
                    tail_chars(&stderr, 500),
                    tail_chars(&stdout, 500)
                );
                tracing::warn!(
                    "Playwright 安装失败 (尝试 {}/{}): {}",
                    attempt + 1,
                    PLAYWRIGHT_INSTALL_MAX_RETRIES,
                    last_err_msg
                );
            }
            Err(crate::environment::uv::CommandOutputError::Cancelled) => {
                return Err(EnvironmentError::Cancelled);
            }
            Err(crate::environment::uv::CommandOutputError::Io(e)) => {
                last_err_msg = e.to_string();
                tracing::warn!(
                    "Playwright 安装 IO 错误 (尝试 {}/{}): {}",
                    attempt + 1,
                    PLAYWRIGHT_INSTALL_MAX_RETRIES,
                    last_err_msg
                );
            }
            Err(crate::environment::uv::CommandOutputError::Timeout) => {
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

    #[tokio::test]
    async fn test_install_playwright_browser_rejects_unknown_before_io() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
            false,
        );
        let cancel = CancellationToken::new();
        let result = install_playwright_browser(&mgr, "chrome", &cancel).await;
        assert!(matches!(
            result,
            Err(EnvironmentError::UnsupportedPlaywrightBrowser { browser }) if browser == "chrome"
        ));
    }

    #[tokio::test]
    async fn test_install_playwright_browser_observes_supplied_cancel_scope_before_io() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
            false,
        );
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = install_playwright_browser(&mgr, "chromium", &cancel).await;
        assert!(matches!(result, Err(EnvironmentError::Cancelled)));
    }

    #[test]
    fn test_ocr_declared_requires_ocr_optional_extra() {
        let optional = r#"
[project]
dependencies = ["playwright>=1.40"]

[project.optional-dependencies]
ocr = [
    "ddddocr>=1.6.1",
]
"#;
        assert!(ocr_declared_in_pyproject(optional));

        let legacy_main_dependency = r#"
[project]
dependencies = [
    "ddddocr>=1.6.1",
    "playwright>=1.40",
]
"#;
        assert!(!ocr_declared_in_pyproject(legacy_main_dependency));

        let unrelated_extra = r#"
[project.optional-dependencies]
devtools = ["ddddocr>=1.6.1"]
ocr = ["pillow"]
"#;
        assert!(!ocr_declared_in_pyproject(unrelated_extra));
    }
}
