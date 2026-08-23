//! 引导流程编排：核心 + 能力两阶段

use std::path::PathBuf;

use crate::environment::{
    BootstrapStage, EnvironmentError, EnvironmentManager, EnvironmentStatus, PROGRESS_MINGIT,
    PROGRESS_PLAYWRIGHT, PROGRESS_UV_DOWNLOAD, PROGRESS_VENV_SYNC,
};

/// 引导安装完整浏览器自动化能力（uv -> Python venv -> Playwright）
///
/// 四阶段流程：
/// 1. 确保 uv 就绪（下载或跳过）
/// 2. 确保 Python 虚拟环境就绪（uv sync 或跳过）
/// 3. 安装 Playwright Chromium 浏览器
/// 4. 可选安装 MinGit（仅开发者模式按需）
pub async fn bootstrap_capability(
    mgr: &EnvironmentManager,
) -> Result<(), EnvironmentError> {
    tracing::info!("开始引导浏览器自动化能力...");

    // 先做一次快速检查，跳过已就绪的阶段
    check_environment(mgr).await?;

    // ── 阶段 1: 确保 uv 就绪 ──
    if !mgr.read_status().uv_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::DownloadingUv);
        mgr.report_progress(
            "downloading_uv",
            PROGRESS_UV_DOWNLOAD.0,
            "正在下载 uv...",
        );

        match crate::environment::uv::download_uv(mgr).await {
            Ok(_) => {
                mgr.write_status(|s| s.uv_ready = true);
                mgr.report_progress(
                    "downloading_uv",
                    PROGRESS_UV_DOWNLOAD.1,
                    "uv 下载完成",
                );
            }
            Err(e) => {
                let msg = format!("uv 下载失败: {}", e);
                tracing::error!("{}", msg);
                mark_error(mgr, &msg);
                return Err(e);
            }
        }
    }

    // 检查取消
    if mgr.cancel_token().is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    // ── 阶段 2: 确保 Python 虚拟环境就绪 ──
    // 仅创建 venv（基础依赖）；OCR 依赖（ddddocr）由前端显式经
    // `uv add/remove` 单独管理，不在此自动补装。
    if !mgr.read_status().python_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::SyncingVenv);
        mgr.report_progress(
            "syncing_venv",
            PROGRESS_VENV_SYNC.0,
            "正在安装 Python 环境和依赖...",
        );

        match crate::environment::python::ensure_venv(mgr).await {
            Ok(_) => {
                mgr.write_status(|s| s.python_ready = true);
                mgr.report_progress(
                    "syncing_venv",
                    PROGRESS_VENV_SYNC.1,
                    "Python 环境安装完成",
                );
            }
            Err(e) => {
                let msg = format!("Python 环境安装失败: {}", e);
                tracing::error!("{}", msg);
                mark_error(mgr, &msg);
                return Err(e);
            }
        }
    }

    // 检查取消
    if mgr.cancel_token().is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    // ── 阶段 3: 安装 Playwright Chromium 浏览器 ──
    if !mgr.read_status().playwright_ready {
        mgr.write_status(|s| s.stage = BootstrapStage::InstallingPlaywright);
        mgr.report_progress(
            "installing_playwright",
            PROGRESS_PLAYWRIGHT.0,
            "正在安装浏览器...",
        );

        match crate::environment::python::install_playwright(mgr).await {
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

    // 检查取消
    if mgr.cancel_token().is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    // ── 阶段 4: 可选安装 MinGit（仅开发者模式启用） ──
    if !mgr.read_status().git_ready && mgr.git_download_enabled() {
        mgr.write_status(|s| s.stage = BootstrapStage::DownloadingMinGit);
        mgr.report_progress(
            "downloading_mingit",
            PROGRESS_MINGIT.0,
            "正在下载 MinGit...",
        );

        match crate::environment::git::download_mingit(mgr).await {
            Ok(_) => {
                mgr.write_status(|s| s.git_ready = true);
                mgr.report_progress(
                    "downloading_mingit",
                    PROGRESS_MINGIT.1,
                    "MinGit 下载完成",
                );
            }
            Err(e) => {
                // MinGit 为可选组件，失败不阻断引导流程
                tracing::warn!("MinGit 下载失败（可选组件，不影响核心功能）: {}", e);
            }
        }
    }

    // ── 全部完成 ──
    mgr.write_status(|s| {
        s.stage = BootstrapStage::Done;
        s.capability_ready = true;
    });
    mgr.report_progress("done", 100, "环境就绪");

    // 环境重建成功：复位 Bridge 连续 spawn 失败熔断（B3）
    mgr.fire_bootstrap_done();

    tracing::info!("浏览器自动化能力引导完成");
    Ok(())
}

/// 快速路径：检测各组件是否已就绪，更新 EnvironmentStatus
///
/// 每次启动时同步执行（毫秒级），判断 uv / Python / Playwright / Git 是否就绪。
pub async fn check_environment(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let env_path = mgr.env_path();

    // 1. 检查 uv（优先本地，其次系统 PATH + 最低版本校验）
    let uv_exe = env_path.join(crate::environment::UV_EXE_NAME);
    let uv_ready = if uv_exe.exists() {
        true
    } else {
        // PATH 上的 uv 需满足最低版本要求，否则视为未就绪（触发下载最新版）
        crate::environment::uv::check_uv_on_path().await
    };

    // 2. 检查 Python 虚拟环境
    let worker_project_path = mgr.worker_project_path();
    let python_exe = worker_project_path.join(crate::environment::PYTHON_EXE_RELATIVE);
    let python_ready = crate::environment::python::python_executable_works(&python_exe).await;

    // 3. 检查 Playwright Chromium
    //    通过检查 ms-playwright 缓存目录判断
    let playwright_ready = if python_ready {
        check_playwright_chromium_installed()
    } else {
        false
    };

    // 4. 检查 Git（可选）
    let git_ready = crate::environment::git::check_git(mgr).await.unwrap_or(false);

    // 5. 综合判定
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

/// 检查 Playwright Chromium 是否已安装
///
/// 通过检查 ms-playwright 缓存目录判断 Chromium 浏览器是否存在。
/// 优先读取 `PLAYWRIGHT_BROWSERS_PATH` 环境变量覆盖默认缓存位置；
/// chromium-* 目录需非空才算已安装（避免下载中断留空的残目录被误判）。
fn check_playwright_chromium_installed() -> bool {
    // 方式 0: PLAYWRIGHT_BROWSERS_PATH 环境变量覆盖默认位置
    if let Some(dir) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        return playwright_dir_has_chromium(PathBuf::from(dir));
    }

    // 方式 1: Windows %LOCALAPPDATA%/ms-playwright/
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            if playwright_dir_has_chromium(
                PathBuf::from(local_app_data).join("ms-playwright"),
            ) {
                return true;
            }
        }
    }

    // 方式 2: macOS ~/Library/Caches/ms-playwright/
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            if playwright_dir_has_chromium(
                PathBuf::from(home)
                    .join("Library")
                    .join("Caches")
                    .join("ms-playwright"),
            ) {
                return true;
            }
        }
    }

    // 方式 3: Linux ~/.cache/ms-playwright/
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            if playwright_dir_has_chromium(
                PathBuf::from(home).join(".cache").join("ms-playwright"),
            ) {
                return true;
            }
        }
    }

    false
}

/// 检查 ms-playwright 目录下是否存在非空的 chromium-* 子目录
fn playwright_dir_has_chromium(dir: PathBuf) -> bool {
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("chromium-") {
            // 目录非空（至少含一个条目）才算已安装；空/下载中断的残目录视为未安装
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
/// 重置状态后重新开始引导。
pub async fn retry_install(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    // 重置状态后重新开始引导
    mgr.write_status(|s| {
        s.stage = BootstrapStage::Idle;
        s.progress = None;
        s.last_error = None;
    });
    bootstrap_capability(mgr).await
}

/// 标记安装失败状态
fn mark_error(mgr: &EnvironmentManager, message: &str) {
    mgr.write_status(|s| {
        s.stage = BootstrapStage::Error;
        s.last_error = Some(message.to_string());
    });
    mgr.report_progress("error", 0, message);
}
