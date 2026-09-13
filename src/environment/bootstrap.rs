//! 引导流程编排：核心 + 能力两阶段

use tokio_util::sync::CancellationToken;

use crate::environment::python::tail_chars;
use crate::environment::{
    BootstrapStage, EnvironmentError, EnvironmentManager, EnvironmentStatus, PROGRESS_PLAYWRIGHT,
    PROGRESS_UV_DOWNLOAD, PROGRESS_VENV_SYNC,
};

/// 轻量 Python 引导结果，向 Worker 规划层传递本轮真实同步证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythonBootstrapOutcome {
    /// 本轮是否执行并成功完成了 `uv sync`。
    pub venv_synchronized: bool,
}

/// 引导轻量 Python 运行时（uv -> Python venv）。
///
/// 供默认项目 Python 脚本首次执行使用；不会安装 Playwright 浏览器。
pub async fn bootstrap_python_runtime(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<PythonBootstrapOutcome, EnvironmentError> {
    tracing::info!("开始引导 Python 运行时...");

    // ── 阶段 1: 确保 uv 就绪 ──
    let uv_exe = mgr.env_path().join(crate::environment::UV_EXE_NAME);
    let uv_ready = if uv_exe.exists() {
        crate::environment::uv::uv_executable_works(&uv_exe).await
    } else {
        crate::environment::uv::check_uv_on_path().await
    };
    mgr.write_status(|s| s.uv_ready = uv_ready);
    if !uv_ready {
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
    // 不依赖可能滞后的内存 status，始终实际探测解释器；返回值记录本轮是否 sync，
    // 供 Missing 指纹状态区分“刚创建的新环境”与“状态文件丢失的旧环境”。
    mgr.write_status(|s| s.stage = BootstrapStage::SyncingVenv);
    mgr.report_progress(
        "syncing_venv",
        PROGRESS_VENV_SYNC.0,
        "正在检查 Python 环境和依赖...",
    );
    let venv = match crate::environment::python::ensure_venv_with_state(mgr, cancel).await {
        Ok(outcome) => outcome,
        Err(e) => {
            let msg = format!("Python 环境安装失败: {}", e);
            tracing::error!("{}", msg);
            mark_error(mgr, &msg);
            return Err(e);
        }
    };
    mgr.write_status(|s| s.python_ready = true);
    mgr.report_progress("syncing_venv", PROGRESS_VENV_SYNC.1, "Python 环境已就绪");

    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }

    Ok(PythonBootstrapOutcome {
        venv_synchronized: venv.synchronized,
    })
}

/// 引导安装完整浏览器自动化能力（uv -> Python venv -> Playwright）。
///
/// Python 运行时阶段与脚本执行共用，避免维护两套 uv / venv 引导逻辑。
pub async fn bootstrap_capability(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    bootstrap_capability_inner(mgr, cancel, false).await
}

async fn bootstrap_capability_inner(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
    force_worker_sync: bool,
) -> Result<(), EnvironmentError> {
    tracing::info!("开始引导浏览器自动化能力...");
    bootstrap_worker_runtime(mgr, cancel, force_worker_sync).await?;

    // ── 阶段 3: 安装 Playwright Chromium 浏览器 ──
    // 核心自动化能力只要求 Chromium；Firefox/WebKit 为可选浏览器，
    // /api/browsers 会按实际缓存分别探测，不能把本标记等价为三种引擎均已安装。
    //
    // 策略（Windows 尤其重要）：**系统浏览器存在时不下载 Chromium**。
    // Windows 出厂自带 Edge（默认渠道 msedge，见 `schema.rs` 的 BrowserSettings 默认值），
    // Playwright 可经 channel 直连系统浏览器，无需 Chromium 内核——避免为绝大多数用户
    // 白占约 150MB 磁盘。Chromium 只在两种情况下安装：
    //   1) 全无系统浏览器时的兜底自愈（下面这个分支）；
    //   2) 用户在设置页显式点击安装（`POST /api/install/playwright` →
    //      `perform_playwright_install`，见 `web/routes/system.rs`）。
    // 读取即释放：作用域内取快照，避免 RwLock 读守卫跨越后续 await 使 future 失去 Send
    let need_managed_chromium = {
        let status = mgr.read_status();
        should_download_managed_chromium(status.playwright_ready, status.system_browser_ready)
    };

    if need_managed_chromium {
        mgr.write_status(|s| s.stage = BootstrapStage::InstallingPlaywright);
        mgr.report_progress(
            "installing_playwright",
            PROGRESS_PLAYWRIGHT.0,
            "正在安装浏览器...",
        );

        match crate::environment::python::install_playwright(mgr, cancel).await {
            Ok(_) => {
                // 不在此处置 playwright_ready：统一由下方的
                // refresh_browser_and_capability_status 按同一口径重算，避免同一字段多套赋值
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

    refresh_browser_and_capability_status(mgr);
    if !mgr.read_status().capability_ready {
        let reason = mgr
            .read_status()
            .last_error
            .clone()
            .unwrap_or_else(|| "浏览器自动化能力最终验证未通过".to_string());
        mark_error(mgr, &reason);
        return Err(EnvironmentError::WorkerRuntimeInvalid { reason });
    }
    mgr.write_status(|s| s.stage = BootstrapStage::Done);
    mgr.report_progress("done", 100, "环境就绪");
    tracing::info!("浏览器自动化能力引导完成");
    Ok(())
}

/// 引导并验证 Worker 核心运行时，不要求浏览器二进制。
pub async fn bootstrap_worker_runtime(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
    force_sync: bool,
) -> Result<(), EnvironmentError> {
    let python = bootstrap_python_runtime(mgr, cancel).await?;
    mgr.write_status(|s| s.stage = BootstrapStage::VerifyingWorker);
    mgr.report_progress(
        "verifying_worker",
        PROGRESS_VENV_SYNC.1,
        "正在验证 Worker 核心依赖与版本...",
    );
    if let Err(error) = crate::environment::health::ensure_worker_runtime(
        mgr,
        cancel,
        force_sync,
        python.venv_synchronized,
    )
    .await
    {
        let message = format!("Worker 环境修复失败: {error}");
        mark_error(mgr, &message);
        return Err(error);
    }

    mgr.write_status(|s| s.stage = BootstrapStage::ApplyingOcr);
    mgr.report_progress("applying_ocr", 58, "正在对齐 OCR 可选依赖...");
    if let Err(error) = crate::environment::uv::reconcile_ocr_preference(mgr, cancel).await {
        // OCR 是补充能力：操作失败且清单已回滚时允许核心 Worker 继续；若 uv 已改写
        // 清单但后续验证失败，则下方指纹完整性检查会拒绝把环境误标为就绪。
        tracing::warn!("OCR 可选依赖对齐失败，将按清单完整性决定核心 Worker 状态: {error}");
    }

    let manifest_current = matches!(
        crate::environment::health::runtime_manifest_state(mgr),
        Ok(crate::environment::health::ManifestState::Current)
    );
    if !manifest_current {
        let reason = "OCR 对齐后 Python 依赖清单未通过完整性验证".to_string();
        mark_error(mgr, &reason);
        return Err(EnvironmentError::WorkerRuntimeInvalid { reason });
    }
    mgr.write_status(|s| {
        s.python_ready = true;
        s.worker_ready = true;
        s.manifest_current = manifest_current;
        s.last_error = None;
    });
    refresh_browser_and_capability_status(mgr);
    mgr.report_progress(
        "worker_ready",
        PROGRESS_VENV_SYNC.1,
        "Worker 核心环境已验证",
    );
    mgr.fire_bootstrap_done();
    Ok(())
}

/// 只做浏览器文件系统探测并重算派生能力，不重复启动 Python/Worker import 探针。
///
/// 供引导收尾与**显式安装浏览器成功后**复用：安装改变了 `ms-playwright` 内容，
/// 必须立刻重算 `playwright_ready` / `capability_ready`，否则状态与界面会滞后，
/// 登录侧的渠道探测缓存也不会失效（其缓存键含这两个标志）。
pub(crate) fn refresh_browser_and_capability_status(mgr: &EnvironmentManager) {
    let managed_browser_ready = playwright_browser_installed(mgr, "chromium");
    let system_browser_ready = crate::browser::system_browser_available();
    mgr.write_status(|s| {
        // 与 check_environment 同口径：表示「Python 运行时 + Chromium 二进制」均就绪。
        // 少了 python_ready 会出现「界面显示浏览器已就绪、capability_ready 却为假」的自相矛盾。
        s.playwright_ready = s.python_ready && managed_browser_ready;
        s.system_browser_ready = system_browser_ready;
        s.capability_ready = derive_capability_ready(
            s.uv_ready,
            s.python_ready,
            s.worker_ready,
            s.manifest_current,
            managed_browser_ready,
            system_browser_ready,
        );
    });
}

/// 快速路径：检测各组件是否已就绪，更新 EnvironmentStatus。
///
/// `playwright_ready` 特指核心能力需要的 Chromium 是否存在；Firefox/WebKit
/// 通过 [`playwright_browser_installed`] 单独按实际缓存探测。`capability_ready`
/// 另计系统浏览器：有 Edge/Chrome 即视为具备自动化能力，不强制下载 Chromium。
pub async fn check_environment(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    mgr.write_status(|s| s.stage = BootstrapStage::Checking);
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

    let manifest_state = if python_ready {
        match crate::environment::health::runtime_manifest_state(mgr) {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!("依赖指纹探测失败，视为未就绪: {error}");
                crate::environment::health::ManifestState::Stale
            }
        }
    } else {
        crate::environment::health::ManifestState::Missing
    };
    let manifest_current = manifest_state == crate::environment::health::ManifestState::Current;
    // 清单不可信时最终必需 sync，先 import Worker 没有决策价值，还会把冷启动
    // 重模块加载成本白付一次；仅 Current 状态执行运行时探针。
    let worker_probe = if python_ready && manifest_current {
        crate::environment::health::probe_worker_runtime(mgr).await
    } else {
        crate::environment::health::WorkerRuntimeProbe {
            ready: false,
            version: None,
            error: None,
        }
    };
    let playwright_ready = python_ready && playwright_browser_installed(mgr, "chromium");
    let system_browser_ready = crate::browser::system_browser_available();
    let ocr_enabled = crate::environment::health::ocr_enabled(mgr).unwrap_or_else(|error| {
        tracing::warn!("OCR 偏好读取失败，安全回退为未启用: {error}");
        false
    });
    let ocr_ready = crate::environment::python::ddddocr_installed(mgr);

    let capability_ready = derive_capability_ready(
        uv_ready,
        python_ready,
        worker_probe.ready,
        manifest_current,
        playwright_ready,
        system_browser_ready,
    );
    let diagnostic = if python_ready && !manifest_current {
        Some("Python 依赖清单尚未通过当前版本验证，将在首次使用时自动同步".to_string())
    } else if python_ready && !worker_probe.ready {
        worker_probe.error.clone()
    } else if worker_probe.ready && !(playwright_ready || system_browser_ready) {
        Some("未检测到可用浏览器，首次使用时将自动安装 Chromium".to_string())
    } else {
        None
    };

    mgr.write_status(|s: &mut EnvironmentStatus| {
        s.uv_ready = uv_ready;
        s.python_ready = python_ready;
        s.worker_ready = worker_probe.ready;
        s.manifest_current = manifest_current;
        s.playwright_ready = playwright_ready;
        s.system_browser_ready = system_browser_ready;
        s.ocr_enabled = ocr_enabled;
        s.ocr_ready = ocr_ready;
        s.capability_ready = capability_ready;
        s.last_error = diagnostic;
        if s.stage == BootstrapStage::Checking {
            s.stage = if capability_ready {
                BootstrapStage::Done
            } else {
                BootstrapStage::Idle
            };
        }
    });

    // 首轮探测（容器启动路径）升 info，供用户从日志确认环境真实状态；
    // 后续（引导流程内的复用探测）保持 debug，避免刷屏。进程级静态标记是
    // 不改函数签名的最小区分方式——容器启动即 spawn 本函数，几乎必然是首调用方。
    let summary = format!(
        "uv={uv_ready}, python={python_ready}, worker={}, manifest={manifest_current}, playwright={playwright_ready}, system_browser={system_browser_ready}, ocr={ocr_ready}/{ocr_enabled}, capability={capability_ready}",
        worker_probe.ready
    );
    if !FIRST_CHECK_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("环境检查: {summary}");
    } else {
        tracing::debug!("环境检查: {summary}");
    }

    Ok(())
}

/// 汇总完整浏览器能力；系统浏览器只能替代浏览器下载，不能替代 Python 包。
fn derive_capability_ready(
    uv_ready: bool,
    python_ready: bool,
    worker_ready: bool,
    manifest_current: bool,
    managed_browser_ready: bool,
    system_browser_ready: bool,
) -> bool {
    uv_ready
        && python_ready
        && worker_ready
        && manifest_current
        && (managed_browser_ready || system_browser_ready)
}

/// 是否需要下载 Playwright 托管的 Chromium
///
/// 仅当「Chromium 尚未安装」且「系统没有可用浏览器（Edge/Chrome）」时才自动下载；
/// 有系统浏览器时由 Playwright 经 channel 直连，省下约 150MB 磁盘。
/// 用户显式安装（`POST /api/install/playwright`）不走本判定，见调用方注释。
fn should_download_managed_chromium(playwright_ready: bool, system_browser_ready: bool) -> bool {
    !playwright_ready && !system_browser_ready
}

/// 是否已完成过首轮环境探测（首个调用方视为容器启动路径，探测摘要升 info）
static FIRST_CHECK_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 检查 Playwright 管理的指定浏览器是否已安装完成
///
/// 支持 `chromium` / `firefox` / `webkit`；未知名称返回 false。
/// 判定基于 Playwright 自身的 registry（`browsers.json`）与 `INSTALLATION_COMPLETE`
/// 完成标记，而非「缓存目录非空」——细节见 [`crate::environment::browser_registry`]。
///
/// chromium 要求 `chromium-<rev>` 与 `chromium_headless_shell-<rev>` **两套都完整**：
/// 应用自身的 `playwright install chromium` 在 registry 中把两者都标为默认安装项，
/// 因此这正是「安装完整」的语义，且使判定不依赖 `headless` 配置。
pub fn playwright_browser_installed(mgr: &EnvironmentManager, engine: &str) -> bool {
    crate::environment::missing_components(mgr, engine).is_empty()
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
            bootstrap_capability_inner(mgr, &cancel, true).await
        })
        .await
}

