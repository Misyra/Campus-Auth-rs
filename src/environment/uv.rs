//! uv 下载 + 调用封装

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::environment::{
    EnvironmentError, EnvironmentManager, UV_DOWNLOAD_MAX_RETRIES, UV_DOWNLOAD_RETRY_DELAY,
    UV_DOWNLOAD_TIMEOUT, UV_EXE_NAME, UV_MIN_VERSION, UV_RELEASES_BASE, UV_SYNC_TIMEOUT, UV_TARGET,
};

/// 等待环境子进程，同时响应用户取消与阶段超时。
///
/// `kill_on_drop(true)` 保证 select/timeout 放弃 `output()` future 时，已经 spawn 的
/// 子进程不会脱离 Rust 任务继续占用 `.venv`、缓存目录或安装锁。
#[derive(Debug)]
pub(crate) enum CommandOutputError {
    Cancelled,
    Timeout,
    Io(std::io::Error),
}

pub(crate) async fn command_output_with_cancel(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<std::process::Output, CommandOutputError> {
    cmd.kill_on_drop(true);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CommandOutputError::Cancelled),
        result = tokio::time::timeout(timeout, cmd.output()) => match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(CommandOutputError::Io(error)),
            Err(_) => Err(CommandOutputError::Timeout),
        },
    }
}
/// 流式输出上限：stdout/stderr 各自最多保留字节数（Playwright 安装输出可达
/// 数 MB，全缓冲既"假死"又吃内存；超限后仍透出回调，仅截断最终 `Output`）。
const STREAM_CAPTURE_CAP: usize = 256 * 1024;

/// 有上限累积输出：总量超 [`STREAM_CAPTURE_CAP`] 后丢弃新字节（回调不受影响）。
fn push_capped(buf: &mut Vec<u8>, bytes: &[u8]) {
    let room = STREAM_CAPTURE_CAP.saturating_sub(buf.len());
    if room == 0 {
        return;
    }
    buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// 单行切段上限：`\r` 进度行无换行，超限即切段透出，防行缓冲无界增长。
const LINE_SPLIT_CAP: usize = 1024 * 1024;

/// 透出无换行尾行（EOF/读错时）：非空才回调并清空，不重复累积。
fn flush_tail(line: &mut String, on_line: &mut impl FnMut(&str)) {
    if !line.trim_end().is_empty() {
        on_line(line.trim_end());
    }
    line.clear();
}

/// 终止子进程整棵树：Windows 先 `taskkill /T`（uv 拉起的 node/下载孙进程否则残留
/// 并咬住缓存锁），再 `start_kill` 兜底；均为 best-effort。
pub(crate) fn kill_process_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(id) = child.id() {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn();
    }
    let _ = child.start_kill();
}

/// 执行长耗时子进程：双管道流式透出（防全缓冲"假死"与内存膨胀），超时/取消时杀整棵树。
///
/// `on_line` 逐行回调（含 stderr 行，不带行尾换行）；返回的 `Output` 与
/// [`command_output_with_cancel`] 同构（超 [`STREAM_CAPTURE_CAP`] 截断）。
pub(crate) async fn command_output_streaming(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(&str),
) -> Result<std::process::Output, CommandOutputError> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.kill_on_drop(true);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(CommandOutputError::Io)?;

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout 已管道化"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr 已管道化"));
    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    let mut out_line = String::new();
    let mut err_line = String::new();
    let mut out_eof = false;
    let mut err_eof = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    // 双管道必须同时排空，否则子进程写满管道缓冲即阻塞；任一 EOF 后只读另一侧。
    while !(out_eof && err_eof) {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                kill_process_tree(&mut child);
                return Err(CommandOutputError::Cancelled);
            }
            _ = &mut deadline => {
                kill_process_tree(&mut child);
                return Err(CommandOutputError::Timeout);
            }
            r = stdout.read_line(&mut out_line), if !out_eof => {
                match r {
                    Ok(0) => {
                        flush_tail(&mut out_line, &mut on_line);
                        out_eof = true;
                    }
                    Ok(_) => {
                        push_capped(&mut out_buf, out_line.as_bytes());
                        // 正常行（`\n` 结尾）即时回调；`\r` 进度行无换行，超限切段透出。
                        if out_line.ends_with('\n') || out_line.len() > LINE_SPLIT_CAP {
                            on_line(out_line.trim_end());
                            out_line.clear();
                        }
                    }
                    Err(_) => {
                        push_capped(&mut out_buf, out_line.as_bytes());
                        flush_tail(&mut out_line, &mut on_line);
                        out_eof = true;
                    }
                }
            }
            r = stderr.read_line(&mut err_line), if !err_eof => {
                match r {
                    Ok(0) => {
                        flush_tail(&mut err_line, &mut on_line);
                        err_eof = true;
                    }
                    Ok(_) => {
                        push_capped(&mut err_buf, err_line.as_bytes());
                        // 正常行（`\n` 结尾）即时回调；`\r` 进度行无换行，超限切段透出。
                        if err_line.ends_with('\n') || err_line.len() > LINE_SPLIT_CAP {
                            on_line(err_line.trim_end());
                            err_line.clear();
                        }
                    }
                    Err(_) => {
                        push_capped(&mut err_buf, err_line.as_bytes());
                        flush_tail(&mut err_line, &mut on_line);
                        err_eof = true;
                    }
                }
            }
        }
    }

    let status = child.wait().await.map_err(CommandOutputError::Io)?;
    Ok(std::process::Output {
        status,
        stdout: out_buf,
        stderr: err_buf,
    })
}

