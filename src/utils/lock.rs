//! 实例互斥：文件锁
//!
//! 通过 `std::fs::File` 原生的 advisory lock 在 `config/.lock` 文件上获取排他锁，保证单实例运行。
//! 进程崩溃时 OS 自动释放文件锁（内核在进程退出时回收），无需 PID 文件清理逻辑。
//! （Rust 1.96 起 `File` 已内置 `try_lock`/`lock`/`unlock`；MSRV 1.85 期间通过 fs4 提供。）
//!
//! 实例信息（PID + 端口）写入 `config/.instance`，与锁文件分离，
//! 避免 Windows mandatory lock 导致外部进程无法读取。

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

// MSRV 1.85 兼容：fs4::FileExt 为低版本 Rust 提供 try_lock；
// Rust 1.96+ 内置方法优先级更高，不会冲突。
#[allow(unused_imports)]
use fs4::FileExt;

/// 实例锁 RAII 句柄
///
/// 持有期间独占 `config/.lock` 排他锁；drop 时文件关闭、锁自动释放、清理实例信息文件。
pub struct InstanceLock {
    /// 锁文件句柄（drop 时关闭并释放锁）
    _file: std::fs::File,
    /// 锁文件路径（保留以便错误提示）
    _lock_path: PathBuf,
    /// 实例信息文件路径（PID + 端口，供外部 `query_instance` 查询）
    info_path: PathBuf,
}

/// 运行实例信息
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    /// 进程 PID
    pub pid: u32,
    /// 监听端口
    pub port: u16,
    /// 进程是否存活
    pub running: bool,
    /// 运行时长（从实例信息文件修改时间推算）
    pub uptime: Option<Duration>,
}

impl InstanceLock {
    /// 尝试获取实例锁
    ///
    /// 成功返回 RAII 句柄；已被其他实例占用或无法创建锁文件时返回错误。
    pub fn try_acquire(base_path: &Path) -> anyhow::Result<Self> {
        let config_dir = base_path.join("config");
        std::fs::create_dir_all(&config_dir)?;
        let lock_path = config_dir.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        // MSRV 1.85 兼容：此处 try_lock() 由 fs4::FileExt trait 提供，
        // 而非 std::fs::File 内置方法（后者要求 Rust 1.96+）。
        #[allow(clippy::incompatible_msrv)]
        file.try_lock()
            .map_err(|e| anyhow::anyhow!("无法获取实例锁（可能已有实例在运行）: {e}"))?;
        // 清理可能残留的旧实例信息文件
        let info_path = config_dir.join(".instance");
        let _ = std::fs::remove_file(&info_path);
        Ok(Self {
            _file: file,
            _lock_path: lock_path,
            info_path,
        })
    }

    /// 向实例信息文件记录当前实例的 PID 和监听端口
    ///
    /// 在绑定端口后调用，供外部 `query_instance` 查询。
    /// 信息写入独立的 `.instance` 文件（而非锁文件），避免 Windows mandatory lock 阻止外部读取。
    pub fn record_port(&self, port: u16) -> anyhow::Result<()> {
        let pid = std::process::id();
        let data = format!("{pid}\n{port}\n");
        std::fs::write(&self.info_path, data)?;
        Ok(())
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // 清理实例信息文件
        let _ = std::fs::remove_file(&self.info_path);
    }
}

