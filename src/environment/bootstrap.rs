//! 引导流程编排：核心 + 能力两阶段

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::environment::{
    BootstrapStage, EnvironmentError, EnvironmentManager, EnvironmentStatus, PROGRESS_PLAYWRIGHT,
    PROGRESS_UV_DOWNLOAD, PROGRESS_VENV_SYNC,
};

/// 引导安装完整浏览器自动化能力（uv -> Python venv -> Playwright）
///
/// 三阶段流程：
/// 1. 确保 uv 就绪（下载或跳过）
/// 2. 确保 Python 虚拟环境就绪（uv sync 或跳过）
/// 3. 安装 Playwright Chromium 浏览器
///
/// Git 不属于浏览器自动化核心能力。开发者模式只检测系统/已有 Git，
/// 引导流程不会隐式下载 MinGit。
pub async fn bootstrap_capability(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    tracing::info!("开始引导浏览器自动化能力...");

    // 先做一次快速检查，跳过已就绪的阶段
    check_environment(mgr).await?;

    // ── 阶段 1: 确保 uv 就绪 ──
    if !mgr.read_status().uv_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::DownloadingUv);
        mgr.report_progress("downloading_uv", PROGRESS_UV_DOWNLOAD.0, "正在下载 uv...");

        match crate::environment::uv::download_uv(mgr, cancel).await {
            Ok(_) => {
                mgr.write_status(|s| s.uv_ready = true);
                mgr.report_progress("downloading_uv", PROGRESS_UV_DOWNLOAD.1, "uv 下载完成");
            }
            Err(e) => {
                let msg = format!("uv 下载失败: {}", e);
                tracing::error!("{}", msg);
                mark_error(mgr, &msg);
                return Err(e);
            }
        }
    }

    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    // ── 阶段 2: 确保 Python 虚拟环境就绪 ──
    // OCR 是否随 venv 同步由用户持久启用标记决定；未启用时仅安装基础依赖。
    if !mgr.read_status().python_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::SyncingVenv);
        mgr.report_progress(
            "syncing_venv",
            PROGRESS_VENV_SYNC.0,
            "正在安装 Python 环境和依赖...",
        );

        match crate::environment::python::ensure_venv(mgr, cancel).await {
            Ok(_) => {
                mgr.write_status(|s| s.python_ready = true);
                mgr.report_progress("syncing_venv", PROGRESS_VENV_SYNC.1, "Python 环境安装完成");
            }
            Err(e) => {
                let msg = format!("Python 环境安装失败: {}", e);
                tracing::error!("{}", msg);
                mark_error(mgr, &msg);
                return Err(e);
            }
        }
    }

    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    // ── 阶段 3: 安装 Playwright Chromium 浏览器 ──
    // 核心自动化能力只要求 Chromium；Firefox/WebKit 为可选浏览器，
    // /api/browsers 会按实际缓存分别探测，不能把本标记等价为三种引擎均已安装。
    if !mgr.read_status().playwright_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::InstallingPlaywright);
        mgr.report_progress(
            "installing_playwright",
            PROGRESS_PLAYWRIGHT.0,
            "正在安装浏览器...",
        );

        match crate::environment::python::install_playwright(mgr, cancel).await {
            Ok(_) => {
                mgr.write_status(|s| s.playwright_ready = true);
                mgr.report_progress(
                    "installing_playwright",
                    PROGRESS_PLAYWRIGHT.1,
                    "浏览器安装完成",
                );
            }
            Err(e) => {
                let msg = format!("Playwright 安装失败: {}", e);
                tracing::error!("{}", msg);
                mark_error(mgr, &msg);
                return Err(e);
            }
        }
    }

    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    mgr.write_status(|s| {
        s.stage = BootstrapStage::Done;
        s.capability_ready = true;
    });
    mgr.report_progress("done", 100, "环境就绪");
    mgr.fire_bootstrap_done();
    tracing::info!("浏览器自动化能力引导完成");
    Ok(())
}

