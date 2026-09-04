//! 更新 helper 独立入口：等待主进程退出 -> 替换 exe -> 启动新 exe -> 清理
//!
//! 由主进程的 `UpdaterService::spawn_helper()` spawn，接收 `--pid` 参数。
//! 从 `<base_path>/update/pending.json` 读取 staging / target 信息，
//! 等待主进程退出后完成替换并重启新版本。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use campus_auth::utils::lock::is_process_alive;
use chrono::Local;
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

/// helper 的 best-effort 落盘日志
///
/// GUI 子系统（Windows release 双击启动）下 stdout/stderr 完全不可见，更新失败
/// 将不可诊断。每条消息同时写入 `<base_path>/logs/helper.log`（追加）与 stderr：
/// 目录不存在则尝试创建；文件打开/写入失败仅退回 stderr，绝不 panic、绝不阻断
/// 更新流程（日志是尽力而为的旁路）。
struct HelperLog {
    file: Option<std::fs::File>,
}

impl HelperLog {
    /// 打开日志文件（失败则退回仅 stderr 模式）
    fn open(base_path: &Path) -> Self {
        let log_path = base_path.join("logs").join("helper.log");
        let file = std::fs::create_dir_all(log_path.parent().unwrap_or_else(|| Path::new(".")))
            .ok()
            .and_then(|()| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .ok()
            });
        Self { file }
    }

    /// 写一条日志：stderr（带 [helper] 前缀与时间戳）+ 文件追加
    fn write(&mut self, level: &str, msg: &str) {
        let line = format!(
            "[helper] {} [{level}] {msg}",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        eprintln!("{line}");
        if let Some(f) = self.file.as_mut() {
            // 写日志失败（磁盘满/句柄失效）静默放弃，不影响主流程
            let _ = writeln!(f, "{line}");
        }
    }

    fn info(&mut self, msg: &str) {
        self.write("INFO", msg);
    }

    fn error(&mut self, msg: &str) {
        self.write("ERROR", msg);
    }

    fn debug(&mut self, msg: &str) {
        self.write("DEBUG", msg);
    }
}

