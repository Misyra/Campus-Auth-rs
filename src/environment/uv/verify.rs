//! uv 就绪校验与版本探测：可执行文件实启校验、PATH 回退检查与 SHA256 校验

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::process::uv_command;
use crate::environment::{EnvironmentError, EnvironmentManager, UV_EXE_NAME, UV_MIN_VERSION};

/// PATH 上 uv --version 探测的超时（ENV-10：与 uv_executable_works 的 5s 对齐）
const UV_ON_PATH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    // ENV-10：与 uv_executable_works 同口径加超时兜底——PATH 上的 uv 若挂起
    // （防病毒扫描/损坏的可执行文件）会卡死整个环境检查；kill_on_drop 确保超时
    // 放弃后子进程被终止
    let out = match tokio::time::timeout(
        UV_ON_PATH_CHECK_TIMEOUT,
        tokio::process::Command::new("uv")
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(_)) | Err(_) => return false,
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

/// 校验文件 SHA256 与期望值一致
pub async fn verify_sha256(path: &Path, expected: &str) -> Result<(), EnvironmentError> {
    use crate::utils::io_human::io_error_reason;
    use tokio::io::AsyncReadExt;
    // 人话化载荷：失败类别 + 原始错误（动作由 UvExtractFailed 的 Display 表达）
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| EnvironmentError::UvExtractFailed(io_error_reason(e)))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| EnvironmentError::UvExtractFailed(io_error_reason(e)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusManager;
    use std::sync::Arc;

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
}
