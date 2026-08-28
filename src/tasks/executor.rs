//! 任务异步执行器：TaskExecutor
//!
//! 统一入口 [`TaskExecutor::execute`] 按 [`TaskKind`] 分派：
//! - `Browser` → 经 [`BridgeSupervisor`] 执行浏览器任务，执行前后向 [`StatusManager`] 上报
//!   Worker 忙/空闲状态，并确保 Python 环境能力就绪（[`EnvironmentManager`]）；
//! - `Script` / `Shell` → 用 `tokio::process::Command` 执行，超时/取消通过 `tokio::time::timeout`
//!   与 `kill_on_drop`/Windows Job Object 实现，标准输出/错误持续排空并截断到
//!   `OUTPUT_TRUNCATE_LEN`，避免管道阻塞和内存无界增长。
//!
//! 脚本/Shell 执行按任务 ID 经各自的 `tokio::sync::Mutex` 串行化：
//! 同一任务串行执行，不同任务互不阻塞（避免一个长脚本阻塞所有脚本任务）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::bridge::{BridgeSupervisor, Outcome, StructuredResult};
use crate::config::ConfigService;
use crate::environment::EnvironmentManager;
use crate::status::{PartialSnapshot, StatusManager, WorkerStatus};
use crate::tasks::TaskError;
use crate::tasks::models::*;

/// 任务执行统一结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskResult {
    /// 是否成功（exit_code == 0 或 Bridge 返回 success）
    pub success: bool,
    /// 合并后的输出（stdout + stderr，截断到 `OUTPUT_TRUNCATE_LEN`）
    pub output: String,
    /// 进程退出码（Bridge 任务为 0/1）
    pub exit_code: i32,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// 失败时的错误消息
    pub error: Option<String>,
}