fn main() {
    let cli = HelperCli::parse();

    if !cli.apply_update {
        eprintln!("campus-auth-helper v{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // 0. 计算基准路径并初始化 best-effort 落盘日志
    // （GUI 子系统下 stdout/stderr 不可见，更新失败必须可从 helper.log 诊断）
    let base_path = cli.base_path.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let mut log = HelperLog::open(&base_path);

    // 1. 等待主进程退出
    log.info(&format!("等待主进程 (PID {}) 退出...", cli.pid));
    if !wait_for_process_exit(cli.pid) {
        // 主进程未退出：中止更新，保留 staging 与 pending.json，待主进程下次启动
        // 时由 apply_pending_on_startup 应用（不执行 cleanup，避免摧毁待应用更新）
        std::process::exit(1);
    }
    // 额外等待一小段时间，确保文件句柄完全释放
    sleep(Duration::from_millis(500));

    // 2. 从 pending.json 读取配置（CLI 参数优先）
    let pending_path = base_path.join("update").join("pending.json");
    // 区分三种情形：不存在（正常，按 CLI 参数继续）/ 解析失败（丢失 sha256 与
    // original_args，告警）/ 其他读取错误（告警）
    let pending: Option<PendingInfo> = match std::fs::read_to_string(&pending_path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(p) => Some(p),
            Err(e) => {
                log.error(&format!(
                    "pending.json 存在但解析失败（{e}），sha256/original_args 信息丢失，将按 CLI 参数继续"
                ));
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log.debug("无 pending.json，按 CLI 参数继续");
            None
        }
        Err(e) => {
            log.error(&format!("读取 pending.json 失败（{e}），将按 CLI 参数继续"));
            None
        }
    };

    let staging_dir = match cli
        .staging
        .or_else(|| pending.as_ref().map(|p| PathBuf::from(&p.staging_dir)))
    {
        Some(p) => p,
        None => {
            log.error("缺少 staging 目录路径（需 --staging 或 pending.json）");
            std::process::exit(1);
        }
    };

    let target_exe = match cli
        .target
        .or_else(|| pending.as_ref().map(|p| PathBuf::from(&p.target_exe)))
    {
        Some(p) => p,
        None => {
            log.error("缺少目标 exe 路径（需 --target 或 pending.json）");
            std::process::exit(1);
        }
    };

    // G13：staging / target 路径必须位于 base_path 之内（canonicalize 后
    // starts_with 检查）。二者取值可能来自 pending.json——文件被篡改时会把
    // 任意系统路径变成替换目标（任意位置覆写）或清理对象，必须先拒绝。
    if !is_within_base(&staging_dir, &base_path) {
        log.error(&format!(
            "拒绝执行：staging 路径不在 base_path 之内: {}",
            staging_dir.display()
        ));
        std::process::exit(1);
    }
    if !is_within_base(&target_exe, &base_path) {
        log.error(&format!(
            "拒绝执行：target 路径不在 base_path 之内: {}",
            target_exe.display()
        ));
        std::process::exit(1);
    }

    let extracted_exe = staging_dir.join("extracted").join(exe_name());

    // 3. 校验 staging 文件存在
    if !extracted_exe.exists() {
        log.error(&format!("staging 文件不存在: {}", extracted_exe.display()));
        cleanup(&base_path, &staging_dir, &mut log);
        std::process::exit(1);
    }

    // 3.5 G13：复制前复核 staging exe 的 SHA256（期望值从 pending.json 传入）
    // 校验失败（staging 损坏/被篡改）时中止替换并清理不可信的 staging
    let expected_sha = pending
        .as_ref()
        .map(|p| p.sha256.as_str())
        .unwrap_or_default();
    if !verify_staging_sha256(&extracted_exe, expected_sha) {
        log.error("staging exe SHA256 复核失败，中止替换");
        cleanup(&base_path, &staging_dir, &mut log);
        std::process::exit(1);
    }

    // 4. 备份旧 exe（统一 "<原名>.bak"：unix 上 with_extension("exe.bak") 会产出
    // campus-auth.exe.bak 的怪名——无扩展名文件被凭空拼出 .exe）
    let backup_path = target_exe
        .file_name()
        .map(|n| target_exe.with_file_name(format!("{}.bak", n.to_string_lossy())))
        .unwrap_or_else(|| target_exe.with_extension("exe.bak"));
    if target_exe.exists() {
        log.info(&format!("备份旧版本 -> {}", backup_path.display()));
        if let Err(e) = std::fs::copy(&target_exe, &backup_path) {
            log.error(&format!(
                "备份失败: {e}，将在无备份情况下继续替换，失败后无法回滚"
            ));
            // 备份失败不阻断替换流程
        }
    }

    // 5. 替换 exe（helper 复制新文件覆盖旧 exe，而非替换自身）
    log.info(&format!(
        "替换 {} -> {}",
        extracted_exe.display(),
        target_exe.display()
    ));
    if let Err(e) = std::fs::copy(&extracted_exe, &target_exe) {
        log.error(&format!("替换失败: {e}"));
        // 尝试回退：从备份恢复
        if backup_path.exists() {
            match std::fs::copy(&backup_path, &target_exe) {
                Ok(_) => log.error("已回退到备份版本"),
                Err(e) => log.error(&format!(
                    "回退失败（{e}），exe 处于未知状态，请手动用备份 {} 恢复",
                    backup_path.display()
                )),
            }
        }
        cleanup(&base_path, &staging_dir, &mut log);
        std::process::exit(1);
    }
    // unix：fs::copy 只复制源文件权限，为防解压链路丢 +x，替换后显式确保
    // 新二进制可执行——否则重启必然 Exec format/permission 失败
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&target_exe, std::fs::Permissions::from_mode(0o755))
        {
            log.debug(&format!("设置新 exe 可执行权限失败: {e}"));
        }
    }

    // 5.5 同步全量分发内容：更新包不只有 exe——python_worker/（Python 代码）、
    // resources/、docs/ 同为新版本的一部分，只换 exe 会让 Python 侧修复
    // （如反馈资源快照）永远到不了走应用内更新的用户。overlay 语义：覆盖同名
    // 文件、新增缺失文件、绝不删除目标侧多余内容（python_worker/.venv 是
    // 用户运行态，config/tasks/logs 等用户数据不在 staging 内天然不受影响）。
    let extracted_dir = staging_dir.join("extracted");
    sync_distribution_files(&extracted_dir, &base_path);
    replace_helper(&extracted_dir, &base_path, &mut log);

    // 6. 启动新 exe（传递原始启动参数）
    let original_args = pending
        .as_ref()
        .map(|p| p.original_args.clone())
        .unwrap_or_default();
    log.info("启动新版本...");
    match std::process::Command::new(&target_exe)
        .args(&original_args)
        .spawn()
    {
        Ok(mut child) => {
            log.info("新版本已启动");
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
                if let Err(e) = std::fs::remove_file(&backup_path) {
                    log.debug(&format!("清理备份文件失败: {e}"));
                }
                log.info("新版本持续运行，已清理备份");
            } else {
                log.error(&format!(
                    "新版本启动后疑似异常退出，保留备份 {} 供手动回退",
                    backup_path.display()
                ));
            }
        }
        Err(e) => {
            log.error(&format!("启动新版本失败: {e}"));
            log.error(&format!("保留备份 {} 供手动回退", backup_path.display()));
        }
    }

    // 7. 清理
    cleanup(&base_path, &staging_dir, &mut log);

    log.info("更新完成");
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

