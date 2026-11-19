//! uv sync 与 OCR 依赖交接：venv 对齐、`uv add/remove` 与项目文件备份回滚

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::download::{github_asset_probe_urls, rank_mirror_urls};
use super::process::{CommandOutputError, command_output_with_cancel, uv_command};
use super::verify::uv_exe_path;
use crate::environment::{
    EnvironmentError, EnvironmentManager, GITHUB_MIRRORS, PYTHON_BUILD_STANDALONE_BASE,
    UV_DOWNLOAD_MAX_RETRIES, UV_DOWNLOAD_RETRY_DELAY, UV_SYNC_TIMEOUT,
    mirror::{MirrorChoice, mirror_prefix_of, read_mirror_choice},
};
use crate::utils::io_human::io_error_reason;

/// ddddocr 按需安装版本下限（`uv add` 用，不预声明于 pyproject）
const DDDDOCR_REQUIREMENT: &str = "ddddocr>=1.6.1";
/// ddddocr 包名（`uv remove` 用）
const DDDDOCR_PACKAGE: &str = "ddddocr";

/// uv 托管 Python 下载源阶梯：按用户选源偏好构造 `UV_PYTHON_INSTALL_MIRROR`
/// 取值序列（None = 直连不加环境变量）。
///
/// `uv sync` 首建 venv 时会从 python-build-standalone（GitHub Releases）下载
/// CPython——此前该链路直连无任何镜像加速，国内网络常拖满 600s 同步超时。
/// Auto 并发测速（URL 复用 uv 的发布资产，同 host 同 CDN 路由等价）；
/// Direct = 直连最前、镜像依次兜底；Mirror = 选中镜像最前、其余镜像次之、
/// 直连殿后。Python 压缩包完整性由 uv 自带的 SHA256 校验兜底，镜像只影响
/// 传输路径。
async fn python_mirror_ladder(mgr: &EnvironmentManager) -> Vec<Option<&'static str>> {
    let choice = read_mirror_choice(mgr);
    // Mirror(String) 在解析入口已校验属名单；这里回查静态前缀供阶梯复用——
    // 若文件被手改成名单外前缀，脏值不得注入环境变量，按 Auto 测速兜底
    let chosen = choice
        .mirror_prefix()
        .and_then(|p| GITHUB_MIRRORS.iter().copied().find(|x| *x == p));
    if let Some(prefix) = chosen {
        // 选中镜像最前，其余镜像按名单序次之，直连殿后兜底
        let mut ladder: Vec<Option<&'static str>> = Vec::with_capacity(GITHUB_MIRRORS.len() + 1);
        ladder.push(Some(prefix));
        ladder.extend(
            GITHUB_MIRRORS
                .iter()
                .copied()
                .filter(|m| *m != prefix)
                .map(Some),
        );
        ladder.push(None);
        return ladder;
    }
    if matches!(choice, MirrorChoice::Direct) {
        let mut ladder: Vec<Option<&'static str>> = vec![None];
        ladder.extend(GITHUB_MIRRORS.iter().copied().map(Some));
        return ladder;
    }
    // Auto（以及名单外的脏 Mirror 值兜底）
    let probe_urls = github_asset_probe_urls();
    let order = rank_mirror_urls(mgr, &probe_urls).await;
    let urls: Vec<String> = order
        .into_iter()
        .map(|idx| probe_urls[idx].clone())
        .collect();
    urls.iter()
        .filter_map(|url| mirror_prefix_of(url))
        .map(Some)
        .chain(std::iter::once(None))
        .collect()
}

/// 为 uv 子进程命令附加托管 Python 下载镜像环境变量
///
/// 镜像语义：`UV_PYTHON_INSTALL_MIRROR` 以「镜像前缀 + python-build-standalone
/// Releases 基址」替换 uv 默认的 GitHub 直连基址。None = 直连，不加变量。
fn apply_python_mirror_env(cmd: &mut tokio::process::Command, mirror: Option<&str>) {
    if let Some(mirror) = mirror {
        cmd.env(
            "UV_PYTHON_INSTALL_MIRROR",
            format!("{mirror}{PYTHON_BUILD_STANDALONE_BASE}"),
        );
    }
}

