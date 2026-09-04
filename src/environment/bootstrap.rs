//! 引导流程编排：核心 + 能力两阶段

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::environment::{
    BootstrapStage, EnvironmentError, EnvironmentManager, EnvironmentStatus, PROGRESS_PLAYWRIGHT,
    PROGRESS_UV_DOWNLOAD, PROGRESS_VENV_SYNC,
};

/// 引导轻量 Python 运行时（uv -> Python venv）。
///
/// 供默认项目 Python 脚本首次执行使用；不会安装 Playwright 浏览器。
pub async fn bootstrap_python_runtime(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    tracing::info!("开始引导 Python 运行时...");

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

    Ok(())
}

/// 引导安装完整浏览器自动化能力（uv -> Python venv -> Playwright）。
///
/// Python 运行时阶段与脚本执行共用，避免维护两套 uv / venv 引导逻辑。
pub async fn bootstrap_capability(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    tracing::info!("开始引导浏览器自动化能力...");
    bootstrap_python_runtime(mgr, cancel).await?;

    // ── 阶段 3: 安装 Playwright Chromium 浏览器 ──
    // 核心自动化能力只要求 Chromium；Firefox/WebKit 为可选浏览器，
    // /api/browsers 会按实际缓存分别探测，不能把本标记等价为三种引擎均已安装。
    // 系统已装 Edge/Chrome 时跳过下载：Playwright 经 channel 直连系统浏览器，
    // 无需 Chromium 内核；全无可用浏览器时仍下载 Chromium 兜底自愈。
    if !mgr.read_status().playwright_ready && !crate::browser::system_browser_available() {
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
    } else if !mgr.read_status().playwright_ready {
        tracing::info!("检测到系统浏览器（Edge/Chrome），跳过 Chromium 下载");
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
/// 通过 [`playwright_browser_installed`] 单独按实际缓存探测。`capability_ready`
/// 另计系统浏览器：有 Edge/Chrome 即视为具备自动化能力，不强制下载 Chromium。
pub async fn check_environment(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let env_path = mgr.env_path();

    let uv_exe = env_path.join(crate::environment::UV_EXE_NAME);
    let uv_ready = if uv_exe.exists() {
        crate::environment::uv::uv_executable_works(&uv_exe).await
    } else {
        crate::environment::uv::check_uv_on_path().await
    };
    if !uv_ready {
        // 探测函数仅返回 bool（拿不到命令错误细节），按探测对象区分失败原因
        if uv_exe.exists() {
            tracing::debug!(
                path = %uv_exe.display(),
                "uv --version 探测未通过（文件存在但无法执行），视为未就绪"
            );
        } else {
            tracing::debug!("PATH 上未找到可用的 uv，视为未就绪");
        }
    }

    let worker_project_path = mgr.worker_project_path();
    let python_exe = worker_project_path.join(crate::environment::PYTHON_EXE_RELATIVE);
    let python_ready = crate::environment::python::python_executable_works(&python_exe).await;
    if !python_ready {
        tracing::debug!(
            path = %python_exe.display(),
            "python --version 探测未通过（文件缺失或无法执行），视为未就绪"
        );
    }

    let playwright_ready = python_ready && playwright_browser_installed("chromium");
    let system_browser_ready = crate::browser::system_browser_available();

    let capability_ready = uv_ready && python_ready && (playwright_ready || system_browser_ready);

    mgr.write_status(|s: &mut EnvironmentStatus| {
        s.uv_ready = uv_ready;
        s.python_ready = python_ready;
        s.playwright_ready = playwright_ready;
        s.capability_ready = capability_ready;
    });

    // 首轮探测（容器启动路径）升 info，供用户从日志确认环境真实状态；
    // 后续（引导流程内的复用探测）保持 debug，避免刷屏。进程级静态标记是
    // 不改函数签名的最小区分方式——容器启动即 spawn 本函数，几乎必然是首调用方。
    let summary = format!(
        "uv={uv_ready}, python={python_ready}, playwright={playwright_ready}, system_browser={system_browser_ready}, capability={capability_ready}"
    );
    if !FIRST_CHECK_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("环境检查: {summary}");
    } else {
        tracing::debug!("环境检查: {summary}");
    }

    Ok(())
}

/// 是否已完成过首轮环境探测（首个调用方视为容器启动路径，探测摘要升 info）
static FIRST_CHECK_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 检查 Playwright 管理的指定浏览器是否实际安装。
///
/// 支持 `chromium` / `firefox` / `webkit`。判断依据是 Playwright 缓存目录中
/// 对应 `<browser>-*` 子目录存在且非空；未知名称直接返回 false。
/// 优先尊重 `PLAYWRIGHT_BROWSERS_PATH`，否则按操作系统检查 Playwright 默认缓存；
/// 空值 / `"0"` 表示无独立缓存（与卸载侧 `playwright_cache_dir` 同口径），同样走默认缓存。
pub fn playwright_browser_installed(browser: &str) -> bool {
    let prefix = match browser {
        "chromium" => "chromium-",
        "firefox" => "firefox-",
        "webkit" => "webkit-",
        _ => return false,
    };

    if let Some(dir) =
        resolve_custom_browsers_dir(std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").as_deref())
    {
        return playwright_dir_has_browser(dir, prefix);
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
/// 解析自定义浏览器缓存目录：未设置 / 空 / `"0"` 返回 `None`（回退 OS 默认），纯函数便于单测。
fn resolve_custom_browsers_dir(var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let dir = var?;
    if dir.is_empty() || dir == "0" {
        return None;
    }
    Some(PathBuf::from(dir))
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
    /// 自定义目录解析：未设置/空/"0" 回退默认（None），其余原样透出。
    #[test]
    fn custom_browsers_dir_exempts_empty_and_zero() {
        use std::ffi::OsStr;
        assert_eq!(resolve_custom_browsers_dir(None), None);
        assert_eq!(resolve_custom_browsers_dir(Some(OsStr::new(""))), None);
        assert_eq!(resolve_custom_browsers_dir(Some(OsStr::new("0"))), None);
        assert_eq!(
            resolve_custom_browsers_dir(Some(OsStr::new("/tmp/pw-browsers"))),
            Some(PathBuf::from("/tmp/pw-browsers"))
        );
    }
}
