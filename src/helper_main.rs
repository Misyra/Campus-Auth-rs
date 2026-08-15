//! 更新 helper 独立入口：等待主进程退出 -> 替换 exe -> 启动新 exe -> 清理
//!
//! 由主进程的 `UpdaterService::spawn_helper()` spawn，接收 `--pid` 参数。
//! 从 `<base_path>/update/pending.json` 读取 staging / target 信息，
//! 等待主进程退出后完成替换并重启新版本。

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

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

    let extracted_exe = staging_dir.join("extracted").join(exe_name());

    // 3. 校验 staging 文件存在
    if !extracted_exe.exists() {
        eprintln!("[helper] staging 文件不存在: {}", extracted_exe.display());
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
        Ok(_) => println!("[helper] 新版本已启动"),
        Err(e) => eprintln!("[helper] 启动新版本失败: {e}"),
    }

    // 7. 清理
    cleanup(&base_path, &staging_dir);

    // 删除备份（替换成功后不再需要）
    let _ = std::fs::remove_file(&backup_path);

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
fn cleanup(base_path: &Path, staging_dir: &Path) {
    let pending_path = base_path.join("update").join("pending.json");
    let _ = std::fs::remove_file(&pending_path);
    let _ = std::fs::remove_dir_all(staging_dir);
}

/// 获取当前平台的可执行文件名
fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "campus-auth.exe"
    } else {
        "campus-auth"
    }
}

/// 检查指定 PID 的进程是否存活
#[cfg(target_os = "windows")]
fn is_process_alive(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut core::ffi::c_void;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

/// 检查指定 PID 的进程是否存活（Linux，通过 procfs 判断）
#[cfg(all(not(target_os = "windows"), target_os = "linux"))]
fn is_process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// 检查指定 PID 的进程是否存活（macOS，无 procfs，使用 kill -0 探测）
#[cfg(all(not(target_os = "windows"), target_os = "macos"))]
fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) 不实际发送信号，仅校验进程存在性与权限；返回 0 表示进程存活
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