/// 执行 `uv sync` 安装 Python 虚拟环境。
///
/// 本函数只按当前 pyproject/uv.lock 对齐 venv；OCR 用户偏好由独立状态文件
/// 保存，并在 Worker 核心同步后由 [`reconcile_ocr_preference`] 通过 add/remove 对齐。
/// 瞬态失败（网络抖动/锁占用）自动重试：与 uv 二进制下载共用重试常量，
/// 交互路径（设置页安装/修复）无需用户手动再点。
pub async fn run_uv_sync(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }

    let uv_exe = uv_exe_path(mgr);
    // 人话化载荷：失败类别 + 原始错误（动作由 UvExtractFailed 的 Display 表达）
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(|e| EnvironmentError::UvExtractFailed(io_error_reason(e)))?;
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut last_err: Option<EnvironmentError> = None;
    // 托管 Python 下载源阶梯（直连/镜像按实测吞吐排序）：venv 缺失时 uv 会现场
    // 下载 CPython，直连 GitHub 在国内常拖满同步超时；重试时沿阶梯换源
    let mirror_ladder = python_mirror_ladder(mgr).await;
    for attempt in 1..=UV_DOWNLOAD_MAX_RETRIES {
        if attempt > 1 {
            tracing::warn!(
                "uv sync 第 {}/{} 次尝试（{}s 后重试）",
                attempt,
                UV_DOWNLOAD_MAX_RETRIES,
                UV_DOWNLOAD_RETRY_DELAY.as_secs()
            );
            tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
        }
        if cancel.is_cancelled() {
            return Err(EnvironmentError::Cancelled);
        }
        let mut cmd = uv_command(&uv_exe);
        cmd.arg("sync")
            .arg("--project")
            .arg(&*mgr.worker_project_path().to_string_lossy());
        // 每次重试取阶梯的下一档（超出阶梯长度回退直连）
        let mirror = mirror_ladder.get((attempt - 1) as usize).copied().flatten();
        apply_python_mirror_env(&mut cmd, mirror);
        cmd.env("UV_PROJECT_ENVIRONMENT", &venv_path)
            .current_dir(mgr.base_path());

        let output = match command_output_with_cancel(cmd, UV_SYNC_TIMEOUT, cancel).await {
            Ok(output) => output,
            Err(CommandOutputError::Cancelled) => return Err(EnvironmentError::Cancelled),
            Err(CommandOutputError::Timeout) => {
                last_err = Some(EnvironmentError::UvSyncTimeout {
                    timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
                });
                continue;
            }
            Err(CommandOutputError::Io(error)) => {
                last_err = Some(EnvironmentError::UvExtractFailed(io_error_reason(error)));
                continue;
            }
        };

        if output.status.success() {
            return Ok(());
        }
        // 全量 stderr 留给日志排查；进错误消息的 stderr 截尾到 400 字符，
        // 保证末尾的"可能原因"提示不被 last_error 的 600 字符截断吃掉
        let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
        tracing::warn!(
            "uv sync 第 {attempt}/{UV_DOWNLOAD_MAX_RETRIES} 次失败，stderr: {}",
            crate::environment::python::tail_chars(&stderr_full, 4000)
        );
        last_err = Some(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr: crate::environment::python::tail_chars(&stderr_full, 400),
        });
    }
    Err(last_err.expect("uv sync 重试循环结束后必有最后一次错误"))
}

/// 将部署副本中的 ddddocr 声明/安装状态对齐到独立用户偏好。
///
/// 偏好为真时使用 `uv add`，为假时使用 `uv remove`；旧 `ocr.enabled`
/// 只在首次读取时迁移，成功后清理。OCR 失败由上层按补充能力处理，不改变
/// Worker 核心就绪结论。
pub async fn reconcile_ocr_preference(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    let had_legacy_marker = mgr.ocr_marker_path().is_file();
    let enabled = crate::environment::health::ocr_enabled(mgr)?;
    if had_legacy_marker {
        crate::environment::health::set_ocr_enabled(mgr, enabled).await?;
    }

    let declared = crate::environment::health::ddddocr_declared(mgr);
    let installed = crate::environment::python::ddddocr_installed(mgr);
    let changed = match (enabled, declared, installed) {
        (true, false, _) => {
            run_uv_package_alter(mgr, "add", cancel).await?;
            true
        }
        (true, true, false) | (false, false, true) => {
            run_uv_sync(mgr, cancel).await?;
            true
        }
        (false, true, _) => {
            run_uv_package_alter(mgr, "remove", cancel).await?;
            true
        }
        _ => false,
    };

    // 无动作时沿用 ensure_worker_runtime 刚完成的验证结论，避免冷启动重复
    // import 整个 Worker；发生依赖变更时才重新探针并记录新指纹。
    if changed {
        verify_and_record_after_alter(mgr).await?;
    }
    mgr.write_status(|status| {
        status.ocr_enabled = enabled;
        status.ocr_ready = crate::environment::python::ddddocr_installed(mgr);
    });
    if had_legacy_marker {
        crate::environment::health::clear_legacy_ocr_marker(mgr).await;
    }
    Ok(())
}

