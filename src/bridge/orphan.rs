//! 孤儿浏览器进程清理
//!
//! 启动时与 Worker 崩溃恢复时调用，清理上次崩溃残留的 chromium 进程。
//! 判定规则（两条同时成立才 kill，避免误杀用户其他浏览器）：
//! 1. 命令行匹配 chromium 特征（`--headless` / `--remote-debugging` / headless_shell 等）
//! 2. 父进程已不存在（孤儿）
//!
//! 全程 best-effort：任何枚举/解析错误仅记录日志，不向上抛出。

use std::collections::HashSet;
use std::process::Command;

use tracing::{debug, warn};

/// 清理上次崩溃残留的孤儿浏览器进程（best-effort）。
pub fn cleanup_orphan_browsers() {
    let count = match cleanup_orphan_browsers_inner() {
        Ok(n) => n,
        Err(e) => {
            debug!("孤儿浏览器进程清理跳过: {e}");
            0
        }
    };
    if count > 0 {
        warn!(target: "python_worker", "清理了 {count} 个孤儿浏览器进程");
    }
}

/// 跨平台清理实现
#[cfg(windows)]
fn cleanup_orphan_browsers_inner() -> Result<usize, String> {
    use std::os::windows::process::CommandExt;
    // 使用 Get-CimInstance 替代已废弃的 wmic
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,CommandLine | ConvertTo-Csv -NoTypeInformation",
        ])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW：避免弹黑窗
        .output()
        .map_err(|e| format!("Get-CimInstance 执行失败: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    // 首行为表头，用于定位列索引
    let header = lines
        .next()
        .ok_or("进程枚举无输出")?
        .split(',')
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>();
    let idx_pid = header
        .iter()
        .position(|c| c.eq_ignore_ascii_case("ProcessId"))
        .ok_or("缺失 ProcessId 列")?;
    let idx_ppid = header
        .iter()
        .position(|c| c.eq_ignore_ascii_case("ParentProcessId"))
        .ok_or("缺失 ParentProcessId 列")?;
    let idx_cmd = header
        .iter()
        .position(|c| c.eq_ignore_ascii_case("CommandLine"))
        .ok_or("缺失 CommandLine 列")?;

    let mut alive_pids: HashSet<u32> = HashSet::new();
    let mut candidates: Vec<(u32, u32)> = Vec::new();
    for line in lines {
        // CommandLine 为最后一列，可能含逗号，故按列数上限拆分保留尾部
        let parts: Vec<&str> = line.splitn(idx_cmd + 1, ',').collect();
        if parts.len() <= idx_cmd {
            continue;
        }
        let pid = parts[idx_pid].trim().parse::<u32>().ok();
        let ppid = parts[idx_ppid].trim().parse::<u32>().ok();
        let cmd = parts[idx_cmd];
        let (Some(pid), Some(ppid)) = (pid, ppid) else {
            continue;
        };
        alive_pids.insert(pid);
        if is_chromium(cmd) {
            candidates.push((pid, ppid));
        }
    }

    let mut killed = 0;
    for (pid, ppid) in candidates {
        // 父进程不存在（孤儿）才强杀
        if !alive_pids.contains(&ppid) && kill_pid(pid) {
            killed += 1;
        }
    }
    Ok(killed)
}

/// 跨平台清理实现（Unix：读取 /proc）
#[cfg(unix)]
fn cleanup_orphan_browsers_inner() -> Result<usize, String> {
    let proc = std::fs::read_dir("/proc").map_err(|e| format!("/proc 读取失败: {e}"))?;
    let mut pid_to_ppid: HashSet<u32> = HashSet::new();
    let mut candidates: Vec<(u32, u32)> = Vec::new();

    for entry in proc.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // 读取 ppid（/proc/<pid>/stat 的第三个字段）
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // 单个进程 stat 解析失败不应中止整个清理（与同函数其他错误处理一致），跳过该进程
        let ppid = match parse_ppid_from_stat(&stat) {
            Ok(p) => p,
            Err(e) => {
                debug!("解析进程 {pid} 的 ppid 失败: {e}");
                continue;
            }
        };
        pid_to_ppid.insert(pid);
        // 读取 cmdline 判断是否为 chromium
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let cmd = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if is_chromium(&cmd) {
            candidates.push((pid, ppid));
        }
    }

    let mut killed = 0;
    for (pid, ppid) in candidates {
        // 父进程不存在（含被 init 收养 ppid==1 的情况）即视为孤儿
        if ppid == 1 || !pid_to_ppid.contains(&ppid) {
            if kill_pid(pid) {
                killed += 1;
            }
        }
    }
    Ok(killed)
}

/// 从 /proc/<pid>/stat 解析 ppid（第三个字段）
#[cfg(unix)]
fn parse_ppid_from_stat(stat: &str) -> Result<u32, String> {
    // 格式：pid (comm) state ppid ...
    // comm 可能含空格/括号，故用首个 ')' 后定位
    let after_comm = stat
        .split_once(')')
        .map(|(_, rest)| rest)
        .ok_or("stat 格式异常")?;
    let ppid = after_comm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("ppid 解析失败")?;
    Ok(ppid)
}

/// 判断命令行是否匹配 chromium 特征（仅匹配 headless/debug Chrome，避免误杀普通浏览器）
fn is_chromium(cmd: &str) -> bool {
    cmd.contains("--headless")
        || cmd.contains("--remote-debugging-port")
        || cmd.contains("headless_shell")
}

/// 通过 taskkill 强杀（Windows）
#[cfg(windows)]
fn kill_pid(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .creation_flags(0x08000000)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 通过 kill -9 强杀（Unix）
#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