/// 确定 uv 可执行文件路径：本地 `environment/uv.exe` 存在则用本地路径，否则回退到
/// PATH 中的 `uv`（`Command::new("uv")` 自动走 PATH 解析）。
///
/// 修复 5.4：bootstrap 判定 "PATH 上有 uv 即就绪" 只发生在 `uv_ready`，但阶段 2/3
/// 硬编码本地路径导致 PATH-only 机器 uv sync 必失败。统一走本 helper 后两者一致。
pub fn uv_exe_path(mgr: &EnvironmentManager) -> std::path::PathBuf {
    let local = mgr.env_path().join(UV_EXE_NAME);
    if local.exists() {
        local
    } else {
        std::path::PathBuf::from("uv")
    }
}

/// 解析 `uv --version` 输出中的版本号（形如 "uv 0.5.0 (...)"、Windows 下 "uv 0.5.0"）
fn parse_uv_version<N: AsRef<str>>(output: N) -> Option<semver::Version> {
    let line = output.as_ref().lines().next()?;
    let tok = line.split_whitespace().nth(1)?;
    semver::Version::parse(tok).ok()
}

/// 校验 PATH 上是否可调用 uv 且满足最低版本要求（UV_MIN_VERSION）
///
/// 供 `check_environment` 的 PATH 回退分支使用：PATH 上的 uv 过旧则视为未就绪，
/// 触发引导下载最新版，避免旧版 uv 语法/行为不兼容导致 sync 失败。
pub async fn check_uv_on_path() -> bool {
    let out = match tokio::process::Command::new("uv")
        .arg("--version")
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(ver) = parse_uv_version(&text) else {
        return false;
    };
    let Ok(min) = semver::Version::parse(UV_MIN_VERSION) else {
        return false;
    };
    ver >= min
}

/// 实际启动本地 uv 并检查退出状态（F11）
///
/// 仅凭 `uv.exe` 文件存在会误判半成品为就绪（上次安装 copy 回退失败残留），
/// 参照 `python_executable_works` 模式：执行 `uv --version` 验证确实可启动，
/// Windows 上加 CREATE_NO_WINDOW 避免环境检查弹黑窗。
pub(crate) async fn uv_executable_works(uv_exe: &Path) -> bool {
    if !uv_exe.is_file() {
        return false;
    }
    let mut cmd = uv_command(uv_exe);
    cmd.arg("--version");
    matches!(
        tokio::time::timeout(Duration::from_secs(5), cmd.output()).await,
        Ok(Ok(output)) if output.status.success()
    )
}