/// 安装 OCR 依赖：`uv add ddddocr` 写入项目主依赖并自动重锁 + 同步。
pub async fn install_ocr_dep(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    // 偏好先落盘：网络失败时仍能在下次环境修复中继续完成用户意图。
    crate::environment::health::set_ocr_enabled(mgr, true).await?;
    mgr.write_status(|status| status.ocr_enabled = true);
    run_uv_package_alter(mgr, "add", cancel).await?;
    if !crate::environment::python::ddddocr_installed(mgr) {
        return Err(EnvironmentError::UvPackageAlterFailed {
            op: "add",
            exit_code: None,
            stderr: "uv add 成功但 venv 内未探测到 ddddocr".to_string(),
        });
    }
    verify_and_record_after_alter(mgr).await?;
    mgr.write_status(|status| status.ocr_ready = true);
    crate::environment::health::clear_legacy_ocr_marker(mgr).await;
    tracing::info!("OCR 可选依赖安装完成");
    Ok(())
}

/// 卸载 OCR 依赖：`uv remove ddddocr` 摘除按需写入的主依赖并自动重锁 + 同步；
/// 存量副本仍保留 `--optional ocr` 兼容回退。
///
/// 幂等：venv 内无 ddddocr 且无旧标记时直接成功；`uv remove` 因"未声明"报错
/// 但环境已为空时同样按成功计（旧 extra 残留态）。
pub async fn remove_ocr_dep(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    // 与安装同理先保存期望状态；若卸载受阻，下次修复会继续收敛到未启用。
    crate::environment::health::set_ocr_enabled(mgr, false).await?;
    mgr.write_status(|status| status.ocr_enabled = false);
    let declared = crate::environment::health::ddddocr_declared(mgr);
    if !crate::environment::python::ddddocr_installed(mgr) && !declared {
        crate::environment::health::clear_legacy_ocr_marker(mgr).await;
        return Ok(());
    }
    let result = if declared {
        run_uv_package_alter(mgr, "remove", cancel).await
    } else {
        run_uv_sync(mgr, cancel).await
    };
    match result {
        Ok(()) => {}
        Err(e) if !crate::environment::python::ddddocr_installed(mgr) => {
            tracing::debug!("uv remove 未改变已为空的环境: {e}");
        }
        Err(e) => return Err(e),
    }
    verify_and_record_after_alter(mgr).await?;
    mgr.write_status(|status| status.ocr_ready = false);
    crate::environment::health::clear_legacy_ocr_marker(mgr).await;
    Ok(())
}

async fn verify_and_record_after_alter(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let probe = crate::environment::health::probe_worker_runtime(mgr).await;
    if !probe.ready {
        return Err(EnvironmentError::WorkerRuntimeInvalid {
            reason: probe
                .error
                .unwrap_or_else(|| "OCR 依赖变更后 Worker import 验证失败".to_string()),
        });
    }
    crate::environment::health::record_verified_runtime(mgr).await
}

