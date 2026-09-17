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
        let cmd = unescape_csv_field(parts[idx_cmd]);
        let (Some(pid), Some(ppid)) = (pid, ppid) else {
            continue;
        };
        alive_pids.insert(pid);
        if is_chromium(&cmd) {
            candidates.push((pid, ppid));
        }
    }

    let mut killed = 0;
    for (pid, ppid) in candidates {
        // 父进程不存在（孤儿）才强杀
        if !alive_pids.contains(&ppid) && still_orphan_chromium(pid, ppid) && kill_pid(pid) {
            killed += 1;
        }
    }
    Ok(killed)
}

/// 反转义 `ConvertTo-Csv` 的字段：剥外层引号并把双写引号还原为单引号。
///
/// `ConvertTo-Csv` 按 RFC4180 把字段内的 `"` 转义为 `""`，且**总是**给字段加引号。
/// 因此真实命令行
/// `"C:\Program Files\Google\Chrome\Application\chrome.exe" --headless=new …`
/// 在 CSV 里变成 `"""C:\Program Files\…\chrome.exe"" --headless=new …"`。
///
/// 这一步对命令行的**语义判定**是必需的：浏览器安装路径普遍含空格、且被引号包住，
/// 不还原就会让按基名匹配的 [`is_chromium`] 全部落空（实测漏掉真实孤儿 chrome.exe）。
/// 旧实现用全命令行子串匹配（`contains`）对此免疫，故该缺陷是在收紧为基名匹配后
/// 才暴露的。
/// 仅 Windows 的 `Get-CimInstance` + `ConvertTo-Csv` 路径需要，unix 分支读
/// `/proc/<pid>/cmdline` 无 CSV 转义，不加上 `cfg(windows)` 会在 unix 下触发
/// dead_code（CI 的 -D warnings 直接失败）
#[cfg(windows)]
fn unescape_csv_field(field: &str) -> String {
    let inner = field
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(field);
    inner.replace("\"\"", "\"")
}

/// kill 前复核候选仍是同一 Chromium 进程，且其父进程**实时**已不存在（Windows）。
///
/// 与 unix 分支的 [`still_orphan_chromium`] 同源动机：`Get-CimInstance` 枚举在本机
/// 460 进程规模下实测耗时 ~710ms，这段窗口内进程可能已退出、PID 被复用、或父进程
/// 刚刚启动。不复核就直接 `taskkill /F /T` 会杀掉一个**与我方无关的新进程**。
///
/// 三道复核，任一不满足即放弃 kill（偏保守——漏清一个残留无害，误杀不可接受）：
/// 1. 实时重读该 PID 的映像路径，确认仍是浏览器可执行文件（防 PID 复用）；
/// 2. 实时重读其父 PID，确认与枚举时观察到的值一致（父进程未变）；
/// 3. 确认该父 PID 实时已不存在（仍为孤儿）。
#[cfg(windows)]
fn still_orphan_chromium(pid: u32, observed_ppid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    unsafe {
        // 复核 1：实时映像路径仍是浏览器（PID 未被复用的直接证据）
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // 进程已退出或无权限：无从确认身份，保守放弃
            return false;
        }
        let mut len = MAX_PATH;
        let mut buf = [0u16; 260];
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        let image_path = String::from_utf16_lossy(&buf[..len as usize]);
        // 复用 is_chromium 的基名判定：只传映像路径（无参数），headless 特征无法
        // 由此判定，故单独按基名判断是否为浏览器可执行文件
        if !is_browser_executable(&image_path) {
            return false;
        }

        // 复核 2 + 3：实时父 PID 未变，且该父进程已不存在
        let Some(current_ppid) = parent_pid_of(pid) else {
            return false;
        };
        current_ppid == observed_ppid && parent_pid_of(current_ppid).is_none()
    }
}

/// 判断映像路径是否为浏览器可执行文件（基名匹配，与 [`is_chromium`] 同一白名单）
#[cfg(windows)]
fn is_browser_executable(image_path: &str) -> bool {
    let lower = image_path.to_ascii_lowercase();
    let basename = lower
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(lower.as_str())
        .trim_matches('"');
    const BROWSER_BASENAMES: [&str; 8] = [
        "chrome.exe",
        "chrome",
        "chromium.exe",
        "chromium",
        "chrome-headless-shell.exe",
        "chrome-headless-shell",
        "headless_shell.exe",
        "headless_shell",
    ];
    BROWSER_BASENAMES.contains(&basename) || basename.starts_with("msedge")
}

