//! 更新 helper 独立入口：等待主进程退出 -> 替换 exe -> 启动新 exe -> 清理
//!
//! 由主进程的 `UpdaterService::spawn_helper()` spawn，接收 `--pid` 参数。
//! 从 `<base_path>/update/pending.json` 读取 staging / target 信息，
//! 等待主进程退出后完成替换并重启新版本。

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use campus_auth::utils::lock::is_process_alive;
use clap::Parser;

/// campus-auth 更新助手进程
#[derive(Parser)]
#[command(name = "campus-auth-helper", version, about = "Campus-Auth 更新助手")]
struct HelperCli {
    /// 应用待处理更新（从 pending.json 读取配置）
    #[arg(long)]
    apply_update: bool,

    /// 主进程 PID（等待其退出后执行替换）
    #[arg(long)]
    pid: u32,

    /// staging 目录路径（可选，默认从 pending.json 读取）
    #[arg(long)]
    staging: Option<PathBuf>,

    /// 目标 exe 路径（可选，默认从 pending.json 读取）
    #[arg(long)]
    target: Option<PathBuf>,

    /// 基础路径（可选，默认从 exe 所在目录推断）
    #[arg(long)]
    base_path: Option<PathBuf>,
}

/// pending.json 数据结构（与 UpdaterService 的 PendingUpdate 对应）
#[derive(serde::Deserialize)]
struct PendingInfo {
    staging_dir: String,
    target_exe: String,
    original_args: Vec<String>,
    /// 暂存包预期 SHA256（G13：替换前复核；空 = 发布源未提供，降级跳过）
    #[serde(default)]
    sha256: String,
    #[allow(dead_code)]
    version: String,
}