/// overlay 同步更新包内的分发目录到 base_path（步骤 5.5）
///
/// 覆盖 `resources/`、`docs/`、`python_worker/`（Python 源码与 pyproject/uv.lock）。
/// `skip_names` 命中的目录名整棵子树跳过——发布包本就不含这些（release.yml 已排除），
/// 此处是防御性双保险：`.venv` 是用户引导出的运行态，`__pycache__` 运行时自动再生。
/// best-effort：单文件失败仅告警继续，不回滚（exe 已替换，半新半旧由下次更新收敛）。
fn sync_distribution_files(extracted_dir: &Path, base_path: &Path) {
    for dir in ["resources", "docs", "python_worker"] {
        let src = extracted_dir.join(dir);
        if !src.exists() {
            continue;
        }
        let dst = base_path.join(dir);
        println!("[helper] 同步 {dir}/ -> {}", dst.display());
        if let Err(e) = copy_dir_overlay(&src, &dst, &[".venv", "__pycache__"]) {
            eprintln!("[helper] 同步 {dir}/ 失败（继续）: {e}");
        }
    }
}

/// 递归 overlay 复制目录：目标侧不存在的路径创建，已存在的文件覆盖
fn copy_dir_overlay(src: &Path, dst: &Path, skip_names: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if skip_names.contains(&name_str) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            copy_dir_overlay(&src_path, &dst_path, skip_names)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 替换 helper 自身（步骤 5.5，best-effort）
///
/// Windows 不允许覆盖写运行中的 exe，但允许 rename：先把旧 helper 改名
/// `<原名>.old` 让位，再复制新版本；最后尝试删 .old（运行中删除会失败，
/// 残留一个无害文件，下次更新覆盖重试）。任一步失败仅告警——旧 helper
/// 依然能完成未来的 exe 替换（接口仅依赖 pending.json 文件，保持稳定）。
fn replace_helper(extracted_dir: &Path, base_path: &Path, log: &mut HelperLog) {
    let helper_name = if cfg!(target_os = "windows") {
        "campus-auth-helper.exe"
    } else {
        "campus-auth-helper"
    };
    let new_helper = extracted_dir.join(helper_name);
    if !new_helper.exists() {
        return;
    }
    let target = base_path.join(helper_name);
    let old = base_path.join(format!("{helper_name}.old"));
    if let Err(e) = std::fs::remove_file(&old) {
        log.debug(&format!("清理残留的 {} 失败: {e}", old.display()));
    }
    if target.exists() {
        if let Err(e) = std::fs::rename(&target, &old) {
            eprintln!("[helper] 旧 helper 改名失败，跳过 helper 自更新: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::copy(&new_helper, &target) {
        eprintln!("[helper] helper 替换失败: {e}，恢复旧版本");
        if let Err(e) = std::fs::rename(&old, &target) {
            log.debug(&format!(
                "恢复旧 helper 失败（{} -> {}）: {e}",
                old.display(),
                target.display()
            ));
        }
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)) {
            log.debug(&format!("设置新 helper 可执行权限失败: {e}"));
        }
    }
    println!("[helper] helper 已更新");
    if let Err(e) = std::fs::remove_file(&old) {
        log.debug(&format!("清理 {} 失败: {e}", old.display()));
    }
}

/// 清理 pending.json 标记与 staging 目录（staging 目录用 CLI --staging 传入的实际路径）
///
/// G13：staging 取值可能来自被篡改的 pending.json，remove_dir_all 前复核其
/// 确实位于 base_path 之内，避免 cleanup 变成任意目录删除。
fn cleanup(base_path: &Path, staging_dir: &Path, log: &mut HelperLog) {
    let pending_path = base_path.join("update").join("pending.json");
    if let Err(e) = std::fs::remove_file(&pending_path) {
        log.debug(&format!("清理 pending.json 失败: {e}"));
    }
    if is_within_base(staging_dir, base_path) {
        if let Err(e) = std::fs::remove_dir_all(staging_dir) {
            log.debug(&format!("清理 staging 目录失败: {e}"));
        }
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
/// `expected` 为空直接拒绝（不再降级信任 HTTPS）；非空但与实际不符时返回
/// false——staging 损坏或被篡改，必须中止替换（调用方随后清理不可信 staging）。
fn verify_staging_sha256(extracted_exe: &Path, expected: &str) -> bool {
    if expected.is_empty() {
        eprintln!("[helper] pending.json 未携带 SHA256，已拒绝替换（需补校验值）");
        return false;
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

    /// G13：SHA 复核——正确值通过、错误值拒绝、空值拒绝（P1-3：缺失拒绝，不降级）、文件缺失拒绝
    #[test]
    fn test_verify_staging_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("campus-auth.exe");
        std::fs::write(&exe, b"staged-binary-content").unwrap();

        use sha2::{Digest, Sha256};
        let correct = hex::encode(Sha256::digest(b"staged-binary-content"));

        // 空期望：拒绝（缺失拒绝，不降级跳过）
        assert!(!verify_staging_sha256(&exe, ""));
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

    /// overlay 同步：覆盖同名文件、新增缺失文件、跳过 .venv/__pycache__、
    /// 不删除目标侧独有内容
    #[test]
    fn test_copy_dir_overlay() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");

        // 源：worker 新版文件 + 意外混入的 .venv（防御性跳过）
        std::fs::create_dir_all(src.join("tests")).unwrap();
        std::fs::write(src.join("playwright_worker.py"), b"NEW").unwrap();
        std::fs::write(src.join("tests").join("t.py"), b"new-test").unwrap();
        std::fs::create_dir_all(src.join(".venv").join("Scripts")).unwrap();
        std::fs::write(src.join(".venv").join("Scripts").join("python.exe"), b"x").unwrap();

        // 目标：旧版同名文件 + 用户运行态 .venv + 目标独有文件
        std::fs::create_dir_all(dst.join(".venv").join("Scripts")).unwrap();
        std::fs::write(dst.join("playwright_worker.py"), b"OLD").unwrap();
        std::fs::write(
            dst.join(".venv").join("Scripts").join("python.exe"),
            b"USER-VENV",
        )
        .unwrap();
        std::fs::write(dst.join("user-config-only.txt"), b"KEEP").unwrap();

        copy_dir_overlay(&src, &dst, &[".venv", "__pycache__"]).unwrap();

        // 同名覆盖
        assert_eq!(
            std::fs::read(dst.join("playwright_worker.py")).unwrap(),
            b"NEW"
        );
        // 新增缺失
        assert_eq!(
            std::fs::read(dst.join("tests").join("t.py")).unwrap(),
            b"new-test"
        );
        // 目标侧 .venv 保留（用户运行态），src 侧 .venv 不入侵
        assert_eq!(
            std::fs::read(dst.join(".venv").join("Scripts").join("python.exe")).unwrap(),
            b"USER-VENV"
        );
        // 目标独有内容不删除
        assert!(dst.join("user-config-only.txt").exists());
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