/// 执行 `uv add/remove`（自动重锁 + 同步）：改写前备份 `pyproject.toml`/`uv.lock`，
/// 失败或取消时回滚，成功提交。
async fn run_uv_package_alter(
    mgr: &EnvironmentManager,
    op: &'static str,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    debug_assert!(op == "add" || op == "remove");
    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }

    let uv_exe = uv_exe_path(mgr);
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(|e| EnvironmentError::UvExtractFailed(io_error_reason(e)))?;
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);
    // remove 的参数组：新版 pyproject 无 extra 声明，优先裸 remove；
    // 存量副本仍带 ocr extra 声明时再回退 `--optional ocr`。
    let arg_sets: Vec<Vec<&str>> = if op == "add" {
        vec![vec![DDDDOCR_REQUIREMENT]]
    } else {
        vec![
            vec![DDDDOCR_PACKAGE],
            vec!["--optional", "ocr", DDDDOCR_PACKAGE],
        ]
    };
    let mut last_err: Option<EnvironmentError> = None;
    // add/remove 可能触发托管 Python 下载（venv 缺失等异常路径）：同样走测速选源
    let mirror_ladder = python_mirror_ladder(mgr).await;
    for (i, args) in arg_sets.iter().enumerate() {
        let backup = ProjectFilesBackup::take(mgr.worker_project_path())
            .await
            .map_err(|e| EnvironmentError::UvExtractFailed(io_error_reason(e)))?;

        let mut cmd = uv_package_alter_command(
            &uv_exe,
            op,
            args,
            mgr.worker_project_path(),
            &venv_path,
            mgr.base_path(),
        );
        apply_python_mirror_env(&mut cmd, mirror_ladder.first().copied().flatten());

        let output = match command_output_with_cancel(cmd, UV_SYNC_TIMEOUT, cancel).await {
            Ok(output) => output,
            Err(CommandOutputError::Cancelled) => {
                backup.restore().await;
                return Err(EnvironmentError::Cancelled);
            }
            Err(CommandOutputError::Timeout) => {
                backup.restore().await;
                return Err(EnvironmentError::UvSyncTimeout {
                    timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
                });
            }
            Err(CommandOutputError::Io(error)) => {
                backup.restore().await;
                return Err(EnvironmentError::UvExtractFailed(io_error_reason(error)));
            }
        };

        if output.status.success() {
            backup.discard().await;
            return Ok(());
        }
        backup.restore().await;
        // 同 run_uv_sync：全量 stderr 留给日志，错误消息内截尾保证提示可见
        let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
        tracing::warn!(
            "uv {op} 第 {}/{} 组参数失败 (exit code={:?})，stderr: {}",
            i + 1,
            arg_sets.len(),
            output.status.code(),
            crate::environment::python::tail_chars(&stderr_full, 4000)
        );
        last_err = Some(EnvironmentError::UvPackageAlterFailed {
            op,
            exit_code: output.status.code(),
            stderr: crate::environment::python::tail_chars(&stderr_full, 400),
        });
    }
    Err(last_err.expect("参数组非空，循环结束后必有最后一次错误"))
}

/// 构造 `uv add/remove` 命令；子命令必须位于依赖参数之前。
fn uv_package_alter_command(
    uv_exe: &Path,
    op: &'static str,
    args: &[&str],
    worker_project_path: &Path,
    venv_path: &Path,
    base_path: &Path,
) -> tokio::process::Command {
    let mut cmd = uv_command(uv_exe);
    cmd.arg(op)
        .args(args)
        .arg("--project")
        .arg(&*worker_project_path.to_string_lossy());
    cmd.env("UV_PROJECT_ENVIRONMENT", venv_path)
        .current_dir(base_path);
    cmd
}

/// `pyproject.toml` + `uv.lock` 快照：`uv add/remove` 改写前备份，失败/取消回滚。
struct ProjectFilesBackup {
    entries: Vec<(PathBuf, Option<PathBuf>)>,
}

impl ProjectFilesBackup {
    async fn take(project_dir: &Path) -> std::io::Result<Self> {
        let mut entries = Vec::with_capacity(2);
        for name in ["pyproject.toml", "uv.lock"] {
            let orig = project_dir.join(name);
            if !orig.is_file() {
                entries.push((orig, None));
                continue;
            }
            let bak = {
                let mut name = orig.as_os_str().to_owned();
                name.push(".bak");
                PathBuf::from(name)
            };
            tokio::fs::copy(&orig, &bak).await?;
            entries.push((orig, Some(bak)));
        }
        Ok(Self { entries })
    }