fn main() {
    let cli = HelperCli::parse();

    if !cli.apply_update {
        eprintln!("campus-auth-helper v{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // 1. 等待主进程退出
    println!("[helper] 等待主进程 (PID {}) 退出...", cli.pid);
    if !wait_for_process_exit(cli.pid) {
        // 主进程未退出：中止更新，保留 staging 与 pending.json，待主进程下次启动
        // 时由 apply_pending_on_startup 应用（不执行 cleanup，避免摧毁待应用更新）
        std::process::exit(1);
    }
    // 额外等待一小段时间，确保文件句柄完全释放
    sleep(Duration::from_millis(500));

    // 2. 从 pending.json 读取配置（CLI 参数优先）
    let base_path = cli.base_path.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    });

    let pending_path = base_path.join("update").join("pending.json");
    let pending: Option<PendingInfo> = std::fs::read_to_string(&pending_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    let staging_dir = cli
        .staging
        .or_else(|| pending.as_ref().map(|p| PathBuf::from(&p.staging_dir)))
        .expect("缺少 staging 目录路径（需 --staging 或 pending.json）");

    let target_exe = cli
        .target
        .or_else(|| pending.as_ref().map(|p| PathBuf::from(&p.target_exe)))
        .expect("缺少目标 exe 路径（需 --target 或 pending.json）");

    // G13：staging / target 路径必须位于 base_path 之内（canonicalize 后
    // starts_with 检查）。二者取值可能来自 pending.json——文件被篡改时会把
    // 任意系统路径变成替换目标（任意位置覆写）或清理对象，必须先拒绝。
    if !is_within_base(&staging_dir, &base_path) {
        eprintln!(
            "[helper] 拒绝执行：staging 路径不在 base_path 之内: {}",
            staging_dir.display()
        );
        std::process::exit(1);
    }
    if !is_within_base(&target_exe, &base_path) {
        eprintln!(
            "[helper] 拒绝执行：target 路径不在 base_path 之内: {}",
            target_exe.display()
        );
        std::process::exit(1);
    }

    let extracted_exe = staging_dir.join("extracted").join(exe_name());

    // 3. 校验 staging 文件存在
    if !extracted_exe.exists() {
        eprintln!("[helper] staging 文件不存在: {}", extracted_exe.display());
        cleanup(&base_path, &staging_dir);
        std::process::exit(1);
    }

    // 3.5 G13：复制前复核 staging exe 的 SHA256（期望值从 pending.json 传入）
    // 校验失败（staging 损坏/被篡改）时中止替换并清理不可信的 staging
    let expected_sha = pending
        .as_ref()
        .map(|p| p.sha256.as_str())
        .unwrap_or_default();
    if !verify_staging_sha256(&extracted_exe, expected_sha) {
        eprintln!("[helper] staging exe SHA256 复核失败，中止替换");
        cleanup(&base_path, &staging_dir);
        std::process::exit(1);
    }

    // 4. 备份旧 exe
    let backup_path = target_exe.with_extension("exe.bak");
    if target_exe.exists() {
        println!("[helper] 备份旧版本 -> {}", backup_path.display());
        if let Err(e) = std::fs::copy(&target_exe, &backup_path) {
            eprintln!("[helper] 备份失败: {e}");
            // 备份失败不阻断替换流程
        }
    }

    // 5. 替换 exe（helper 复制新文件覆盖旧 exe，而非替换自身）
    println!(
        "[helper] 替换 {} -> {}",
        extracted_exe.display(),
        target_exe.display()
    );
    if let Err(e) = std::fs::copy(&extracted_exe, &target_exe) {
        eprintln!("[helper] 替换失败: {e}");
        // 尝试回退：从备份恢复
        if backup_path.exists() {
            let _ = std::fs::copy(&backup_path, &target_exe);
            eprintln!("[helper] 已回退到备份版本");
        }
        cleanup(&base_path, &staging_dir);
        std::process::exit(1);
    }

    // 6. 启动新 exe（传递原始启动参数）
    let original_args = pending
        .as_ref()
        .map(|p| p.original_args.clone())
        .unwrap_or_default();
    println!("[helper] 启动新版本...");
    match std::process::Command::new(&target_exe)
        .args(&original_args)
        .spawn()
    {
        Ok(mut child) => {
            println!("[helper] 新版本已启动");
            // G13：延迟删 .bak——spawn 成功不代表新 exe 能正常运行（依赖缺失、
            // 版本不兼容时会秒退），立即删备份会让用户失去回退手段。
            // 权衡：多等 5 秒 + try_wait 两次探活，仅在确认新进程持续存活后才
            // 删除 .bak；否则保留备份并输出回退提示。
            let first_probe = probe_alive(&mut child);
            let second_probe = if matches!(first_probe, Ok(None)) {
                sleep(Duration::from_secs(5));
                probe_alive(&mut child)
            } else {
                // 首查已退出/探测失败：无需二查
                first_probe
            };
            if decide_backup_deletion(first_probe, second_probe) {
                let _ = std::fs::remove_file(&backup_path);
                println!("[helper] 新版本持续运行，已清理备份");
            } else {
                eprintln!(
                    "[helper] 新版本启动后疑似异常退出，保留备份 {} 供手动回退",
                    backup_path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("[helper] 启动新版本失败: {e}");
            eprintln!(
                "[helper] 保留备份 {} 供手动回退",
                backup_path.display()
            );
        }
    }

    // 7. 清理
    cleanup(&base_path, &staging_dir);

    println!("[helper] 更新完成");
}

/// 轮询等待指定 PID 的进程退出（最多等待 60 秒）
///
/// 返回 `true` 表示主进程已退出；超时返回 `false`。5.3：超时后**不再强制继续**——
/// 主进程仍存活时覆盖运行中 exe 的替换必然失败，且强制继续会走 cleanup 摧毁 staging
/// 与 pending.json，导致更新彻底丢失。改为报错退出并保留 staging/pending，把应用机会
/// 留给主进程下次启动的 `apply_pending_on_startup`。
fn wait_for_process_exit(pid: u32) -> bool {
    for _ in 0..600 {
        if !is_process_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(100));
    }
    eprintln!("[helper] 等待进程退出超时（60 秒），中止更新");
    false
}

/// 清理 pending.json 标记与 staging 目录（staging 目录用 CLI --staging 传入的实际路径）
///
/// G13：staging 取值可能来自被篡改的 pending.json，remove_dir_all 前复核其
/// 确实位于 base_path 之内，避免 cleanup 变成任意目录删除。
fn cleanup(base_path: &Path, staging_dir: &Path) {
    let pending_path = base_path.join("update").join("pending.json");
    let _ = std::fs::remove_file(&pending_path);
    if is_within_base(staging_dir, base_path) {
        let _ = std::fs::remove_dir_all(staging_dir);
    } else {
        eprintln!(
            "[helper] 拒绝清理 base_path 之外的 staging 目录: {}",
            staging_dir.display()
        );
    }
}

/// 获取当前平台的可执行文件名
fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "campus-auth.exe"
    } else {
        "campus-auth"
    }
}

/// 校验路径位于 base_path 之内（G13，防 pending.json 篡改的路径逃逸）
///
/// 双方 canonicalize（解析为绝对真实路径，含符号链接折叠与 Windows `\\?\`
/// 前缀归一）后做 `starts_with` 前缀比较；任一路径不存在（canonicalize 失败）
/// 均视为不合法——合法流程走到此处时 staging 与 target 必然已经存在。
fn is_within_base(path: &Path, base_path: &Path) -> bool {
    let (Ok(canonical), Ok(base_canonical)) = (path.canonicalize(), base_path.canonicalize())
    else {
        return false;
    };
    canonical.starts_with(&base_canonical)
}