/// 查询当前运行实例的信息
///
/// 读取 `config/.instance` 中的 PID 和端口，检查进程是否存活。
/// 未找到信息文件、格式异常或进程已退出时返回 `None`。
pub fn query_instance(base_path: &Path) -> Option<InstanceInfo> {
    let info_path = base_path.join("config").join(".instance");
    let content = std::fs::read_to_string(&info_path).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.trim().parse().ok()?;
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let running = is_process_alive(pid);
    // 用文件修改时间近似实例启动时间
    let uptime = if running {
        std::fs::metadata(&info_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
    } else {
        None
    };
    Some(InstanceInfo {
        pid,
        port,
        running,
        uptime,
    })
}

/// 优雅停止运行实例
///
/// 通过 HTTP POST `/api/system/shutdown` 通知实例关闭，然后轮询等待进程退出（最多 10 秒）。
pub async fn stop_instance(base_path: &Path) -> anyhow::Result<()> {
    let info =
        query_instance(base_path).ok_or_else(|| anyhow::anyhow!("未找到运行中的实例信息"))?;

    if !info.running {
        // 进程已退出，清理残留信息文件即可
        let _ = std::fs::remove_file(base_path.join("config").join(".instance"));
        return Ok(());
    }

    if info.port == 0 {
        anyhow::bail!("实例运行于轻量模式且 Web 控制台尚未启动，无法远程停止；请通过系统托盘退出");
    }

    // 发送关机请求（忽略 HTTP 错误，进程可能已开始关闭）。
    // 本地 API 已启用 token 鉴权，必须携带 config/.auth_token 中的 token
    let url = format!("http://127.0.0.1:{}/api/system/shutdown", info.port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let mut req = client.post(&url);
    if let Some(token) = crate::web::auth::read_token_file(base_path) {
        req = req.header("X-Auth-Token", token);
    }
    let _ = req.send().await;

    // 轮询等待进程退出；上限须覆盖优雅关闭的最坏预算
    // （bridge 3s + scheduler 5s + engine 8s = 16s），取 20s 留余量。
    // unix 附加"监听端口已关闭"判据：若父进程未回收子进程，已退出的实例
    // 会以僵尸形态存在，kill(pid,0) 仍返回成功——此时端口关闭才是可靠的
    // 停止信号（shutdown POST 已送达，Axum 关闭即代表实例已停止）。
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !is_process_alive(info.pid) {
            return Ok(());
        }
        if std::net::TcpStream::connect(("127.0.0.1", info.port)).is_err() {
            return Ok(());
        }
    }

    anyhow::bail!("等待进程退出超时（20 秒）")
}

/// 强制杀死指定 PID 的进程
///
/// - Windows: `TerminateProcess`
/// - unix: `kill(pid, SIGKILL)`（SIGKILL 不可被捕获或忽略，进程必然终止；
///   进程已退出时返回 ESRCH，静默忽略）
pub fn force_kill(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // SIGKILL 直接由内核终止目标；失败（进程已退出 / 无权限）不影响
        // 调用方随后的抢锁重试逻辑
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// 检查指定 PID 的进程是否存活
///
/// **已知局限**：PID 复用（PID reuse）可能导致误判——若原进程已退出且其 PID 被新进程占用，
/// 本函数仍会返回 `true`。此为 OS 级限制，所有基于 PID 的探测均无法完全避免。
/// 本模块以文件锁（`InstanceLock`）作为主要互斥机制，PID 检查仅用于辅助状态查询，
/// 因此 PID 复用场景的实际影响有限。
#[cfg(target_os = "windows")]
pub fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        // 仅凭 OpenProcess 成功不足以判定存活：父进程持有未关闭的子进程句柄时
        // （如更新助手、测试框架），已退出的进程对象仍可被打开。必须再查退出码——
        // 进程终止后退出码固化为实际值，不再是 STILL_ACTIVE(259)。
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        if ok == 0 {
            return false;
        }
        exit_code == 259 // STILL_ACTIVE
    }
}

/// 检查指定 PID 的进程是否存活（非 Windows：`kill(pid, 0)` 探测）
///
/// `kill(pid, 0)` 不发送信号，仅检查进程是否存在且有权访问。
/// 返回 0 表示进程存活；返回 -1 且 errno 为 EPERM 表示进程存在但无权限
/// （如 root 下查询其他用户进程），同样视为存活。
///
/// **已知局限**：PID 复用（PID reuse）可能导致误判。本模块以文件锁为主要互斥机制，
/// PID 检查仅用于辅助状态查询，实际影响有限。
#[cfg(not(target_os = "windows"))]
pub fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0): 信号 0 仅做存在性检查，不实际发送信号
    // 返回 0 = 进程存在且有权限；EPERM = 进程存在但无权限，也视为"存活"
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