/// 从 GitHub Releases 下载 uv 二进制、SHA256 校验、解压到 environment/uv.exe
pub async fn download_uv(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<std::path::PathBuf, EnvironmentError> {
    let env_path = mgr.env_path();
    let uv_dest = env_path.join(UV_EXE_NAME);

    // 获取版本号：优先使用锁定版本，否则查询 GitHub API
    let version = match crate::environment::UV_PINNED_VERSION {
        Some(v) => {
            tracing::debug!(version = %v, "uv 版本决策：使用锁定版本");
            v.to_string()
        }
        None => {
            let v = fetch_latest_uv_version(mgr).await?;
            tracing::debug!(version = %v, "uv 版本决策：未锁定，使用 latest 最新版");
            v
        }
    };

    let mut last_err_msg = String::new();

    for attempt in 0..UV_DOWNLOAD_MAX_RETRIES {
        // 检查取消
        if cancel.is_cancelled() {
            return Err(EnvironmentError::Cancelled);
        }

        // 每次重试重新获取版本号（首次已在循环外获取）
        let ver = if attempt == 0 {
            version.clone()
        } else {
            match crate::environment::UV_PINNED_VERSION {
                Some(v) => v.to_string(),
                None => fetch_latest_uv_version(mgr).await?,
            }
        };
        let sha_urls = uv_sha_urls(&ver);
        let archive_urls = uv_archive_urls(&ver);

        // 1. 下载 SHA256 校验文件（多镜像）
        let expected_hash = match download_text_with_mirrors(mgr, &sha_urls).await {
            Ok(text) => text.split_whitespace().next().unwrap_or("").to_string(),
            Err(e) => {
                tracing::warn!(
                    "下载 uv SHA256 文件失败 (尝试 {}/{}): {}",
                    attempt + 1,
                    UV_DOWNLOAD_MAX_RETRIES,
                    e
                );
                last_err_msg = e.to_string();
                tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
                continue;
            }
        };

        // 2. 流式下载压缩包到临时文件（多镜像 + 带超时）
        let tmp_archive = env_path.join("uv-archive.tmp");
        let mut archive_downloaded = false;
        for archive_url in &archive_urls {
            if cancel.is_cancelled() {
                return Err(EnvironmentError::Cancelled);
            }
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            let dl_result = tokio::time::timeout(
                UV_DOWNLOAD_TIMEOUT,
                download_file_streaming(mgr, archive_url, &tmp_archive),
            )
            .await;
            match dl_result {
                Ok(Ok(())) => {
                    archive_downloaded = true;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::debug!("压缩包下载失败 {}: {}", archive_url, e);
                    last_err_msg = e.to_string();
                }
                Err(_) => {
                    tracing::debug!("压缩包下载超时: {}", archive_url);
                    last_err_msg = format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs());
                }
            }
        }
        if !archive_downloaded {
            tracing::warn!(
                "下载 uv 压缩包全部镜像失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                last_err_msg
            );
            tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
            continue;
        }

        // 3. SHA256 校验
        if let Err(e) = verify_sha256(&tmp_archive, &expected_hash).await {
            tracing::warn!(
                "uv SHA256 校验失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            last_err_msg = e.to_string();
            tokio::time::sleep(UV_DOWNLOAD_RETRY_DELAY).await;
            continue;
        }

        // 4. 解压提取 uv 可执行文件
        let tmp_exe = env_path.join("uv.tmp");
        if let Err(e) = extract_uv_from_archive(&tmp_archive, &tmp_exe) {
            tracing::warn!(
                "uv 解压失败 (尝试 {}/{}): {}",
                attempt + 1,
                UV_DOWNLOAD_MAX_RETRIES,
                e
            );
            let _ = tokio::fs::remove_file(&tmp_archive).await;
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 5. 原子安装：rename 到目标位置（跨卷回退走 copy→临时名→rename）
        let _ = tokio::fs::remove_file(&tmp_archive).await;

        // F11/A6：统一走 utils::io::rename_or_copy——rename 失败（跨卷）时
        // copy 到目标同目录临时名再原子 rename，目标位置永远不会出现半成品；
        // copy 失败自动清理临时文件，残留的 tmp_exe 一并清除。
        if let Err(e) = crate::utils::io::rename_or_copy(&tmp_exe, &uv_dest).await {
            let _ = tokio::fs::remove_file(&tmp_exe).await;
            return Err(EnvironmentError::UvExtractFailed(e));
        }

        // 6. 验证可执行
        let output = uv_command(&uv_dest)
            .arg("--version")
            .output()
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;

        if !output.status.success() {
            return Err(EnvironmentError::UvExtractFailed(std::io::Error::other(
                "uv --version 执行失败",
            )));
        }

        tracing::info!("uv 下载安装成功: {}", uv_dest.display());
        return Ok(uv_dest);
    }

    // 所有重试均失败
    Err(EnvironmentError::UvDownloadIoFailed {
        retries: UV_DOWNLOAD_MAX_RETRIES,
        message: last_err_msg,
    })
}

/// 通过 GitHub API 获取 uv 最新版本号（多镜像）
async fn fetch_latest_uv_version(mgr: &EnvironmentManager) -> Result<String, EnvironmentError> {
    let urls = github_api_urls();
    let mut last_err = String::new();

    for url in &urls {
        let resp = match tokio::time::timeout(
            UV_DOWNLOAD_TIMEOUT,
            mgr.http_client()
                .get(url)
                .header("User-Agent", "campus-auth")
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::debug!("GitHub API 镜像失败 {}: {}", url, e);
                last_err = e.to_string();
                continue;
            }
            Err(_) => {
                tracing::debug!(
                    "GitHub API 镜像超时 {}: {}",
                    url,
                    UV_DOWNLOAD_TIMEOUT.as_secs()
                );
                last_err = format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs());
                continue;
            }
        };

        if !resp.status().is_success() {
            tracing::debug!("GitHub API 镜像失败 {}: HTTP {}", url, resp.status());
            last_err = format!("HTTP {}", resp.status());
            continue;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!("GitHub API 镜像解析失败 {}: {}", url, e);
                last_err = e.to_string();
                continue;
            }
        };

        if let Some(tag) = json["tag_name"].as_str() {
            let version = tag.strip_prefix('v').unwrap_or(tag);
            return Ok(version.to_string());
        }
        last_err = "tag_name 字段缺失".to_string();
    }

    Err(EnvironmentError::GitHubApiError(format!(
        "所有 GitHub API 镜像均失败: {last_err}"
    )))
}

