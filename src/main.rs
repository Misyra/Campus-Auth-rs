//! Campus-Auth 校园网自动认证工具 — 应用入口
//!
//! CLI 参数解析 -> tracing 初始化 -> 特殊命令处理 -> 构建 tokio Runtime -> 启动 launcher。

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
        tracing::error!("启动失败: {e}");
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