/// 标记安装失败状态（message 会截断到 600 字符，避免超长 stderr 撑爆快照/日志）
fn mark_error(mgr: &EnvironmentManager, message: &str) {
    // 复用 python.rs 的 tail_chars（与 uv/playwright 输出截断同一实现，避免重复定义）
    let truncated = tail_chars(message, 600);
    mgr.write_status(|s| {
        s.stage = BootstrapStage::Error;
        s.last_error = Some(truncated.clone());
    });
    mgr.report_progress("error", 0, &truncated);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_browser_cannot_mask_missing_worker_package() {
        assert!(!derive_capability_ready(
            true, true, false, true, false, true
        ));
        assert!(!derive_capability_ready(
            true, true, true, false, false, true
        ));
        assert!(derive_capability_ready(true, true, true, true, false, true));
    }

    /// Windows 默认装 Edge：系统浏览器存在时不得自动下载 Chromium（省磁盘）
    #[test]
    fn managed_chromium_download_skipped_when_system_browser_exists() {
        // 有 Edge/Chrome → 不下载
        assert!(!should_download_managed_chromium(false, true));
        // 已装 Chromium → 不重复下载
        assert!(!should_download_managed_chromium(true, true));
        assert!(!should_download_managed_chromium(true, false));
        // 两者都没有 → 兜底下载，保证自愈
        assert!(should_download_managed_chromium(false, false));
    }
}