/// 下载文本内容（用于获取 SHA256 校验文件）
async fn download_text(mgr: &EnvironmentManager, url: &str) -> Result<String, EnvironmentError> {
    let resp = tokio::time::timeout(
        UV_DOWNLOAD_TIMEOUT,
        mgr.http_client()
            .get(url)
            .header("User-Agent", "campus-auth")
            .send(),
    )
    .await
    .map_err(|_| EnvironmentError::UvDownloadIoFailed {
        retries: 0,
        message: format!("下载超时 (超过 {}s)", UV_DOWNLOAD_TIMEOUT.as_secs()),
    })?
    .map_err(|e| EnvironmentError::UvDownloadFailed {
        retries: 0,
        source: e,
    })?;

    resp.text()
        .await
        .map_err(|e| EnvironmentError::UvDownloadFailed {
            retries: 0,
            source: e,
        })
}

/// 流式下载文件到指定路径（不含超时控制，由调用方包裹）
async fn download_file_streaming(
    mgr: &EnvironmentManager,
    url: &str,
    dest: &Path,
) -> Result<(), EnvironmentError> {
    crate::utils::io::download_streaming(mgr.http_client(), url, dest, 256 * 1024 * 1024)
        .await
        .map_err(|e| match e {
            crate::utils::io::DownloadError::Http(e) => EnvironmentError::UvDownloadFailed {
                retries: 0,
                source: e,
            },
            crate::utils::io::DownloadError::Io(e) => EnvironmentError::UvExtractFailed(e),
            crate::utils::io::DownloadError::TooLarge { limit } => {
                EnvironmentError::UvDownloadIoFailed {
                    retries: 0,
                    message: format!("下载内容超过大小上限 {limit} 字节"),
                }
            }
        })
}

/// 校验文件 SHA256 与期望值一致
pub async fn verify_sha256(path: &Path, expected: &str) -> Result<(), EnvironmentError> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let got = hex::encode(digest);
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(EnvironmentError::UvChecksumMismatch {
            expected: expected.to_string(),
            got,
        })
    }
}

/// 从压缩包（zip / tar.gz，按扩展名分派）中提取 uv 可执行文件到目标路径
fn extract_uv_from_archive(archive_path: &Path, dest: &Path) -> std::io::Result<()> {
    // uv 二进制位于压缩包内 `uv-{target}/uv.exe`（或 `uv`）。先按文件名过滤解压到
    // 临时目录，再把找到的可执行文件复制到目标路径（复用 extract_archive 模板）。
    let tmp_dir = tempfile::tempdir()?;
    let mut found: Option<PathBuf> = None;
    crate::utils::io::extract_archive(archive_path, tmp_dir.path(), |name| {
        // 同时接受 uv / uv.exe：官方 unix 资产为 uv，Windows zip 为 uv.exe；
        // 宽松匹配让解压逻辑与当前平台的 UV_EXE_NAME 解耦
        if name
            .file_name()
            .is_some_and(|f| f == UV_EXE_NAME || f == "uv" || f == "uv.exe")
        {
            found = Some(tmp_dir.path().join(name));
            true
        } else {
            false
        }
    })?;
    let src = found.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "压缩包中未找到 uv 可执行文件")
    })?;
    std::fs::copy(&src, dest)?;
    // unix：显式补 0755 兜底——uv 不可执行 = 环境引导整体失败，不依赖
    // 解压/复制链路上任何一环的权限位传递是否完整
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// ddddocr 依赖声明（`uv add` 用，版本下限与旧 `ocr` extra 一致）
const DDDDOCR_REQUIREMENT: &str = "ddddocr>=1.6.1";
/// ddddocr 包名（`uv remove` 用）
const DDDDOCR_PACKAGE: &str = "ddddocr";

