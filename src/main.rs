//! Campus-Auth 校园网自动认证工具 — 应用入口
//!
//! CLI 参数解析 -> tracing 初始化 -> 特殊命令处理 -> 构建 tokio Runtime -> 启动 launcher。

// Windows release 构建改用 GUI 子系统：双击 exe 不再弹出控制台窗口。
// debug 构建保持控制台子系统，保证 cargo run 与集成测试的 stdout 可捕获；
// release 从终端启动时的输出可见性由 attach_parent_console 兜底。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use campus_auth::launcher::{AutostartAction, CliArgs};
use clap::Parser;

/// 解析基准路径：CLI 参数 > exe 所在目录 > 当前目录
fn resolve_base_path(cli: &CliArgs) -> PathBuf {
    if let Some(ref p) = cli.base_path {
        if p.is_absolute() {
            return p.clone();
        }
        return std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn main() -> anyhow::Result<()> {
    // 0. GUI 子系统下从终端启动时重新接回控制台输出（必须在任何 println 之前）
    #[cfg(windows)]
    attach_parent_console();

    // 1. CLI 解析
    let cli = CliArgs::parse();

    // 2. tracing subscriber 不在此处初始化，由 launcher::init_file_logging 统一注册
    //    （全局 subscriber 只能 init 一次，提前 init 会导致文件日志层和广播层注册失败）

    let base_path = resolve_base_path(&cli);

    // 3. 处理特殊命令（直接执行后退出，不进入主流程）
    if cli.status {
        return handle_status(&base_path);
    }
    if cli.stop {
        return handle_stop(&base_path);
    }
    if let Some(ref action) = cli.autostart {
        return handle_autostart(action);
    }

    // 4. 构建 tokio Runtime -> block_on(launcher::run)
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if let Err(e) = runtime.block_on(campus_auth::launcher::run(cli, base_path)) {
        // 双写系有意：subscriber 初始化失败（或尚未注册）时 error! 无处输出，
        // eprintln 是此时唯一可见通道
        tracing::error!("启动失败: {e}");
        // 实例锁等早期错误发生在日志系统初始化之前（tracing 无 subscriber，
        // error! 会丢失），必须同步落 stderr 才能让用户看见失败原因
        eprintln!("启动失败: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

/// 查询运行实例状态
fn handle_status(base_path: &Path) -> anyhow::Result<()> {
    match campus_auth::utils::lock::query_instance(base_path) {
        Some(info) => {
            println!("实例运行中");
            println!("  PID:      {}", info.pid);
            if info.port == 0 {
                println!("  端口:     未监听（轻量模式，Web 控制台按需启动）");
            } else {
                println!("  端口:     {}", info.port);
            }
            println!("  进程存活: {}", info.running);
            if let Some(uptime) = info.uptime {
                println!("  运行时长: {:.0}s", uptime.as_secs_f64());
            }
            Ok(())
        }
        None => {
            println!("没有运行中的实例");
            std::process::exit(0);
        }
    }
}

/// 停止运行实例
fn handle_stop(base_path: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(campus_auth::utils::lock::stop_instance(base_path)) {
        Ok(()) => {
            println!("实例已停止");
            Ok(())
        }
        Err(e) => {
            eprintln!("停止失败: {e}");
            std::process::exit(1);
        }
    }
}

/// 注册 / 取消开机自启动
fn handle_autostart(action: &AutostartAction) -> anyhow::Result<()> {
    let enabled = matches!(action, AutostartAction::Enable);
    campus_auth::utils::platform::set_self_start(enabled)?;
    if enabled {
        println!("已注册开机自启动");
    } else {
        println!("已取消开机自启动");
    }
    Ok(())
}

/// Windows：为 GUI 子系统的 release 构建重新接回控制台输出
///
/// release 构建为 `windows_subsystem = "windows"`（双击不弹控制台），但从
/// cmd / PowerShell 启动时 stdout/stderr 不会自动连接终端，`--status` /
/// `--stop` 等子命令会失去输出。此处附着父进程控制台，并把标准输出/错误句柄
/// 显式指向 CONOUT$（AttachConsole 不保证刷新进程标准句柄表）。
///
/// 双击启动时父进程为 explorer（无控制台），附着失败直接返回；此后标准句柄
/// 保持无效，Rust std 对无效标准句柄的写入按静默丢弃处理，不影响启动流程。
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };

    // SAFETY：以下 Win32 调用仅影响当前进程的控制台关联与标准句柄表；
    // 任一步失败（无控制台 / 已附着 / CONOUT$ 打开失败）均无窗口等副作用，
    // 统一静默忽略，不阻断启动流程。
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // 仅补齐缺失的标准句柄：已被重定向的有效句柄（文件/管道）必须原样保留，
        // 否则 `--status > out.txt` 这类重定向会被覆写成控制台导致输出丢失
        let missing = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].into_iter().any(|id| {
            let cur = GetStdHandle(id);
            cur.is_null() || cur == INVALID_HANDLE_VALUE
        });
        if !missing {
            return;
        }
        // "CONOUT$" 的 UTF-16 编码（含结尾 NUL）
        let conout: &[u16] = &[0x0043, 0x004F, 0x004E, 0x004F, 0x0055, 0x0054, 0x0024, 0];
        let handle = CreateFileW(
            conout.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return;
        }
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
        // 故意不 CloseHandle：标准句柄需在整个进程生命周期内保持有效
    }
}