/// 脚本/Shell 任务异步执行器
pub struct TaskExecutor {
    /// 脚本/Shell 文件根目录（= `<base_path>/scripts` 的父级 `tasks/`）
    tasks_dir: PathBuf,
    /// `tasks/scripts/` 目录（解析相对 script_path 用）
    scripts_dir: PathBuf,
    /// 状态管理器（上报 Worker 忙碌状态）
    status: Arc<StatusManager>,
    /// Bridge 监督器（执行浏览器任务）
    bridge: Arc<BridgeSupervisor>,
    /// 环境能力管理器（浏览器任务前确保就绪）
    env: Arc<EnvironmentManager>,
    /// 配置服务（读取浏览器启动设置，随浏览器任务一并下发 Worker）
    config: Arc<ConfigService>,
    /// 脚本/Shell 执行的按任务 ID 锁注册表：同任务串行、不同任务并行
    ///
    /// registry 条目不清理：任务数量有限（数十量级），每个条目仅一个空 Mutex，
    /// 常驻内存开销可忽略，换取实现简单与并发安全。
    lock_registry: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// 仅 `.py` 且未指定自定义解释器时使用项目内 Python。
fn uses_project_python(ext: &str, binary_path: Option<&str>) -> bool {
    ext == "py" && binary_path.is_none_or(|path| path.trim().is_empty())
}

impl TaskExecutor {
    /// 构造执行器。`base_path` 即任务根目录（`tasks/`），脚本位于其 `scripts/` 子目录
    pub fn new(
        base_path: &Path,
        status: Arc<StatusManager>,
        bridge: Arc<BridgeSupervisor>,
        env: Arc<EnvironmentManager>,
        config: Arc<ConfigService>,
    ) -> Arc<Self> {
        let tasks_dir = base_path.join("tasks");
        let scripts_dir = tasks_dir.join("scripts");
        Arc::new(Self {
            tasks_dir,
            scripts_dir,
            status,
            bridge,
            env,
            config,
            lock_registry: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// 统一执行入口：按任务类型分派
    pub async fn execute(&self, task: &TaskKind) -> Result<TaskResult, TaskError> {
        match task {
            TaskKind::Browser(cfg) => self.execute_browser(cfg).await,
            TaskKind::Script(cfg) => self.execute_script(cfg).await,
            TaskKind::Shell(cfg) => self.execute_shell(cfg).await,
        }
    }

    /// 统一执行入口（带超时覆写）：按任务类型分派，并用 `timeout_secs` 覆盖任务配置
    /// 中的超时字段（供调度器等需要按定时任务定义统一覆盖超时的调用方使用）。
    ///
    /// 超时单位差异在此集中消化：浏览器任务的 `TaskConfig::timeout` 单位是**毫秒**，
    /// 脚本/Shell 任务的单位是**秒**（且执行时钳制到 `[1, 3600]`）。调用方统一传秒，
    /// 由本方法按类型换算，避免各调用点自行重复处理单位与钳制。
    pub async fn execute_with_timeout_override(
        &self,
        task: &TaskKind,
        timeout_secs: u64,
    ) -> Result<TaskResult, TaskError> {
        let timeout_secs = clamp_timeout(timeout_secs);
        match task {
            TaskKind::Browser(cfg) => {
                let mut cfg = cfg.clone();
                // 浏览器超时单位为毫秒：秒 → 毫秒（饱和乘法防溢出）
                cfg.timeout = timeout_secs.saturating_mul(1000);
                self.execute_browser(&cfg).await
            }
            TaskKind::Script(cfg) => {
                let mut cfg = cfg.clone();
                cfg.timeout = timeout_secs;
                self.execute_script(&cfg).await
            }
            TaskKind::Shell(cfg) => {
                let mut cfg = cfg.clone();
                cfg.timeout = timeout_secs;
                self.execute_shell(&cfg).await
            }
        }
    }

    /// 执行浏览器任务（通用语义：打卡/签到等日常自动化，经 Bridge 的 `execute_browser_task`）
    ///
    /// 不注入账号密码、不做登录后网络验证；步骤执行完成即成功。
    /// 带凭据的登录语义请走 [`crate::login::LoginOrchestrator::submit`]。
    pub async fn execute_browser(&self, cfg: &TaskConfig) -> Result<TaskResult, TaskError> {
        // 执行前标记 Worker 忙
        self.status.merge(PartialSnapshot::Worker {
            state: WorkerStatus::Busy,
        });

        // 确保 Python 环境能力就绪
        let ensure = if self.env.capability_ready() {
            Ok(())
        } else {
            self.env
                .ensure_capability()
                .await
                .map_err(|e| TaskError::Environment(e.to_string()))
        };

        let result = async {
            ensure?;
            // 序列化浏览器任务配置并包装为 `task_config`（Worker 约定的步骤载体键），
            // 同时下发现运行时浏览器设置，与登录路径保持一致。
            let task_val = serde_json::to_value(cfg).map_err(TaskError::JsonError)?;
            let browser_settings = serde_json::to_value(&self.config.runtime().load_full().browser)
                .unwrap_or(serde_json::Value::Null);
            let params = serde_json::json!({
                "task_config": task_val,
                "browser_settings": browser_settings,
            });
            self.bridge
                .execute_with_timeout(
                    "execute_browser_task",
                    params,
                    Duration::from_millis(cfg.timeout.max(1)),
                )
                .await
                .map_err(TaskError::Bridge)
        }
        .await;

        // 执行后恢复 Worker 实际状态
        let restored = self.bridge.worker_status();
        self.status
            .merge(PartialSnapshot::Worker { state: restored });

        let resp = result?;

        let success = resp.result.success;
        let data = resp.result.data.clone();
        let structured: StructuredResult =
            serde_json::from_value(data.clone()).unwrap_or_else(|_| StructuredResult {
                outcome: if success {
                    Outcome::Success
                } else {
                    Outcome::UnknownError
                },
                message: String::new(),
                data,
                screenshot_url: None,
                duration_ms: 0,
            });
        let mut out = structured.message.clone();
        if let Some(e) = &resp.result.error {
            out = format!("{out}\n{e}");
        }
        Ok(TaskResult {
            success,
            output: out,
            exit_code: if success { 0 } else { 1 },
            duration_ms: structured.duration_ms,
            error: resp.result.error.clone(),
        })
    }

    /// 执行脚本任务
    pub async fn execute_script(&self, cfg: &ScriptTaskConfig) -> Result<TaskResult, TaskError> {
        // 同任务串行、不同任务并行：按任务 ID 取各自的执行锁
        let lock = self.task_exec_lock(&cfg.common.task_id);
        let _guard = lock.lock().await;

        // 解析脚本来源（内联 content → 临时文件，或 script_path）
        let (script_file, _tmp) = self.resolve_script_source(cfg).await?;

        // 校验扩展名
        let ext = script_file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !is_supported_ext(&ext) {
            return Err(TaskError::UnsupportedExtension(ext));
        }

        // 默认项目 Python 来自 python_worker/.venv；发布包不携带该目录，首次使用时
        // 只准备 uv + venv，不为了脚本任务额外安装 Playwright Chromium。
        if uses_project_python(&ext, cfg.binary_path.as_deref()) {
            self.env
                .ensure_python_runtime()
                .await
                .map_err(|e| TaskError::Environment(e.to_string()))?;
        }

        // 构建命令
        let python_default = self.env.python_path().to_string_lossy().to_string();
        let (program, args) = build_script_command(cfg, &script_file, &ext, &python_default);
        let work_dir = resolve_work_dir(cfg, &script_file, &self.scripts_dir);
        let envs = build_minimal_env();

        self.run_command(program, args, &work_dir, envs, clamp_timeout(cfg.timeout))
            .await
    }

    /// 执行 Shell 任务
    pub async fn execute_shell(&self, cfg: &ShellTaskConfig) -> Result<TaskResult, TaskError> {
        if cfg.command.trim().is_empty() {
            return Err(TaskError::CommandEmpty);
        }
        // 同任务串行、不同任务并行：按任务 ID 取各自的执行锁
        let lock = self.task_exec_lock(&cfg.common.task_id);
        let _guard = lock.lock().await;

        let (program, flag) = resolve_shell(cfg);
        let args = vec![flag.to_string(), cfg.command.clone()];
        let work_dir = self.tasks_dir.clone();
        let envs = build_minimal_env();

        self.run_command(program, args, &work_dir, envs, clamp_timeout(cfg.timeout))
            .await
    }

    // ---------------- 私有辅助 ----------------

    /// 获取指定任务的执行锁（同任务串行、不同任务并行）
    ///
    /// 注册表本身用 `std::sync::Mutex` 保护，仅做 entry 查找/插入后立即释放，
    /// 真正的串行化由返回的 per-task `tokio::sync::Mutex` 保证。
    /// task_id 为空时（调试等未落盘场景）所有匿名任务共享同一个空键，彼此串行。
    fn task_exec_lock(&self, task_id: &str) -> Arc<Mutex<()>> {
        let mut registry = self
            .lock_registry
            .lock()
            .unwrap_or_else(crate::utils::recover_lock);
        registry
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 解析脚本来源：content 写入临时文件（后缀按 binary_path 推断），否则用 script_path
    async fn resolve_script_source(
        &self,
        cfg: &ScriptTaskConfig,
    ) -> Result<(PathBuf, Option<tempfile::NamedTempFile>), TaskError> {
        if let Some(content) = &cfg.content {
            if content.len() > MAX_SCRIPT_CONTENT_SIZE {
                return Err(TaskError::ScriptNotFound(format!(
                    "脚本内容超过大小上限 {}",
                    MAX_SCRIPT_CONTENT_SIZE
                )));
            }
            let ext = cfg
                .binary_path
                .as_deref()
                .map(binary_to_ext)
                .unwrap_or("py");
            let mut tmp = tempfile::Builder::new()
                .suffix(&format!(".{ext}"))
                .tempfile()
                .map_err(TaskError::IoError)?;
            use std::io::Write;
            tmp.write_all(content.as_bytes())
                .map_err(TaskError::IoError)?;
            let path = tmp.path().to_path_buf();
            Ok((path, Some(tmp)))
        } else if let Some(sp) = &cfg.script_path {
            let path = if Path::new(sp).is_absolute() {
                PathBuf::from(sp)
            } else {
                self.scripts_dir.join(sp)
            };
            if !path.exists() {
                return Err(TaskError::ScriptNotFound(sp.clone()));
            }
            Ok((path, None))
        } else {
            Err(TaskError::ScriptNotFound(
                "缺少 content 与 script_path".to_string(),
            ))
        }
    }

    /// 执行子进程，带超时与输出捕获
    async fn run_command(
        &self,
        program: String,
        args: Vec<String>,
        work_dir: &Path,
        envs: Vec<(String, String)>,
        timeout: u64,
    ) -> Result<TaskResult, TaskError> {
        let start = Instant::now();
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            // 用户任务默认不应继承主进程中的 token、代理密码或调试变量。
            .env_clear()
            .envs(envs)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(TaskError::IoError)?;
        // Windows 下用 KILL_ON_JOB_CLOSE 约束整棵任务进程树。任务超时、调度器
        // 关闭或 future 被取消时，守卫析构都会由内核回收脚本拉起的后代进程。
        #[cfg(windows)]
        let _job = crate::bridge::job::try_assign_job(&child);
        // 记录 PID：超时后需 taskkill /T 递归强杀整个进程树（kill_on_drop 只杀直接子进程）
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TaskError::IoError(std::io::Error::other("无法捕获子进程 stdout")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TaskError::IoError(std::io::Error::other("无法捕获子进程 stderr")))?;

        let timeout_dur = Duration::from_secs(timeout);
        let waited = tokio::time::timeout(
            timeout_dur,
            wait_with_bounded_output(&mut child, stdout, stderr),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match waited {
            Ok(Ok((status, stdout_bytes, stderr_bytes))) => {
                let stdout = String::from_utf8_lossy(&stdout_bytes);
                let stderr = String::from_utf8_lossy(&stderr_bytes);
                let success = status.success();
                let exit_code = status.code().unwrap_or(-1);
                let truncated_stdout = truncate(&stdout, OUTPUT_TRUNCATE_LEN);
                let truncated_stderr = truncate(&stderr, OUTPUT_TRUNCATE_LEN);
                let output = if truncated_stderr.is_empty() {
                    truncated_stdout
                } else {
                    format!("{truncated_stdout}\n{truncated_stderr}")
                };
                let error = if success {
                    None
                } else {
                    Some(truncated_stderr)
                };
                Ok(TaskResult {
                    success,
                    output,
                    exit_code,
                    duration_ms,
                    error,
                })
            }
            Ok(Err(e)) => Err(TaskError::IoError(e)),
            Err(_) => {
                // Windows 上 cmd.exe 启动的脚本子树可能在直接 kill 后仍存活为孤儿，
                // 先递归终止进程树，再回收直接子进程句柄。
                #[cfg(windows)]
                if let Some(pid) = pid {
                    let _ = tokio::task::spawn_blocking(move || {
                        use std::os::windows::process::CommandExt;
                        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                        std::process::Command::new("taskkill")
                            .args(["/T", "/F", "/PID", &pid.to_string()])
                            .creation_flags(CREATE_NO_WINDOW)
                            .status()
                    })
                    .await;
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(TaskError::ExecutionTimeout(timeout))
            }
        }
    }
}

/// 根据扩展名与 binary_path 构建命令（program, args）。
///
/// 自由函数而非方法：`py` 扩展名在未指定 `binary_path` 时需要回退到环境管理器
/// 检测的 Python 路径，通过 `python_default` 参数注入，使本函数可脱离完整执行器
/// 做单元测试（A7）。
fn build_script_command(
    cfg: &ScriptTaskConfig,
    script_file: &Path,
    ext: &str,
    python_default: &str,
) -> (String, Vec<String>) {
    let script = script_file.to_string_lossy().to_string();
    match ext {
        "exe" | "com" => {
            // 直接启动可执行文件
            (script, cfg.args.clone())
        }
        "py" => {
            let python = cfg
                .binary_path
                .clone()
                .unwrap_or_else(|| python_default.to_string());
            let mut args = vec![script];
            args.extend(cfg.args.clone());
            (python, args)
        }
        "bat" | "cmd" => {
            let cmd = cfg
                .binary_path
                .clone()
                .unwrap_or_else(|| "cmd.exe".to_string());
            let mut args = vec!["/c".to_string(), script];
            args.extend(cfg.args.clone());
            (cmd, args)
        }
        "sh" => {
            let sh = cfg.binary_path.clone().unwrap_or_else(|| "sh".to_string());
            let mut args = vec![script];
            args.extend(cfg.args.clone());
            (sh, args)
        }
        _ => (String::new(), Vec::new()),
    }
}

/// 解析工作目录：config.work_dir 优先，否则脚本所在目录（A7：自由函数便于单测）
fn resolve_work_dir(cfg: &ScriptTaskConfig, script_file: &Path, scripts_dir: &Path) -> PathBuf {
    if let Some(wd) = &cfg.work_dir {
        if Path::new(wd).is_absolute() {
            return PathBuf::from(wd);
        }
        return scripts_dir.join(wd);
    }
    script_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| scripts_dir.to_path_buf())
}

/// 解析 Shell 路径与参数标志（A7：自由函数便于单测）
fn resolve_shell(cfg: &ShellTaskConfig) -> (String, &'static str) {
    let shell = if let Some(s) = &cfg.shell_path {
        s.clone()
    } else {
        default_shell()
    };
    let lower = shell.to_lowercase();
    if lower.contains("powershell") || lower.contains("pwsh") {
        (shell, "-Command")
    } else if lower.contains("cmd") && cfg!(windows) {
        (shell, "/c")
    } else {
        (shell, "-c")
    }
}

/// 同时等待子进程退出并持续排空 stdout/stderr，只保留有限前缀防止内存无界增长。
async fn wait_with_bounded_output(
    child: &mut tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
) -> std::io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    // UTF-8 单字符最多 4 字节，额外保留少量空间覆盖无效编码与截断提示场景。
    const CAPTURE_LIMIT: usize = OUTPUT_TRUNCATE_LEN * 4 + 4096;
    let (status, stdout, stderr) = tokio::join!(
        child.wait(),
        read_bounded(stdout, CAPTURE_LIMIT),
        read_bounded(stderr, CAPTURE_LIMIT)
    );
    Ok((status?, stdout?, stderr?))
}

/// 排空异步读取器并最多保留 `limit` 字节，其余内容丢弃但继续读取以免子进程阻塞。
async fn read_bounded<R>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    Ok(retained)
}

/// 是否受支持的脚本扩展名
///
/// 仅支持 shell / bat / python / exe 四类；其他解释器（如 node、powershell）需在 bat/shell 中自定义调用。
fn is_supported_ext(ext: &str) -> bool {
    matches!(ext, "exe" | "com" | "py" | "bat" | "cmd" | "sh")
}

/// 将 binary_path 推断出的临时文件后缀
fn binary_to_ext(binary: &str) -> &'static str {
    let b = binary.to_lowercase();
    // powershell/pwsh 不在支持范围：映射到 ps1 后由 is_supported_ext 拒绝
    if b.contains("powershell") || b.contains("pwsh") {
        "ps1"
    } else if b.contains("python") || b.contains("py") {
        "py"
    } else if b.ends_with("bash") || b.ends_with("zsh") || b.ends_with("/sh") || b.ends_with("\\sh")
    {
        "sh"
    } else if b.contains("cmd") || b.ends_with(".bat") || b.ends_with(".cmd") {
        "bat"
    } else if b.ends_with(".exe") || b.ends_with(".com") {
        "exe"
    } else {
        "py"
    }
}

/// 检测默认 Shell
fn default_shell() -> String {
    if cfg!(windows) {
        for s in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
            if find_in_path(s) {
                return s.to_string();
            }
        }
        "cmd.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string())
    }
}

/// 在 PATH 中查找可执行文件
fn find_in_path(name: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join(name).exists() {
                return true;
            }
        }
    }
    false
}