/// 读取指定 PID 的父 PID（Windows）。
///
/// 经 `CreateToolhelp32Snapshot` 遍历进程表获取——`Get-CimInstance` 太慢（~700ms），
/// 而复核必须在 kill 前尽可能贴近内核状态。返回 `None` 表示进程不存在或无法读取
/// （两者对复核而言等价：无法确认即放弃 kill）。
#[cfg(windows)]
fn parent_pid_of(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = std::mem::zeroed::<PROCESSENTRY32W>();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == pid {
                    found = Some(entry.th32ParentProcessID);
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
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
///
/// 判定分两步，**先看可执行文件基名**再看 headless/调试特征：
/// - 基名白名单（`chrome.exe` / `chromium` / `headless_shell.exe` / `msedge.exe` 等）
///   是 Playwright 实际会拉起的浏览器可执行文件，普通进程不会以其为程序名；
/// - 再要求命令行含 `--headless` 或 `--remote-debugging-port`（或二进制本身即
///   `headless_shell`，其用途唯一）。
///
/// 历史上这里只做全命令行子串匹配（`含 "chrom" 且含 "--headless"`），任何命令行
/// **文本**同时出现这两类字样的进程都会被列为候选——实测 `cmd.exe /c echo chromium
/// --headless` 即可命中，脚本参数、含 `chrom` 的路径、甚至审查这个模块的搜索命令
/// 都会中招。kill 是破坏性操作，误判不可接受，故收紧为基名匹配。
///
/// 漏判（少清一个残留）无害：孤儿进程会在下次清理或进程退出时消失；误杀用户进程
/// 不可接受，故整体偏向严格。
fn is_chromium(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    // 取可执行文件基名：Windows 安装路径含空格时命令行会写成
    // `"C:\Program Files\…\chrome.exe" --headless`，按空白分割会把程序名截成
    // `"c:\program`，故须先处理引号（见 program_basename）。
    let Some(basename) = program_basename(&lower) else {
        return false;
    };

    const BROWSER_BASENAMES: [&str; 8] = [
        "chrome.exe",
        "chrome",
        "chromium.exe",
        "chromium",
        "chrome-headless-shell.exe",
        "chrome-headless-shell",
        "headless_shell.exe",
        "headless_shell",
    ];
    if !BROWSER_BASENAMES.contains(&basename) {
        // msedge / msedgewebview2 等 Edge 变体：基名以 msedge 开头
        if !basename.starts_with("msedge") {
            return false;
        }
    }
    // headless_shell 用途唯一（Playwright headless 专用），单列即命中
    if basename.contains("headless_shell") || basename.contains("headless-shell") {
        return true;
    }
    lower.contains("--headless") || lower.contains("--remote-debugging-port")
}

/// 从（小写）命令行取可执行文件**基名**。
///
/// 两种写法都要处理：
/// - 引号包裹（路径含空格时的常见形态）：取引号内内容；
/// - 裸路径：取首个空白分隔 token。
///
/// 返回 `None` 表示无法解析出程序名（空串等），调用方按「非浏览器」处理——
/// 偏保守：漏判只少清一个残留，误判会杀用户进程。
fn program_basename(lower_cmd: &str) -> Option<&str> {
    let cmd = lower_cmd.trim_start();
    let program = match cmd.strip_prefix('"') {
        // 引号包裹：到下一个引号为止（未闭合时退化为整串，交给基名匹配判定）
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => cmd.split_whitespace().next().unwrap_or(""),
    };
    if program.is_empty() {
        return None;
    }
    // 剥离目录（Windows 反斜杠与 unix 斜杠都要处理）
    Some(program.rsplit(['/', '\\']).next().unwrap_or(program))
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

    /// 非浏览器进程的命令行**恰好含**关键词时不得命中（P2-14 的核心回归）
    ///
    /// 旧实现（全命令行子串匹配：含 "chrom" 且含 "--headless"）会把这些判为候选，
    /// 而 Windows 分支当时无 kill 前复核 → 可能误杀用户进程。实测
    /// `cmd.exe /c echo chromium --headless` 在旧实现下确实命中。
    #[test]
    fn test_is_chromium_rejects_keywords_in_arguments() {
        // 程序名不是浏览器，仅参数/正文含关键词
        assert!(!is_chromium("cmd.exe /c echo chromium --headless"));
        assert!(!is_chromium(
            "powershell -Command \"Get-Process chromium --headless\""
        ));
        assert!(!is_chromium("git log --grep=chromium --headless"));
        assert!(!is_chromium(
            "/bin/sh -c 'grep chrom --remote-debugging-port files'"
        ));
        // 路径含 chrom 但程序本身不是浏览器
        assert!(!is_chromium(
            "/home/u/chromium-notes/reader --headless --print"
        ));
        assert!(!is_chromium(
            "C:\\tools\\chromedriver\\chromedriver.exe --headless"
        ));
        // 审查本模块的搜索命令（此前实测会把自己列为候选）
        assert!(!is_chromium(
            "rg is_chromium --headless_shell chromium src/bridge/orphan.rs"
        ));
        // 但真正的浏览器 + 特征仍必须命中
        assert!(is_chromium("/opt/chromium-browser/chromium --headless"));
        assert!(is_chromium("C:\\pw\\chrome.exe --headless --disable-gpu"));
    }

    /// CSV 字段反转义：真实命令行带空格路径被双写引号包裹时必须还原
    ///
    /// `ConvertTo-Csv` 把字段内 `"` 转义为 `""` 并总是加外层引号，故
    /// `"C:\Program Files\…\chrome.exe" --headless` 在 CSV 里是
    /// `"""C:\Program Files\…\chrome.exe"" --headless"`。不还原会让按基名匹配的
    /// is_chromium 全部落空（实测漏掉真实孤儿 chrome.exe）。
    #[cfg(windows)]
    #[test]
    fn test_unescape_csv_field() {
        // PowerShell ConvertTo-Csv 对含引号路径的实际形态
        assert_eq!(
            unescape_csv_field(
                "\"\"\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\"\" --headless=new\""
            ),
            "\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\" --headless=new"
        );
        // 普通无引号字段
        assert_eq!(unescape_csv_field("\"123\""), "123");
        assert_eq!(unescape_csv_field("123"), "123");
        assert_eq!(unescape_csv_field(""), "");
    }

    /// 含空格路径的浏览器（Windows 最常见形态）必须仍被判为候选
    ///
    /// 这是 P2-14 收紧基名匹配后新暴露的回归面：旧的全命令行子串匹配
    /// （contains）对引号免疫，改为基名匹配后若不适配引号写法，安装在
    /// `C:\Program Files\…` 下的浏览器会**全部漏判**，孤儿清理静默失效。
    #[test]
    fn test_is_chromium_handles_quoted_path_with_spaces() {
        // 实测的真实命令行（带引号的含空格路径）
        assert!(is_chromium(
            "\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\" --headless=new --disable-gpu"
        ));
        assert!(is_chromium(
            "\"C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe\" --remote-debugging-port=9222"
        ));
        // 反转义后的形态同样要命中
        assert!(is_chromium(
            "\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\" --headless=new"
        ));
        // Playwright 的 headless_shell（通常无空格）
        assert!(is_chromium(
            "/root/.cache/ms-playwright/chromium_headless_shell-1200/chrome-linux/headless_shell --disable-gpu"
        ));
        // 非浏览器即便带引号含空格也不得命中
        assert!(!is_chromium(
            "\"C:\\Program Files\\NotABrowser\\reader.exe\" --headless"
        ));
        assert!(!is_chromium("\"C:\\tools\\chromedriver.exe\" --headless"));
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

    /// 映像路径基名判定：只有浏览器可执行文件才通过（kill 前复核的第一道）
    #[cfg(windows)]
    #[test]
    fn test_is_browser_executable_matrix() {
        assert!(is_browser_executable(
            "C:\\pw\\chromium-1228\\chrome-win\\chrome.exe"
        ));
        assert!(is_browser_executable("C:\\pw\\headless_shell.exe"));
        assert!(is_browser_executable("/opt/chromium"));
        assert!(is_browser_executable(
            "C:\\Program Files (x86)\\Microsoft\\Edge\\msedge.exe"
        ));
        // 非浏览器：即便名字里含 chrom
        assert!(!is_browser_executable("C:\\tools\\chromedriver.exe"));
        assert!(!is_browser_executable("C:\\Windows\\System32\\cmd.exe"));
        assert!(!is_browser_executable("C:\\Users\\u\\notepad.exe"));
    }

    /// `parent_pid_of` 对真实进程读到的祖先后关系正确
    ///
    /// 该函数是 kill 前复核的第二/三道（父 PID 未变 + 父进程已不存在），
    /// 必须在真实进程表上验证——静态推理无法确认 ToolHelp 快照的字段语义
    /// （`th32ParentProcessID` 是否与 `Get-CimInstance` 的 `ParentProcessId` 一致）。
    #[cfg(windows)]
    #[test]
    fn test_parent_pid_of_real_process() {
        // 自身进程必有父进程
        let me = std::process::id();
        let ppid = parent_pid_of(me).expect("应能读到本进程的父 PID");
        assert!(ppid > 0, "父 PID 应为有效值，实际 {ppid}");

        // 不存在的 PID 返回 None（调用方据此保守放弃 kill）
        // 取一个几乎不可能存在的 PID：0 是 System Idle，u32::MAX 必然无效
        assert!(
            parent_pid_of(u32::MAX).is_none(),
            "不存在的 PID 应返回 None"
        );

        // 与 Get-CimInstance 的口径交叉核对：父进程确实存在且其 PID 等于读到的值
        //（父进程可能在测试期间退出，故允许「父进程不存在」这一种失败）
        if let Some(grandparent) = parent_pid_of(ppid) {
            assert_ne!(grandparent, me, "不能把自己当成父进程");
        }
    }

    /// `still_orphan_chromium` 对非浏览器 PID 必须拒绝（防 PID 复用误杀）
    #[cfg(windows)]
    #[test]
    fn test_still_orphan_chromium_rejects_non_browser_pid() {
        // 本测试进程显然不是浏览器可执行文件 → 复核应立即拒绝，
        // 无论其父进程状态如何（这正是"PID 复用"防护的核心断言）
        let me = std::process::id();
        let ppid = parent_pid_of(me).unwrap_or(0);
        assert!(
            !still_orphan_chromium(me, ppid),
            "非浏览器进程不得通过 kill 前复核"
        );
    }
}