/// 计算文件 SHA256（hex 小写）
fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// G13：替换前复核 staging exe 的 SHA256
///
/// `expected` 为空（发布源未提供伴随 .sha256，与 G12 的"信任 HTTPS"降级一致）
/// 时跳过复核；非空但与实际不符时返回 false——staging 损坏或被篡改，必须中止
/// 替换（调用方随后清理不可信的 staging）。
fn verify_staging_sha256(extracted_exe: &Path, expected: &str) -> bool {
    if expected.is_empty() {
        eprintln!("[helper] pending.json 未携带 SHA256，跳过 staging 复核（信任 HTTPS 下载链路）");
        return true;
    }
    match file_sha256(extracted_exe) {
        Ok(actual) if actual.eq_ignore_ascii_case(expected) => true,
        Ok(actual) => {
            eprintln!("[helper] SHA256 不匹配: expected={expected}, got={actual}");
            false
        }
        Err(e) => {
            eprintln!("[helper] 计算 staging SHA256 失败: {e}");
            false
        }
    }
}

/// 探测子进程存活状态（G13）
///
/// 返回值：`Ok(None)` = 仍在运行；`Ok(Some(code))` = 已退出（退出码，信号
/// 终止等无退出码场景以 -1 表示）；`Err(())` = try_wait 系统调用失败。
fn probe_alive(child: &mut std::process::Child) -> Result<Option<i32>, ()> {
    child
        .try_wait()
        .map(|status| status.map(|st| st.code().unwrap_or(-1)))
        .map_err(|_| ())
}

/// G13：根据新进程的两次探测结果决定是否删除备份
///
/// 仅当首查存活且 5 秒后复查仍存活（两次均为 `Ok(None)`）时才删除 .bak；
/// 其余组合（任一次已退出 / 探测失败 / 首查失败未二查）一律保留备份。
fn decide_backup_deletion(
    first_probe: Result<Option<i32>, ()>,
    second_probe: Result<Option<i32>, ()>,
) -> bool {
    matches!((first_probe, second_probe), (Ok(None), Ok(None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G13：路径逃逸防护——base_path 内的路径放行，外部路径与不存在路径拒绝
    #[test]
    fn test_is_within_base() {
        let base = tempfile::tempdir().unwrap();
        let inside = base.path().join("update").join("staging");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(is_within_base(&inside, base.path()));

        // 存在但位于 base 之外的路径必须拒绝
        let outside = tempfile::tempdir().unwrap();
        assert!(!is_within_base(outside.path(), base.path()));

        // 不存在的路径 canonicalize 失败 → 拒绝
        assert!(!is_within_base(
            &base.path().join("does-not-exist"),
            base.path()
        ));
    }

    /// G13：SHA 复核——正确值通过、错误值拒绝、空值降级跳过、缺失文件拒绝
    #[test]
    fn test_verify_staging_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("campus-auth.exe");
        std::fs::write(&exe, b"staged-binary-content").unwrap();

        use sha2::{Digest, Sha256};
        let correct = hex::encode(Sha256::digest(b"staged-binary-content"));

        // 空期望：降级跳过复核（返回 true，允许继续替换）
        assert!(verify_staging_sha256(&exe, ""));
        // 正确摘要：通过（大小写不敏感）
        assert!(verify_staging_sha256(&exe, &correct));
        assert!(verify_staging_sha256(&exe, &correct.to_uppercase()));
        // 错误摘要：拒绝
        assert!(!verify_staging_sha256(&exe, "deadbeef"));
        // 文件缺失：拒绝
        assert!(!verify_staging_sha256(
            &dir.path().join("missing.exe"),
            &correct
        ));
    }

    /// G13：延迟删备份决策——仅"两次探测均存活"才允许删除
    #[test]
    fn test_decide_backup_deletion() {
        // 首查存活 + 5 秒后仍存活 → 删除
        assert!(decide_backup_deletion(Ok(None), Ok(None)));
        // 首查已退出（无需二查，重复传入首查值）→ 保留
        assert!(!decide_backup_deletion(Ok(Some(0)), Ok(Some(0))));
        // 首查存活、复查已退出（运行数秒后崩溃）→ 保留
        assert!(!decide_backup_deletion(Ok(None), Ok(Some(1))));
        // 任一次探测失败 → 保留（保守）
        assert!(!decide_backup_deletion(Ok(None), Err(())));
        assert!(!decide_backup_deletion(Err(()), Err(())));
    }

    /// file_sha256 与已知摘要一致
    #[test]
    fn test_file_sha256_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("data.bin");
        std::fs::write(&f, b"hello campus-auth").unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            file_sha256(&f).unwrap(),
            hex::encode(Sha256::digest(b"hello campus-auth"))
        );
    }
}