/// 钳制脚本超时到 `[MIN_SCRIPT_TIMEOUT, MAX_SCRIPT_TIMEOUT]`
fn clamp_timeout(t: u64) -> u64 {
    t.clamp(MIN_SCRIPT_TIMEOUT, MAX_SCRIPT_TIMEOUT)
}

/// 截断字符串到最大长度（按字符）
///
/// 判定与截断统一按字符数（而非字节）：中文等多字节字符输出按字节判定时
/// 会长至预期 3 倍才触发截断。
fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}...(已截断)")
}

/// 构建最小环境变量（仅保留执行任务所需的 PATH/HOME/TEMP 等）
fn build_minimal_env() -> Vec<(String, String)> {
    let mut envs: Vec<(String, String)> = Vec::new();
    if let Ok(p) = std::env::var("PATH") {
        envs.push(("PATH".to_string(), p));
    }
    if let Ok(h) = std::env::var("HOME") {
        envs.push(("HOME".to_string(), h));
    }
    if let Ok(t) = std::env::var("TEMP") {
        envs.push(("TEMP".to_string(), t));
    }
    if let Ok(t) = std::env::var("TMP") {
        envs.push(("TMP".to_string(), t));
    }
    if cfg!(windows) {
        if let Ok(s) = std::env::var("SystemRoot") {
            envs.push(("SystemRoot".to_string(), s));
        }
        if let Ok(s) = std::env::var("ComSpec") {
            envs.push(("ComSpec".to_string(), s));
        }
    }
    // Windows 关键用户目录变量：缺失会导致 PowerShell 配置、pip/uv 缓存、
    // 脚本中 %USERPROFILE% / %LOCALAPPDATA% 展开失败
    for key in [
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "USERNAME",
        "ProgramData",
        "PATHEXT",
    ] {
        if let Ok(v) = std::env::var(key) {
            envs.push((key.to_string(), v));
        }
    }
    envs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_cfg(args: Vec<String>) -> ScriptTaskConfig {
        ScriptTaskConfig {
            args,
            ..Default::default()
        }
    }

    // ============ build_script_command ============

    #[test]
    fn test_build_script_command_exe_empty_args() {
        // exe 直接启动，空 args 时仅脚本本身
        let cfg = script_cfg(vec![]);
        let (program, args) =
            build_script_command(&cfg, Path::new("C:\\tools\\job.exe"), "exe", "python");
        assert_eq!(program, "C:\\tools\\job.exe");
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_script_command_exe_with_args() {
        // args 原样透传
        let cfg = script_cfg(vec!["--flag".into(), "值".into()]);
        let (program, args) = build_script_command(&cfg, Path::new("/opt/tool"), "com", "python");
        assert_eq!(program, "/opt/tool");
        assert_eq!(args, vec!["--flag", "值"]);
    }

    #[test]
    fn test_build_script_command_py_binary_path_override() {
        // 指定 binary_path 时优先于默认 Python
        let cfg = ScriptTaskConfig {
            binary_path: Some("C:\\py310\\python.exe".into()),
            args: vec!["-u".into()],
            ..Default::default()
        };
        let (program, args) = build_script_command(&cfg, Path::new("a.py"), "py", "default_py");
        assert_eq!(program, "C:\\py310\\python.exe");
        assert_eq!(args, vec!["a.py".to_string(), "-u".to_string()]);
    }

    #[test]
    fn test_build_script_command_py_default_python() {
        // 未指定 binary_path 时回退到环境管理器提供的默认 Python
        let cfg = script_cfg(vec![]);
        let (program, args) = build_script_command(&cfg, Path::new("b.py"), "py", "env_python");
        assert_eq!(program, "env_python");
        assert_eq!(args, vec!["b.py".to_string()]);
    }

    #[test]
    fn test_build_script_command_bat_defaults_to_cmd() {
        // bat/cmd 未指定 binary_path 时用 cmd.exe /c
        let cfg = script_cfg(vec![]);
        let (program, args) = build_script_command(&cfg, Path::new("run.bat"), "bat", "python");
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, vec!["/c".to_string(), "run.bat".to_string()]);
    }

    #[test]
    fn test_build_script_command_sh_defaults() {
        let cfg = script_cfg(vec![]);
        let (program, args) = build_script_command(&cfg, Path::new("s.sh"), "sh", "python");
        assert_eq!(program, "sh");
        assert_eq!(args, vec!["s.sh".to_string()]);
    }

    #[test]
    fn test_build_script_command_unknown_ext_returns_empty() {
        // 未知扩展名返回空命令（调用前 is_supported_ext 已拒绝，此处为兜底行为）
        let cfg = script_cfg(vec![]);
        let (program, args) = build_script_command(&cfg, Path::new("x.ps1"), "ps1", "python");
        assert!(program.is_empty());
        assert!(args.is_empty());
    }

    // ============ resolve_work_dir（绝对/相对路径） ============

    #[test]
    fn test_resolve_work_dir_absolute() {
        // 绝对 work_dir 原样使用
        let cfg = ScriptTaskConfig {
            work_dir: Some("D:\\data".into()),
            ..Default::default()
        };
        let wd = resolve_work_dir(
            &cfg,
            Path::new("C:\\t\\a.py"),
            Path::new("C:\\tasks\\scripts"),
        );
        assert_eq!(wd, PathBuf::from("D:\\data"));
    }

    #[test]
    fn test_resolve_work_dir_relative_joined() {
        // 相对 work_dir 相对 scripts 目录解析
        let cfg = ScriptTaskConfig {
            work_dir: Some("sub".into()),
            ..Default::default()
        };
        let wd = resolve_work_dir(&cfg, Path::new("/tmp/a.py"), Path::new("/tasks/scripts"));
        assert_eq!(wd, PathBuf::from("/tasks/scripts").join("sub"));
    }

    #[test]
    fn test_resolve_work_dir_defaults_to_script_parent() {
        // 未指定 work_dir 时用脚本所在目录
        let cfg = ScriptTaskConfig::default();
        let wd = resolve_work_dir(
            &cfg,
            Path::new("/tasks/scripts/deep/a.py"),
            Path::new("/tasks/scripts"),
        );
        assert_eq!(wd, PathBuf::from("/tasks/scripts/deep"));
    }

    // ============ resolve_shell ============

    #[test]
    fn test_resolve_shell_powershell_flag() {
        let cfg = ShellTaskConfig {
            shell_path: Some(
                "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe".into(),
            ),
            ..Default::default()
        };
        let (shell, flag) = resolve_shell(&cfg);
        assert!(shell.contains("powershell"));
        assert_eq!(flag, "-Command");
    }

    #[test]
    fn test_resolve_shell_pwsh_flag() {
        let cfg = ShellTaskConfig {
            shell_path: Some("pwsh.exe".into()),
            ..Default::default()
        };
        let (_, flag) = resolve_shell(&cfg);
        assert_eq!(flag, "-Command");
    }

    #[test]
    fn test_resolve_shell_cmd_flag_platform_dependent() {
        // cmd.exe 的参数标志平台相关：Windows 用 /c，其余平台按通用 -c 处理
        let cfg = ShellTaskConfig {
            shell_path: Some("cmd.exe".into()),
            ..Default::default()
        };
        let (_, flag) = resolve_shell(&cfg);
        assert_eq!(flag, if cfg!(windows) { "/c" } else { "-c" });
    }

    #[test]
    fn test_resolve_shell_custom_path_generic_flag() {
        // 自定义 shell（如 bash）用通用 -c
        let cfg = ShellTaskConfig {
            shell_path: Some("/bin/bash".into()),
            ..Default::default()
        };
        let (shell, flag) = resolve_shell(&cfg);
        assert_eq!(shell, "/bin/bash");
        assert_eq!(flag, "-c");
    }

    // ============ clamp_timeout 边界 ============

    #[test]
    fn test_clamp_timeout_boundaries() {
        // 0 钳制到下限 1
        assert_eq!(clamp_timeout(0), 1);
        // 恰在下限/正常值/恰在上限保持不变
        assert_eq!(clamp_timeout(1), 1);
        assert_eq!(clamp_timeout(60), 60);
        assert_eq!(clamp_timeout(3600), 3600);
        // 超上限钳制到 3600
        assert_eq!(clamp_timeout(3601), 3600);
        assert_eq!(clamp_timeout(u64::MAX), 3600);
    }

    // ============ binary_to_ext ============

    #[test]
    fn test_binary_to_ext_known() {
        assert_eq!(binary_to_ext("C:\\Python311\\python.exe"), "py");
        assert_eq!(binary_to_ext("PYTHON.EXE"), "py");
        assert_eq!(binary_to_ext("/usr/bin/bash"), "sh");
        assert_eq!(binary_to_ext("/usr/bin/zsh"), "sh");
        assert_eq!(binary_to_ext("C:\\Windows\\System32\\cmd.exe"), "bat");
        assert_eq!(binary_to_ext("run.bat"), "bat");
        assert_eq!(binary_to_ext("tool.exe"), "exe");
        assert_eq!(binary_to_ext("old.com"), "exe");
    }

    #[test]
    fn test_binary_to_ext_powershell_maps_ps1() {
        // powershell 不在支持范围：映射到 ps1 后由 is_supported_ext 拒绝
        assert_eq!(binary_to_ext("powershell.exe"), "ps1");
        assert_eq!(binary_to_ext("pwsh"), "ps1");
    }

    #[test]
    fn test_binary_to_ext_unknown_falls_back_to_py() {
        // 未知二进制回退为 py（与历史行为一致）
        assert_eq!(binary_to_ext("some-mystery-bin"), "py");
    }

    // ============ is_supported_ext ============

    #[test]
    fn test_is_supported_ext() {
        for ext in ["exe", "com", "py", "bat", "cmd", "sh"] {
            assert!(is_supported_ext(ext), "{ext} 应受支持");
        }
        for ext in ["ps1", "txt", "", "js"] {
            assert!(!is_supported_ext(ext), "{ext} 不应受支持");
        }
    }
}

#[cfg(test)]
mod python_runtime_tests {
    use super::uses_project_python;

    #[test]
    fn uses_project_python_only_for_default_python() {
        assert!(uses_project_python("py", None));
        assert!(uses_project_python("py", Some("")));
        assert!(uses_project_python("py", Some("   ")));
        assert!(!uses_project_python("py", Some("C:/Python312/python.exe")));
        assert!(!uses_project_python("bat", None));
        assert!(!uses_project_python("exe", None));
    }
}