/// 快速路径：检测各组件是否已就绪，更新 EnvironmentStatus。
///
/// `playwright_ready` 特指核心能力需要的 Chromium 是否存在；Firefox/WebKit
/// 通过 [`playwright_browser_installed`] 单独按实际缓存探测。
pub async fn check_environment(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let env_path = mgr.env_path();

    let uv_exe = env_path.join(crate::environment::UV_EXE_NAME);
    let uv_ready = if uv_exe.exists() {
        crate::environment::uv::uv_executable_works(&uv_exe).await
    } else {
        crate::environment::uv::check_uv_on_path().await
    };

    let worker_project_path = mgr.worker_project_path();
    let python_exe = worker_project_path.join(crate::environment::PYTHON_EXE_RELATIVE);
    let python_ready = crate::environment::python::python_executable_works(&python_exe).await;

    let playwright_ready = python_ready && playwright_browser_installed("chromium");

    // Git 仅是开发者辅助能力：开发者模式下检测系统/已有 MinGit，但不自动安装。
    let git_ready = if mgr.git_download_enabled() {
        crate::environment::git::check_git(mgr)
            .await
            .unwrap_or(false)
    } else {
        false
    };

    let capability_ready = uv_ready && python_ready && playwright_ready;

    mgr.write_status(|s: &mut EnvironmentStatus| {
        s.uv_ready = uv_ready;
        s.python_ready = python_ready;
        s.playwright_ready = playwright_ready;
        s.git_ready = git_ready;
        s.capability_ready = capability_ready;
    });

    tracing::debug!(
        "环境检查: uv={}, python={}, playwright={}, git={}, capability={}",
        uv_ready,
        python_ready,
        playwright_ready,
        git_ready,
        capability_ready
    );

    Ok(())
}

/// 检查 Playwright 管理的指定浏览器是否实际安装。
///
/// 支持 `chromium` / `firefox` / `webkit`。判断依据是 Playwright 缓存目录中
/// 对应 `<browser>-*` 子目录存在且非空；未知名称直接返回 false。
/// 优先尊重 `PLAYWRIGHT_BROWSERS_PATH`，否则按操作系统检查 Playwright 默认缓存。
pub fn playwright_browser_installed(browser: &str) -> bool {
    let prefix = match browser {
        "chromium" => "chromium-",
        "firefox" => "firefox-",
        "webkit" => "webkit-",
        _ => return false,
    };

    if let Some(dir) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        return playwright_dir_has_browser(PathBuf::from(dir), prefix);
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            if playwright_dir_has_browser(
                PathBuf::from(local_app_data).join("ms-playwright"),
                prefix,
            ) {
                return true;
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            if playwright_dir_has_browser(
                PathBuf::from(home)
                    .join("Library")
                    .join("Caches")
                    .join("ms-playwright"),
                prefix,
            ) {
                return true;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            if playwright_dir_has_browser(
                PathBuf::from(home).join(".cache").join("ms-playwright"),
                prefix,
            ) {
                return true;
            }
        }
    }

    false
}

/// 检查 ms-playwright 目录下是否存在指定前缀且非空的浏览器子目录。
fn playwright_dir_has_browser(dir: PathBuf, prefix: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(prefix) {
            if let Ok(mut sub) = std::fs::read_dir(entry.path()) {
                if sub.next().is_some() {
                    return true;
                }
            }
        }
    }
    false
}

/// 重试安装（POST /api/system/retry-install 触发）
///
/// 重置状态后重新开始引导。经 BootstrapGate 排他执行（F1）：
/// 显式重装与 ensure_capability 触发的引导共享同一把互斥锁，
/// 防止并发重置/引导踩踏同一 .venv 与固定临时名。
pub async fn retry_install(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    mgr.bootstrap_gate
        .run_exclusive(async {
            // retry 在获得 gate 后才创建新 generation；旧安装此时已经完全退出。
            let cancel = mgr.begin_install_generation();
            mgr.write_status(|s| {
                s.stage = BootstrapStage::Idle;
                s.progress = None;
                s.last_error = None;
            });
            bootstrap_capability(mgr, &cancel).await
        })
        .await
}

/// 标记安装失败状态
fn mark_error(mgr: &EnvironmentManager, message: &str) {
    mgr.write_status(|s| {
        s.stage = BootstrapStage::Error;
        s.last_error = Some(message.to_string());
    });
    mgr.report_progress("error", 0, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playwright_cache_detection_distinguishes_engines() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        std::fs::create_dir_all(dir.path().join("chromium-123").join("chrome"))
            .expect("创建 chromium 缓存");
        std::fs::write(
            dir.path()
                .join("chromium-123")
                .join("chrome")
                .join("marker"),
            b"ok",
        )
        .expect("写入 marker");
        std::fs::create_dir_all(dir.path().join("firefox-456")).expect("创建空 firefox 缓存");

        assert!(playwright_dir_has_browser(
            dir.path().to_path_buf(),
            "chromium-"
        ));
        assert!(!playwright_dir_has_browser(
            dir.path().to_path_buf(),
            "firefox-"
        ));
        assert!(!playwright_dir_has_browser(
            dir.path().to_path_buf(),
            "webkit-"
        ));
    }
}