/// 执行 `uv sync` 安装 Python 虚拟环境。
///
/// 用户偏好已收敛进 `pyproject.toml` 主依赖（见 `install_ocr_dep` 的 `uv add`），
/// 同步天然保留用户选择，无需 `--extra` 开关。
pub async fn run_uv_sync(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    // 存量迁移：旧版 `ocr.enabled` 标记 → 主依赖认领（一次性，失败保留标记下次重试）
    migrate_legacy_ocr_marker(mgr, cancel).await?;
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
        .map_err(EnvironmentError::UvExtractFailed)?;
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut cmd = uv_command(&uv_exe);
    cmd.arg("sync")
        .arg("--project")
        .arg(&*mgr.worker_project_path().to_string_lossy());
    cmd.env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path());

    let output = match command_output_with_cancel(cmd, UV_SYNC_TIMEOUT, cancel).await {
        Ok(output) => output,
        Err(CommandOutputError::Cancelled) => return Err(EnvironmentError::Cancelled),
        Err(CommandOutputError::Timeout) => {
            return Err(EnvironmentError::UvSyncTimeout {
                timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
            });
        }
        Err(CommandOutputError::Io(error)) => {
            return Err(EnvironmentError::UvExtractFailed(error));
        }
    };

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr,
        })
    }
}

/// 存量迁移：旧版 `environment/ocr.enabled` 标记 → `uv add` 认领为项目主依赖。
///
/// 旧版经 `uv sync --extra ocr` 安装 ddddocr；新版裸 `sync` 会按声明对齐环境而卸载它。
/// 标记存在时跑一次 `uv add`（已安装即增量认领），成功后删除标记。
async fn migrate_legacy_ocr_marker(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    if !mgr.ocr_marker_path().is_file() {
        return Ok(());
    }
    tracing::info!("检测到旧版 OCR 启用标记，迁移为项目主依赖...");
    run_uv_package_alter(mgr, "add", cancel).await?;
    if let Err(e) = tokio::fs::remove_file(mgr.ocr_marker_path()).await {
        tracing::warn!("旧版 OCR 标记清理失败（下次同步会再次认领）: {e}");
    }
    Ok(())
}

/// 安装 OCR 依赖：`uv add ddddocr` 写入项目主依赖并自动重锁 + 同步。
pub async fn install_ocr_dep(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    run_uv_package_alter(mgr, "add", cancel).await?;
    if !crate::environment::python::ddddocr_installed(mgr) {
        return Err(EnvironmentError::UvPackageAlterFailed {
            op: "add",
            exit_code: None,
            stderr: "uv add 成功但 venv 内未探测到 ddddocr".to_string(),
        });
    }
    tracing::info!("OCR 可选依赖安装完成");
    Ok(())
}

/// 卸载 OCR 依赖：`uv remove ddddocr` 移出项目主依赖并自动重锁 + 同步。
///
/// 幂等：venv 内无 ddddocr 且无旧标记时直接成功；`uv remove` 因"未声明"报错
/// 但环境已为空时同样按成功计（旧 extra 残留态）。
pub async fn remove_ocr_dep(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
) -> Result<(), EnvironmentError> {
    if !crate::environment::python::ddddocr_installed(mgr) && !mgr.ocr_marker_path().is_file() {
        return Ok(());
    }
    match run_uv_package_alter(mgr, "remove", cancel).await {
        Ok(()) => {}
        Err(e) if !crate::environment::python::ddddocr_installed(mgr) => {
            tracing::debug!("uv remove 未改变已为空的环境: {e}");
        }
        Err(e) => return Err(e),
    }
    let _ = tokio::fs::remove_file(mgr.ocr_marker_path()).await;
    Ok(())
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
        .map_err(EnvironmentError::UvExtractFailed)?;
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);
    let backup = ProjectFilesBackup::take(mgr.worker_project_path())
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    let mut cmd = uv_command(&uv_exe);
    cmd.arg(op);
    if op == "add" {
        cmd.arg(DDDDOCR_REQUIREMENT);
    } else {
        cmd.arg(DDDDOCR_PACKAGE);
    }
    cmd.arg("--project")
        .arg(&*mgr.worker_project_path().to_string_lossy());
    cmd.env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path());

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
            return Err(EnvironmentError::UvExtractFailed(error));
        }
    };

    if output.status.success() {
        backup.discard().await;
        Ok(())
    } else {
        backup.restore().await;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(EnvironmentError::UvPackageAlterFailed {
            op,
            exit_code: output.status.code(),
            stderr,
        })
    }
}

/// `pyproject.toml` + `uv.lock` 快照：`uv add/remove` 改写前备份，失败/取消回滚。
struct ProjectFilesBackup {
    pairs: Vec<(PathBuf, PathBuf)>,
}