    /// 失败回滚：备份覆盖回原位（best-effort，逐个告警）。
    async fn restore(self) {
        for (orig, backup) in &self.entries {
            if let Some(bak) = backup {
                if let Err(e) = tokio::fs::copy(bak, orig).await {
                    tracing::warn!("回滚 {} 失败: {e}", orig.display());
                    continue;
                }
                let _ = tokio::fs::remove_file(bak).await;
            } else if let Err(e) = tokio::fs::remove_file(orig).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("回滚新建文件 {} 失败: {e}", orig.display());
            }
        }
    }

    /// 成功提交：删除备份。
    async fn discard(self) {
        for (_, backup) in &self.entries {
            if let Some(bak) = backup {
                let _ = tokio::fs::remove_file(bak).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::UV_FALLBACK_VERSION;

    /// 探测 URL 列表：直连在前、镜像按名单顺序、指向 uv 发布资产
    #[test]
    fn test_github_asset_probe_urls() {
        let urls = github_asset_probe_urls();
        assert_eq!(urls.len(), 1 + GITHUB_MIRRORS.len());
        assert!(
            urls[0].starts_with("https://github.com/astral-sh/uv/releases/download"),
            "首项为直连"
        );
        assert!(urls[0].contains(UV_FALLBACK_VERSION));
        for (mirror, url) in GITHUB_MIRRORS.iter().zip(urls.iter().skip(1)) {
            assert!(url.starts_with(*mirror));
            assert!(url.ends_with(&urls[0]), "镜像 URL = 前缀 + 直连 URL");
        }
    }

    /// 镜像前缀判定：镜像 URL 归属对应镜像，直连返回 None
    #[test]
    fn test_mirror_prefix_of() {
        let urls = github_asset_probe_urls();
        assert_eq!(mirror_prefix_of(&urls[0]), None, "首项为直连");
        for (mirror, url) in GITHUB_MIRRORS.iter().zip(urls.iter().skip(1)) {
            assert_eq!(mirror_prefix_of(url), Some(*mirror));
        }
    }

    /// 镜像环境变量：Some 时写入 `UV_PYTHON_INSTALL_MIRROR`（镜像前缀 + 基址），None 不写
    #[test]
    fn test_apply_python_mirror_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = uv_command(&dir.path().join("uv"));
        apply_python_mirror_env(&mut cmd, None);
        assert!(
            !cmd.as_std()
                .get_envs()
                .any(|(k, _)| k == "UV_PYTHON_INSTALL_MIRROR"),
            "直连不得设置镜像变量"
        );
        let mirror = GITHUB_MIRRORS[0];
        apply_python_mirror_env(&mut cmd, Some(mirror));
        let expected = format!("{mirror}{PYTHON_BUILD_STANDALONE_BASE}");
        let got = cmd
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == "UV_PYTHON_INSTALL_MIRROR")
            .map(|(_, v)| {
                v.expect("已设置的环境变量必有值")
                    .to_string_lossy()
                    .into_owned()
            });
        assert_eq!(got, Some(expected));
    }

    /// 项目文件备份：take 快照 → 改写 → restore 还原；discard 仅清备份留原文件。
    #[tokio::test]
    async fn test_project_files_backup_restore_and_discard() {
        let dir = tempfile::tempdir().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        let lock = dir.path().join("uv.lock");
        std::fs::write(&pyproject, b"[project]\nname = \"x\"\n").unwrap();
        std::fs::write(&lock, b"# lock\n").unwrap();

        let backup = ProjectFilesBackup::take(dir.path()).await.unwrap();
        std::fs::write(&pyproject, b"dirty").unwrap();
        std::fs::write(&lock, b"dirty").unwrap();
        backup.restore().await;
        assert_eq!(
            std::fs::read(&pyproject).unwrap(),
            b"[project]\nname = \"x\"\n"
        );
        assert_eq!(std::fs::read(&lock).unwrap(), b"# lock\n");
        assert!(!dir.path().join("pyproject.toml.bak").exists());
        assert!(!dir.path().join("uv.lock.bak").exists());

        let backup = ProjectFilesBackup::take(dir.path()).await.unwrap();
        backup.discard().await;
        assert!(!dir.path().join("pyproject.toml.bak").exists());
        assert_eq!(
            std::fs::read(&pyproject).unwrap(),
            b"[project]\nname = \"x\"\n"
        );
    }

    /// 原先不存在的锁文件若被失败命令新建，restore 必须把它删除。
    #[tokio::test]
    async fn test_project_files_backup_removes_new_file_on_restore() {
        let dir = tempfile::tempdir().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        let lock = dir.path().join("uv.lock");
        std::fs::write(&pyproject, b"[project]\nname = \"x\"\n").unwrap();

        let backup = ProjectFilesBackup::take(dir.path()).await.unwrap();
        std::fs::write(&lock, b"new lock").unwrap();
        backup.restore().await;

        assert!(!lock.exists());
        assert_eq!(
            std::fs::read(&pyproject).unwrap(),
            b"[project]\nname = \"x\"\n"
        );
    }

    /// OCR 依赖变更命令必须显式带 add/remove，防止把包名误当 uv 子命令。
    #[test]
    fn test_uv_package_alter_command_includes_operation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("python_worker");
        let venv = project.join(crate::environment::VENV_DIR);

        let add = uv_package_alter_command(
            Path::new("uv"),
            "add",
            &[DDDDOCR_REQUIREMENT],
            &project,
            &venv,
            dir.path(),
        );
        let add_args: Vec<_> = add
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(add_args[0], "add");
        assert_eq!(add_args[1], DDDDOCR_REQUIREMENT);
        assert_eq!(add_args[2], "--project");

        let remove = uv_package_alter_command(
            Path::new("uv"),
            "remove",
            &[DDDDOCR_PACKAGE],
            &project,
            &venv,
            dir.path(),
        );
        let remove_args: Vec<_> = remove
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(remove_args[0], "remove");
        assert_eq!(remove_args[1], DDDDOCR_PACKAGE);
        assert_eq!(remove_args[2], "--project");
    }
}
