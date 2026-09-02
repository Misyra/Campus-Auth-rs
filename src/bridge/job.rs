//! Windows Job Object 进程树治理（P3 架构演进：内核级孤儿回收）
//!
//! 为 Python Worker 创建 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的 Job Object，
//! spawn 后立即将其加入 Job。此后：
//! - Worker 内 Playwright 拉起的 chromium 进程树自动继承 Job 成员身份；
//! - Worker 被 kill / Worker 正常回收 / 本进程自身被强杀（句柄随进程终止被
//!   内核关闭）时，内核自动终止 Job 内全部进程，chromium 树不再残留。
//!
//! 这是进程清理三层防线（todo 7.3 / 审计架构项）的内核级第一层：
//! Job Object 内核回收 → `kill_on_drop` 直杀直接子进程 → orphan.rs 事后扫描兜底。
//!
//! 分配竞态说明：spawn 返回后立即 `AssignProcessToJobObject`。Worker 是 Python
//! 解释器，从进程启动到首个 Playwright 子进程需完成模块导入与初始化（数百 ms），
//! 而句柄分配仅数 µs，窗口可忽略；即便极端情况下 chromium 抢先拉起而未入 Job，
//! orphan.rs 兜底仍覆盖。

/// Job Object 句柄守卫：Drop 时关闭句柄，触发 KILL_ON_JOB_CLOSE 内核回收
#[derive(Debug)]
pub struct JobHandle(usize);

/// 创建 KILL_ON_JOB_CLOSE 的 Job Object
fn create_kill_on_close_job() -> std::io::Result<JobHandle> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    // SAFETY: CreateJobObjectW 仅按值复制入参（此处均为 null），返回裸句柄。
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job == INVALID_HANDLE_VALUE || job.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // 全零初始化后仅置 LimitFlags：JOBOBJECT_EXTENDED_LIMIT_INFORMATION 的
    // 其余字段为零值时表示「不施加对应限制」（整数/句柄字段的 POD 结构）
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: job 为本函数刚创建的有效句柄；info 指针与长度匹配。
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: job 有效且此后不再使用
        unsafe { CloseHandle(job) };
        return Err(err);
    }
    // windows-sys HANDLE 无法跨 await/存入需 Send 的结构（*mut c_void 非 Send），
    // 转成 usize 保存，使用时转回。句柄有效性由守卫生命周期保证。
    Ok(JobHandle(job as usize))
}

impl JobHandle {
    /// 将子进程加入本 Job
    ///
    /// 需在子进程 spawn 后尽早调用（见模块注释的竞态说明）。
    fn assign(&self, child: &tokio::process::Child) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let Some(raw) = child.raw_handle() else {
            return Err(std::io::Error::other("子进程已退出，无可用句柄"));
        };
        // SAFETY: self.0 是由 create_kill_on_close_job 创建、未被关闭的 Job 句柄；
        // raw 是存活子进程的句柄，两者均仅在本次调用中按值使用。
        let ok = unsafe { AssignProcessToJobObject(self.0 as _, raw) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// 立即关闭 Job 句柄：内核终止 Job 内全部进程（含 Worker 与 chromium 树）
    ///
    /// 强杀路径使用：比等待各清理分支逐一执行更快，且覆盖「python 被杀但
    /// chromium 残留」的场景。幂等（Drop 语义）。
    pub fn terminate_tree(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        if self.0 != 0 {
            // SAFETY: self.0 是有效 Job 句柄，关闭后置 0 防止 Double Close
            unsafe { CloseHandle(self.0 as _) };
            self.0 = 0;
        }
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        self.terminate_tree();
    }
}

/// 创建 Job 并把刚 spawn 的子进程加入；任一步失败记录告警并返回 None
///
/// 失败是可运行的（退回 kill_on_drop + orphan.rs 应用层清理），但需留下痕迹：
/// 常见失败原因是本进程自身处于禁用 breakaway 的外部 Job 中（如某些服务管理器）。
pub fn try_assign_job(child: &tokio::process::Child) -> Option<JobHandle> {
    let job = match create_kill_on_close_job() {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("创建 Job Object 失败，Worker 进程树退回应用层清理: {e}");
            return None;
        }
    };
    match job.assign(child) {
        Ok(()) => {
            tracing::info!("Worker 已加入 KILL_ON_JOB_CLOSE Job Object（内核级进程树回收）");
            Some(job)
        }
        Err(e) => {
            tracing::warn!("Worker 加入 Job Object 失败，退回应用层清理: {e}");
            None // job drop 时无成员，无副作用
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Job 句柄关闭后，内核应终止 Job 内进程（KILL_ON_JOB_CLOSE 语义验证）
    #[tokio::test]
    async fn test_job_close_terminates_child() {
        use std::process::Stdio;
        use tokio::process::Command;

        let mut child = Command::new("ping.exe")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ping 失败（Windows 测试环境应有 ping.exe）");

        let Some(mut job) = try_assign_job(&child) else {
            // 本进程处于禁用 Job 分配的外部环境中：跳过而非误报失败
            let _ = child.start_kill();
            eprintln!("跳过：测试进程无法分配 Job Object（宿主环境限制）");
            return;
        };

        // 未关闭句柄前进程应存活（1s 采样，容忍调度延迟）
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            child.try_wait().unwrap().is_none(),
            "Job 存续期间子进程不应退出"
        );

        // 关闭句柄 → 内核终止整棵树
        job.terminate_tree();
        let exited = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        assert!(exited.is_ok(), "Job 句柄关闭后 5s 内子进程应被内核终止");
    }
}
