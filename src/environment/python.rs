//! Python 安装：uv sync + Playwright 浏览器安装

use crate::environment::{
    EnvironmentError, EnvironmentManager, PLAYWRIGHT_INSTALL_MAX_RETRIES,
    PLAYWRIGHT_INSTALL_RETRY_DELAY, PLAYWRIGHT_INSTALL_TIMEOUT, uv_exe_path,
};

use std::path::Path;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

/// 虚拟环境确保结果，供上层区分“复用既有环境”与“本轮已按当前清单同步”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VenvEnsureResult {
    /// Python 解释器路径。
    pub python_exe: std::path::PathBuf,
    /// 本轮是否成功执行过 `uv sync`。
    pub synchronized: bool,
}

/// 取字符串末尾至多 `max_chars` 个字符；超出时按字符边界截断并注明省略长度
///（Playwright 安装输出可达数 MB，全量进错误消息会撑爆日志与状态快照）
pub(crate) fn tail_chars(s: &str, max_chars: usize) -> String {
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
/// 检查解释器是否真实可启动，不可用则执行 `uv sync` 创建/修复。
/// 本函数只回答“Python 能否运行”；Worker import、协议版本与依赖清单指纹由
/// `environment::health` 独立验证，避免普通 Python 脚本被浏览器能力绑死。
/// 返回 Python 解释器路径。
pub async fn ensure_venv(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<std::path::PathBuf, EnvironmentError> {
    Ok(ensure_venv_with_state(mgr, cancel).await?.python_exe)
}

/// 确保 Python 虚拟环境就绪，并返回本轮是否已按当前清单同步。
///
/// `ManifestState::Missing` 是否可以安全认领依赖指纹，取决于这个同步证据；
/// 因此该信息必须由真正执行 `uv sync` 的层向上返回，不能用文件时间猜测。
pub(crate) async fn ensure_venv_with_state(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<VenvEnsureResult, EnvironmentError> {
    let python_exe = mgr
        .worker_project_path()
        .join(crate::environment::PYTHON_EXE_RELATIVE);

    // 文件存在不代表 uv 管理的基础解释器仍存在，必须实际启动一次。
    let failure_reason = if let Err(reason) = python_executable_status(&python_exe).await {
        // 补充探测失败的具体原因（缺失 / 启动失败 / 超时），便于定位 venv 损坏
        tracing::debug!(reason = %reason, "Python 解释器探测未通过，虚拟环境需要修复");
        reason
    } else {
        return Ok(VenvEnsureResult {
            python_exe,
            synchronized: false,
        });
    };

    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);
    let backup = if venv_path.exists() {
        // uv sync 会信任现有 pyvenv.cfg；若其 home 指向已删除的 uv 托管 Python，
        // sync 只改包目录却不会重建解释器。先原地隔离，才能让 uv 创建新 venv。
        tracing::warn!(reason = %failure_reason, "虚拟环境损坏，隔离旧目录后重建");
        Some(quarantine_broken_venv(&venv_path).await?)
    } else {
        tracing::info!("虚拟环境不存在，执行 uv sync 创建...");
        None
    };

    if let Err(error) = crate::environment::uv::run_uv_sync(mgr, cancel).await {
        restore_quarantined_venv(&venv_path, backup.as_deref()).await;
        return Err(error);
    }

    // 验证创建成功
    if !python_executable_works(&python_exe).await {
        restore_quarantined_venv(&venv_path, backup.as_deref()).await;
        return Err(EnvironmentError::VenvCorrupted);
    }

    if let Some(backup) = backup
        && let Err(error) = tokio::fs::remove_dir_all(&backup).await
    {
        tracing::warn!(path = %backup.display(), "新虚拟环境已验证，但旧环境备份清理失败: {error}");
    }

    Ok(VenvEnsureResult {
        python_exe,
        synchronized: true,
    })
}

/// 将损坏 venv 原地改名，保证重建失败时仍可回滚，且 rename 不跨文件系统。
async fn quarantine_broken_venv(venv_path: &Path) -> Result<std::path::PathBuf, EnvironmentError> {
    let parent = venv_path.parent().unwrap_or_else(|| Path::new("."));
    let mut suffix = 0u16;
    let backup = loop {
        let candidate = parent.join(format!(".venv-rebuild-{}-{suffix}", std::process::id()));
        if !candidate.exists() {
            break candidate;
        }
        suffix = suffix.saturating_add(1);
        if suffix == u16::MAX {
            return Err(EnvironmentError::VenvRebuildFailed {
                path: venv_path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "无法分配 venv 隔离目录",
                ),
            });
        }
    };
    tokio::fs::rename(venv_path, &backup)
        .await
        .map_err(|source| EnvironmentError::VenvRebuildFailed {
            path: venv_path.to_path_buf(),
            source,
        })?;
    Ok(backup)
}

/// 重建失败时恢复旧目录；回滚失败只记错误，保留隔离目录供人工恢复。
async fn restore_quarantined_venv(venv_path: &Path, backup: Option<&Path>) {
    let Some(backup) = backup else {
        return;
    };
    if venv_path.exists()
        && let Err(error) = tokio::fs::remove_dir_all(venv_path).await
    {
        tracing::error!(path = %venv_path.display(), "清理失败的新虚拟环境时出错: {error}");
        return;
    }
    if let Err(error) = tokio::fs::rename(backup, venv_path).await {
        tracing::error!(
            backup = %backup.display(),
            target = %venv_path.display(),
            "虚拟环境回滚失败，隔离备份仍保留: {error}"
        );
    }
}

/// 检查 venv 内 ddddocr（OCR 依赖）是否已安装
///
/// 通过 site-packages 下存在 `ddddocr` 包目录或 `ddddocr-*.dist-info` 判定，
/// 兼容 Windows（Lib/site-packages）与 Unix（lib/python3.x/site-packages）布局。
pub(crate) fn ddddocr_installed(mgr: &EnvironmentManager) -> bool {
    // site-packages 布局解析与浏览器 registry 读取共用同一实现（单点维护）
    for site in crate::environment::site_packages_candidates(mgr) {
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

/// Worker 工程是否支持 OCR（按需 `uv add/remove ddddocr`，不再预声明）。
///
/// ddddocr 不预声明于 pyproject，避免裸 `uv sync` 解析/同步失败；
/// 工程存在即具备随时安装的能力。`declared` 不等于已安装。
pub(crate) fn ocr_declared(mgr: &EnvironmentManager) -> bool {
    mgr.worker_project_path().join("pyproject.toml").is_file()
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
    let mut timed_out = false;
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
        // 下载源默认走 npmmirror 镜像（见 `playwright_download_host`，已显式设置则尊重）；
        // 代理不显式设置：子进程默认继承系统环境（HTTPS_PROXY 等），即系统代理。
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
        .env(
            "PLAYWRIGHT_DOWNLOAD_HOST",
            crate::environment::playwright_download_host(),
        )
        .env(
            "PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT",
            crate::environment::PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT_MS.to_string(),
        )
        .current_dir(mgr.base_path());

        // 流式执行：输出透出 + 存活进度（防数分钟"假死"观感），超时/取消杀整棵树。
        let started = Instant::now();
        let mut last_report = started;
        let mut last_line = String::new();
        let result = crate::environment::uv::command_output_streaming(
            cmd,
            PLAYWRIGHT_INSTALL_TIMEOUT,
            cancel,
            |line| {
                let line = line.trim();
                if line.is_empty() {
                    return;
                }
                tracing::debug!(target: "playwright_install", "{line}");
                last_line.clear();
                last_line.push_str(line);
                // 至多 5s 上报一次：60→84 按耗时爬行，消息带最新输出行（截断防刷屏）。
                let now = Instant::now();
                if now.duration_since(last_report) >= Duration::from_secs(5) {
                    last_report = now;
                    let elapsed = now.duration_since(started).as_secs();
                    let pct = 60 + (elapsed * 24 / PLAYWRIGHT_INSTALL_TIMEOUT.as_secs()).min(24);
                    let tail: String = last_line
                        .chars()
                        .rev()
                        .take(80)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    let msg = if tail.len() < last_line.len() {
                        format!("正在安装 {browser}（…{tail}）...")
                    } else {
                        format!("正在安装 {browser}（{tail}）...")
                    };
                    mgr.report_progress("installing_playwright", pct as u8, &msg);
                }
            },
        )
        .await;

        match result {
            Ok(output) if output.status.success() => {
                tracing::info!("Playwright {browser} 安装成功");
                // 安装改变了 ms-playwright 内容：立刻重算浏览器/能力状态，
                // 让界面与登录侧渠道判定立即看到新装的浏览器（ENV-1 自愈链路）
                crate::environment::bootstrap::refresh_browser_and_capability_status(mgr);
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
                timed_out = true;
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

    // 所有重试均失败：原因归类 + 可操作建议（设置页原样展示，见 SystemSettings）。
    let (cause, hint) = if timed_out {
        classify_playwright_failure("timeout")
    } else {
        classify_playwright_failure(&last_err_msg)
    };
    let detail = tail_chars(&last_err_msg, 300);
    let mut message = format!("原因：{cause}\n建议：{hint}\n详情：{detail}");
    // 设置页单段展示，上限防刷屏（原因+建议常驻，只截详情）。
    const MAX_FAILURE_CHARS: usize = 600;
    if message.chars().count() > MAX_FAILURE_CHARS {
        let keep = detail
            .chars()
            .take(
                detail
                    .chars()
                    .count()
                    .saturating_sub(message.chars().count() - MAX_FAILURE_CHARS),
            )
            .collect::<String>();
        message = format!("原因：{cause}\n建议：{hint}\n详情：{keep}");
    }
    Err(EnvironmentError::PlaywrightInstallFailed {
        retries: PLAYWRIGHT_INSTALL_MAX_RETRIES,
        message,
    })
}

/// 归类浏览器下载失败原因 →（原因，操作建议），供失败提示展示。
///
/// 纯函数：输入为各次尝试的末尾输出拼接（小写匹配，兼容中英文报错）。
fn classify_playwright_failure(text: &str) -> (&'static str, &'static str) {
    let lower = text.to_lowercase();
    // 超时：连接太慢或被拦截（调用方超时即传 "timeout" 直达本分支）。
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("超时")
        || lower.contains("deadline")
    {
        return (
            "下载超时",
            "网络太慢或连接被拦截：检查系统代理与防火墙，点“初始化 Python 环境”重试",
        );
    }
    // 建连失败：DNS/拒连/重置/代理。
    if lower.contains("refused")
        || lower.contains("reset by peer")
        || lower.contains("failed to connect")
        || lower.contains("could not resolve")
        || lower.contains("getaddrinfo")
        || lower.contains("unreachable")
        || lower.contains("proxy")
        || lower.contains("代理")
    {
        return (
            "无法连接下载源",
            "检查系统代理与网络，确认能访问 npmmirror 后重试",
        );
    }
    // 镜像侧错误：缺构建/限流。
    if lower.contains("404")
        || lower.contains("403")
        || lower.contains("429")
        || lower.contains("http error")
    {
        return (
            "下载源返回错误",
            "镜像可能缺该版本构建或被限流：稍后重试，持续失败请反馈",
        );
    }
    // 磁盘空间。
    if lower.contains("no space")
        || lower.contains("disk full")
        || lower.contains("not enough space")
        || lower.contains("enospc")
        || lower.contains("空间不足")
    {
        return ("磁盘空间不足", "清理磁盘后点“初始化 Python 环境”重试");
    }
    // 证书（企业网关 MITM）。
    if lower.contains("certificate") || lower.contains("ssl") || lower.contains("证书") {
        return (
            "证书校验失败",
            "多为公司网关拦截：检查代理设置，或配置 NODE_EXTRA_CA_CERTS 后重试",
        );
    }
    ("安装失败", "点“初始化 Python 环境”重试，仍失败请附日志反馈")
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
    async fn broken_venv_quarantine_and_restore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = missing").unwrap();

        let backup = quarantine_broken_venv(&venv).await.unwrap();
        assert!(!venv.exists());
        assert!(backup.join("pyvenv.cfg").is_file());

        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("partial"), b"new but broken").unwrap();
        restore_quarantined_venv(&venv, Some(&backup)).await;
        assert!(venv.join("pyvenv.cfg").is_file());
        assert!(!venv.join("partial").exists());
        assert!(!backup.exists());
    }

    #[tokio::test]
    async fn test_install_playwright_browser_rejects_unknown_before_io() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
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
        );
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = install_playwright_browser(&mgr, "chromium", &cancel).await;
        assert!(matches!(result, Err(EnvironmentError::Cancelled)));
    }

    #[test]
    fn test_ocr_declared_means_worker_project_present() {
        let dir = tempfile::TempDir::new().unwrap();
        // 先建工程目录再构造 mgr：路径在 new() 内即时解析，事后建目录追不上
        let proj = dir.path().join("python_worker");
        std::fs::create_dir_all(&proj).unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
        );
        assert!(!ocr_declared(&mgr));
        std::fs::write(proj.join("pyproject.toml"), "[project]\n").unwrap();
        assert!(ocr_declared(&mgr));
    }
    /// 失败归类：超时/建连/镜像/磁盘/证书/兜底各走对应原因与建议。
    #[test]
    fn test_classify_playwright_failure_branches() {
        let (cause, _) = classify_playwright_failure("安装超时 (超过 600s)");
        assert_eq!(cause, "下载超时");
        let (cause, hint) = classify_playwright_failure("exit code=1, stderr=Connection refused");
        assert_eq!(cause, "无法连接下载源");
        assert!(hint.contains("npmmirror"));
        let (cause, _) = classify_playwright_failure("HTTP error 404 Not Found");
        assert_eq!(cause, "下载源返回错误");
        let (cause, _) = classify_playwright_failure("ENOSPC: no space left on device");
        assert_eq!(cause, "磁盘空间不足");
        let (cause, _) = classify_playwright_failure("self signed certificate in chain");
        assert_eq!(cause, "证书校验失败");
        let (cause, hint) = classify_playwright_failure("exit code=1, stderr=boom");
        assert_eq!(cause, "安装失败");
        assert!(hint.contains("重试"));
    }
}