impl ProjectFilesBackup {
    async fn take(project_dir: &Path) -> std::io::Result<Self> {
        let mut pairs = Vec::with_capacity(2);
        for name in ["pyproject.toml", "uv.lock"] {
            let orig = project_dir.join(name);
            if !orig.is_file() {
                continue;
            }
            let bak = {
                let mut name = orig.as_os_str().to_owned();
                name.push(".bak");
                PathBuf::from(name)
            };
            tokio::fs::copy(&orig, &bak).await?;
            pairs.push((orig, bak));
        }
        Ok(Self { pairs })
    }

    /// 失败回滚：备份覆盖回原位（best-effort，逐个告警）。
    async fn restore(self) {
        for (orig, bak) in &self.pairs {
            if let Err(e) = tokio::fs::copy(bak, orig).await {
                tracing::warn!("回滚 {} 失败: {e}", orig.display());
                continue;
            }
            let _ = tokio::fs::remove_file(bak).await;
        }
    }

    /// 成功提交：删除备份。
    async fn discard(self) {
        for (_, bak) in &self.pairs {
            let _ = tokio::fs::remove_file(bak).await;
        }
    }
}

/// uv 发布资产扩展名：Windows 为 zip；官方 Linux / macOS release 只提供 tar.gz
/// （zip 资产在 unix 目标上 404，是环境引导链的第一处平台断点）
#[cfg(target_os = "windows")]
pub(crate) const UV_ASSET_EXT: &str = "zip";
#[cfg(not(target_os = "windows"))]
pub(crate) const UV_ASSET_EXT: &str = "tar.gz";

/// 构造 uv 下载 URL — 主站
pub(crate) fn uv_archive_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.{UV_ASSET_EXT}")
}

/// 构造 uv SHA256 文件 URL — 主站
pub(crate) fn uv_sha_url(version: &str) -> String {
    format!("{UV_RELEASES_BASE}/{version}/uv-{UV_TARGET}.{UV_ASSET_EXT}.sha256")
}

/// 生成所有镜像的下载 URL 列表（主站 + 代理镜像）
fn uv_archive_urls(version: &str) -> Vec<String> {
    let base = uv_archive_url(version);
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_MIRRORS.len());
    // 先尝试直连
    urls.push(base.clone());
    // 再尝试代理镜像
    for mirror in crate::environment::GITHUB_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 生成所有镜像的 SHA256 URL 列表
fn uv_sha_urls(version: &str) -> Vec<String> {
    let base = uv_sha_url(version);
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_MIRRORS.len());
    urls.push(base.clone());
    for mirror in crate::environment::GITHUB_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 生成所有镜像的 GitHub API URL 列表
fn github_api_urls() -> Vec<String> {
    let base = "https://api.github.com/repos/astral-sh/uv/releases/latest";
    let mut urls = Vec::with_capacity(1 + crate::environment::GITHUB_API_MIRRORS.len());
    urls.push(base.to_string());
    for mirror in crate::environment::GITHUB_API_MIRRORS {
        urls.push(format!("{mirror}{base}"));
    }
    urls
}

/// 尝试从多个镜像下载文本，第一个成功即返回
///
/// 镜像逐个尝试属常规路径（部分镜像不可达是常态），逐镜像日志降为 debug，
/// 仅最终成功（调用方 `uv 下载安装成功`）与整体失败保留可见级别。
async fn download_text_with_mirrors(
    mgr: &EnvironmentManager,
    urls: &[String],
) -> Result<String, EnvironmentError> {
    let mut last_err = String::new();
    tracing::debug!("尝试 {} 个镜像下载", urls.len());
    for (i, url) in urls.iter().enumerate() {
        tracing::debug!("镜像 {}/{}: {}", i + 1, urls.len(), url);
        match download_text(mgr, url).await {
            Ok(text) => {
                tracing::debug!("镜像 {} 下载成功", url);
                return Ok(text);
            }
            Err(e) => {
                tracing::debug!("镜像 {} 失败: {}", url, e);
                last_err = e.to_string();
            }
        }
    }
    Err(EnvironmentError::GitHubApiError(format!(
        "所有镜像均失败: {last_err}"
    )))
}

