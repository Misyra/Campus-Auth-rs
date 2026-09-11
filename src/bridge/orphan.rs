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

// debug! 仅 unix 分支（/proc 扫描）使用：按平台拆分导入，避免 Windows 上
// unused_imports 在 -D warnings 下报错
#[cfg(unix)]
use tracing::{debug, info, warn};
#[cfg(not(unix))]
use tracing::{info, warn};

/// 清理上次崩溃残留的孤儿浏览器进程（best-effort）。
pub fn cleanup_orphan_browsers() {
    let count = match cleanup_orphan_browsers_inner() {
        Ok(n) => n,
        Err(e) => {
            // 解析失败意味着孤儿清理整体失效（残留进程将持续占用资源），
            // 必须以 warn 级别暴露，而不是静默跳过
            warn!("孤儿浏览器进程清理失败: {e}");
            0
        }
    };
    if count > 0 {
        info!(target: "python_worker", "清理了 {count} 个孤儿浏览器进程");
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
    // 注意：ConvertTo-Csv 默认给所有字段加双引号（"ProcessId","123"...），
    // 必须剥去引号后再比较/解析，否则列定位与 pid 解析全部失败
    let header = lines
        .next()
        .ok_or("进程枚举无输出")?
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
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
        let pid = parts[idx_pid].trim().trim_matches('"').parse::<u32>().ok();
        let ppid = parts[idx_ppid].trim().trim_matches('"').parse::<u32>().ok();
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
        // 枚举与 kill 之间进程可能退出、PID 复用或父进程刚出现；破坏性操作前
        // 重新读取候选 cmdline/ppid，并以实时 /proc 父目录存在性复核。
        if (ppid == 1 || !pid_to_ppid.contains(&ppid))
            && still_orphan_chromium(pid, ppid)
            && kill_pid(pid)
        {
            killed += 1;
        }
    }
    Ok(killed)
}

/// kill 前复核候选仍是同一 Chromium 进程，且其父进程实时不存在或已被 init 收养。
#[cfg(unix)]
fn still_orphan_chromium(pid: u32, observed_ppid: u32) -> bool {
    let process_dir = std::path::PathBuf::from(format!("/proc/{pid}"));
    let Ok(stat) = std::fs::read_to_string(process_dir.join("stat")) else {
        return false;
    };
    let Ok(current_ppid) = parse_ppid_from_stat(&stat) else {
        return false;
    };
    if current_ppid != observed_ppid {
        return false;
    }
    let cmdline = std::fs::read(process_dir.join("cmdline")).unwrap_or_default();
    let command = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    is_chromium(&command)
        && (current_ppid == 1 || !std::path::Path::new(&format!("/proc/{current_ppid}")).exists())
}

/// 从 /proc/<pid>/stat 解析 ppid（第三个字段）
#[cfg(unix)]
fn parse_ppid_from_stat(stat: &str) -> Result<u32, String> {
    // 格式：pid (comm) state ppid ...
    // comm 为任意字符串（含空格/括号），字段起点是最后一个 ')' 之后
    //（首个 ')' 在 comm 含括号时会切进 comm 内部，见 proc(5)）
    let after_comm = stat
        .rsplit_once(')')
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
    // 先确认 Chrome 系二进制，再确认 headless/调试特征：
    // 裸 `--headless` 会误伤 `firefox --headless` 等非 Chrome 进程，
    // 而 kill 前仅有"父进程已死"一道闸，不足以兜底误杀。
    // 漏判（少清一个残留）无害，误判（杀掉用户进程）不可接受，故偏向严格。
    let lower = cmd.to_ascii_lowercase();
    let chrome_binary = lower.contains("chrom") || lower.contains("headless_shell");
    let headless_flag = cmd.contains("--headless") || cmd.contains("--remote-debugging-port");
    // headless_shell 二进制名本身足够独特（Playwright headless 专用），单列即命中
    (chrome_binary && headless_flag) || lower.contains("headless_shell")
}

/// 通过 taskkill 强杀（Windows）
///
/// `/T` 递归终止进程树：chromium 的 renderer/GPU 等子进程命令行
/// 不含 `--headless` 等特征，不会被列入候选，仅杀主进程会留下子进程孤儿
#[cfg(windows)]
fn kill_pid(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
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
#[cfg(test)]
mod tests {
    use super::*;

    /// chromium 判定：Chrome 系二进制 + headless/调试特征才命中，普通浏览器不误杀
    #[test]
    fn test_is_chromium_matrix() {
        assert!(is_chromium("/opt/chromium --headless --disable-gpu"));
        assert!(is_chromium("chrome --remote-debugging-port=9222"));
        assert!(is_chromium("/tmp/headless_shell --foo"));
        assert!(is_chromium(
            "C:\\pw\\chromium-1228\\chrome-win\\chrome.exe --headless"
        ));
        // firefox --headless 曾被裸 `--headless` 子串误判命中
        assert!(!is_chromium("/usr/bin/firefox --headless"));
        assert!(!is_chromium(
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
        ));
        assert!(!is_chromium("/usr/bin/firefox"));
        assert!(!is_chromium("node server.js"));
        assert!(!is_chromium(""));
    }

    /// stat 解析：comm 含空格/括号时仍定位 ppid，格式异常返回错误
    #[cfg(unix)]
    #[test]
    fn test_parse_ppid_from_stat() {
        assert_eq!(
            parse_ppid_from_stat("123 (chromium) S 1 123 123 0 -1 4194304").expect("解析"),
            1
        );
        assert_eq!(
            parse_ppid_from_stat("9 (my app) S 7 9 9 0 -1 4194304").expect("含空格 comm"),
            7
        );
        // comm 含括号时首个 ')' 会切进 comm 内部，必须用最后一个 ')' 定位
        assert_eq!(
            parse_ppid_from_stat("9 (my app (1)) S 7 9 9 0 -1 4194304").expect("含括号 comm"),
            7
        );
        assert!(parse_ppid_from_stat("garbage").is_err());
    }
}