/// 构造 uv 子进程 Command（Windows 上隐藏控制台窗口，避免环境引导弹黑窗）
pub(crate) fn uv_command(uv_exe: &std::path::Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(uv_exe);
    // Tokio 默认 drop Child 不会结束子进程；环境安装命令一律在 future 被取消/超时
    // 时终止，避免后台残留 uv/playwright 继续修改同一环境。
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusManager;
    use std::sync::Arc;

    #[test]
    fn cancellable_command_child_process() {
        let Ok(marker) = std::env::var("CAMPUS_AUTH_CANCEL_CHILD_MARKER") else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_secs(3));
        std::fs::write(marker, b"finished").unwrap();
    }

    #[tokio::test]
    async fn test_command_output_with_cancel_kills_inflight_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-finished.marker");
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("cancellable_command_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_CANCEL_CHILD_MARKER", &marker);

        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel_later.cancel();
        });

        let started = std::time::Instant::now();
        let result =
            command_output_with_cancel(cmd, std::time::Duration::from_secs(10), &cancel).await;
        assert!(matches!(result, Err(CommandOutputError::Cancelled)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "取消应快速结束等待"
        );

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!marker.exists(), "被取消的子进程不得继续运行到写入完成标记");
    }

    #[test]
    fn streaming_lines_child_process() {
        if let Ok(secs) = std::env::var("CAMPUS_AUTH_STREAM_SLEEP_SECS") {
            if let Ok(secs) = secs.parse::<u64>() {
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
        }
        let Ok(count) = std::env::var("CAMPUS_AUTH_STREAM_LINES") else {
            return;
        };
        let Ok(count) = count.parse::<usize>() else {
            return;
        };
        for i in 0..count {
            println!("stream-line-{i}");
        }
        eprintln!("stream-err-done");
        // 注意：此处故意不写无换行尾行——子进程与 libtest harness 共享
        // stdout 时，尾字节曾以约 5% 概率丢失（stdout/stderr、flush、sleep、
        // exit 组合均复现过，属 harness 共享输出的脆弱性）。无换行尾行改由
        // 下面的 flush_tail 纯函数单测确定性覆盖；本用例只断言管道行回调与 Output 组装。
    }

    /// 流式执行：逐行回调（stdout + stderr）与最终 Output 均完整。
    #[tokio::test]
    async fn test_command_output_streaming_captures_lines() {
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("streaming_lines_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_STREAM_LINES", "50");

        let cancel = CancellationToken::new();
        let mut lines: Vec<String> = Vec::new();
        let result =
            command_output_streaming(cmd, std::time::Duration::from_secs(30), &cancel, |line| {
                lines.push(line.to_string())
            })
            .await;
        let output = result.expect("流式执行应成功");
        assert!(output.status.success());
        // 子进程即测试二进制自身，harness 自身输出也会进管道，只断言超集。
        for i in 0..50 {
            assert!(
                lines.iter().any(|l| l == &format!("stream-line-{i}")),
                "缺 stdout 行 stream-line-{i}"
            );
        }
        assert!(lines.iter().any(|l| l == "stream-err-done"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("stream-line-49"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("stream-err-done"));
    }

    /// 无换行尾行透出（`flush_tail`，`command_output_streaming` 的 EOF 兜底）：
    /// 子进程侧的尾行覆盖曾因 harness 共享输出 flake，现改由纯函数单测锁定。
    #[test]
    fn test_flush_tail_emits_nonempty_remainder() {
        let mut got = Vec::new();
        let mut pending = String::from("stream-tail-nonewline");
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got, vec!["stream-tail-nonewline"]);
        assert!(pending.is_empty(), "透出后必须清空，避免 EOF 重复累积");
        // 二次调用不再重复回调
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn test_flush_tail_skips_blank_remainder() {
        let mut got = Vec::new();
        for blank in ["", "   ", "\n", "  \n  "] {
            let mut pending = blank.to_string();
            flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
            assert!(pending.is_empty());
        }
        assert!(got.is_empty(), "空白尾行不得回调");
    }

    #[test]
    fn test_flush_tail_trims_trailing_whitespace() {
        let mut got = Vec::new();
        let mut pending = String::from("tail-with-cr\r");
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got, vec!["tail-with-cr"]);
    }

    /// 流式执行超时：快速返回 Timeout（超时路径同样走进程树 kill）。
    #[tokio::test]
    async fn test_command_output_streaming_timeout() {
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("streaming_lines_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_STREAM_SLEEP_SECS", "30")
            .env("CAMPUS_AUTH_STREAM_LINES", "1");

        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let result =
            command_output_streaming(cmd, std::time::Duration::from_secs(2), &cancel, |_| {}).await;
        assert!(matches!(result, Err(CommandOutputError::Timeout)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "超时应快速结束等待"
        );
    }

    /// URL 构造：压缩包与 sha256 均指向主站对应文件（资产扩展名按平台：
    /// Windows zip / unix tar.gz）
    #[test]
    fn test_uv_urls_format() {
        let expected = format!("/0.5.0/uv-{UV_TARGET}.{UV_ASSET_EXT}");
        let archive = uv_archive_url("0.5.0");
        assert!(archive.ends_with(&expected), "archive: {archive}");
        let sha = uv_sha_url("0.5.0");
        assert!(
            sha.ends_with(&format!(".{UV_ASSET_EXT}.sha256")),
            "sha: {sha}"
        );
    }

    /// 镜像列表：直连在前，代理镜像在后，首项为直连
    #[test]
    fn test_uv_mirror_urls() {
        let archives = uv_archive_urls("0.5.0");
        assert_eq!(archives[0], uv_archive_url("0.5.0"));
        assert!(archives.len() > 1, "应包含代理镜像");
        let shas = uv_sha_urls("0.5.0");
        assert_eq!(shas[0], uv_sha_url("0.5.0"));
        assert_eq!(shas.len(), archives.len());
    }

    /// SHA256 校验：正确值通过，错误值被拒
    #[tokio::test]
    async fn test_verify_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, b"hello campus-auth").unwrap();
        let expected = hex::encode(Sha256::digest(b"hello campus-auth"));
        assert!(verify_sha256(&path, &expected).await.is_ok());
        assert!(verify_sha256(&path, "0000deadbeef").await.is_err());
    }

    /// zip 提取：从含 uv.exe 的 zip 中正确提取（Windows 资产格式）
    #[test]
    fn test_extract_uv_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("uv.zip");
        // 构造一个含 uv-{target}/uv.exe 的 zip
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("uv-0.5.0/uv.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write;
        zip.write_all(b"MZ fake-exe").unwrap();
        let cursor = zip.finish().unwrap();
        std::fs::write(&zip_path, cursor.into_inner()).unwrap();

        let dest = dir.path().join("uv.exe");
        extract_uv_from_archive(&zip_path, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ fake-exe");
    }

    /// tar.gz 提取：unix 官方资产格式（uv-{target}/uv）跨平台可解，且
    /// 解出的可执行文件在 unix 上具备 0755 权限
    #[test]
    fn test_extract_uv_from_tar_gz() {
        use flate2::write::GzEncoder;
        use tar::{Builder, Header};

        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("uv.tar.gz");
        let gz = GzEncoder::new(
            std::fs::File::create(&tar_path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = Builder::new(gz);
        let uv_content: &[u8] = b"ELF fake-uv";
        let mut hdr = Header::new_gnu();
        // tar header 的 size 必须与数据长度一致（tar 按大小寻址）
        hdr.set_size(uv_content.len() as u64);
        hdr.set_mode(0o755);
        hdr.set_cksum();
        builder
            .append_data(&mut hdr, "uv-x86_64-unknown-linux-gnu/uv", uv_content)
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let dest = dir.path().join("uv");
        extract_uv_from_archive(&tar_path, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ELF fake-uv");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "解压出的 uv 必须可执行");
        }
    }

    /// 5.4：uv_exe_path 两分支——本地存在返回本地路径，否则回退到 PATH 的 `uv`
    #[test]
    fn test_uv_exe_path_two_branches() {
        let dir = tempfile::tempdir().unwrap();
        let status = Arc::new(StatusManager::new());
        let mgr = EnvironmentManager::new(dir.path().to_path_buf(), status);

        // 本地不存在 → 回退 PATH
        assert_eq!(uv_exe_path(&mgr), std::path::PathBuf::from("uv"));

        // 本地存在 → 返回本地路径
        let env = dir.path().join(crate::environment::ENV_DIR);
        std::fs::create_dir_all(&env).unwrap();
        std::fs::write(env.join(UV_EXE_NAME), b"fake").unwrap();
        assert_eq!(uv_exe_path(&mgr), env.join(UV_EXE_NAME));
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

    /// 5.4：uv --version 输出解析（含 Windows 可能的括号后缀）
    #[test]
    fn test_parse_uv_version() {
        assert_eq!(
            parse_uv_version("uv 0.5.0 (9b1dd64fb 2024-11-26)"),
            Some(semver::Version::parse("0.5.0").unwrap())
        );
        assert_eq!(
            parse_uv_version("uv 0.6.1"),
            Some(semver::Version::parse("0.6.1").unwrap())
        );
        assert!(parse_uv_version("uv: unrecognized option").is_none());
    }

    /// F11：本地 uv 就绪判定加 --version 实启校验——
    /// 不存在的文件与不可执行的半成品内容都不得判为就绪
    #[tokio::test]
    async fn test_uv_executable_works_rejects_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        // 不存在的文件：文件级快速拒绝
        assert!(
            !uv_executable_works(&dir.path().join("missing-uv.exe")).await,
            "不存在的文件应判不可用"
        );
        // 半成品内容：文件存在但不是可执行映像，实际启动必然失败
        // （Windows 上 CreateProcess 拒绝非 PE 文件；Unix 上 execve 报 Exec 格式错误）
        let broken = dir.path().join("uv.exe");
        std::fs::write(&broken, b"half-written garbage").unwrap();
        assert!(
            !uv_executable_works(&broken).await,
            "半成品文件不得因 exists() 被判就绪"
        );
    }
}
