//! 任务异步执行器：TaskExecutor
//!
//! 统一入口 [`TaskExecutor::execute`] 按 [`TaskKind`] 分派：
//! - `Browser` → 经 [`BridgeSupervisor`] 执行浏览器任务，执行前后向 [`StatusManager`] 上报
//!   Worker 忙/空闲状态，并确保 Python 环境能力就绪（[`EnvironmentManager`]）；
//! - `Script` / `Shell` → 用 `tokio::process::Command` 执行，超时/取消通过 `tokio::time::timeout`
//!   与 `kill_on_drop` 实现，标准输出/错误截断到 `OUTPUT_TRUNCATE_LEN`。
//!
//! 脚本/Shell 执行通过 `tokio::sync::Mutex` 串行化，保证同一时刻最多一个在途。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio::process::Command;

use crate::bridge::{BridgeSupervisor, Outcome, StructuredResult};
use crate::config::ConfigService;
use crate::environment::EnvironmentManager;
use crate::status::{PartialSnapshot, StatusManager, WorkerStatus};
use crate::tasks::models::*;
use crate::tasks::TaskError;

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
    /// 脚本/Shell 执行的串行化锁
    exec_lock: Mutex<()>,
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
            exec_lock: Mutex::new(()),
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

    /// 执行浏览器任务（通用语义：打卡/签到等日常自动化，经 Bridge 的 `execute_browser_task`）
    ///
    /// 不注入账号密码、不做登录后网络验证；步骤执行完成即成功。
    /// 带凭据的登录语义请走 [`crate::login::LoginOrchestrator::submit`]。
    pub async fn execute_browser(&self, cfg: &TaskConfig) -> Result<TaskResult, TaskError> {
        // 执行前标记 Worker 忙
        self.status
            .merge(PartialSnapshot::Worker { state: WorkerStatus::Busy });

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
            let browser_settings =
                serde_json::to_value(&self.config.runtime().load_full().browser)
                    .unwrap_or(serde_json::Value::Null);
            let params = serde_json::json!({
                "task_config": task_val,
                "browser_settings": browser_settings,
            });
            self.bridge
                .execute("execute_browser_task", params)
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
                outcome: if success { Outcome::Success } else { Outcome::UnknownError },
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
    pub async fn execute_script(
        &self,
        cfg: &ScriptTaskConfig,
    ) -> Result<TaskResult, TaskError> {
        let _guard = self.exec_lock.lock().await;

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

        // 构建命令
        let (program, args) = self.build_script_command(cfg, &script_file, &ext);
        let work_dir = self.resolve_work_dir(cfg, &script_file);
        let envs = build_minimal_env();

        self.run_command(program, args, &work_dir, envs, clamp_timeout(cfg.timeout))
            .await
    }

    /// 执行 Shell 任务
    pub async fn execute_shell(&self, cfg: &ShellTaskConfig) -> Result<TaskResult, TaskError> {
        if cfg.command.trim().is_empty() {
            return Err(TaskError::CommandEmpty);
        }
        let _guard = self.exec_lock.lock().await;

        let (program, flag) = self.resolve_shell(cfg);
        let args = vec![flag.to_string(), cfg.command.clone()];
        let work_dir = self.tasks_dir.clone();
        let envs = build_minimal_env();

        self.run_command(program, args, &work_dir, envs, clamp_timeout(cfg.timeout))
            .await
    }

    // ---------------- 私有辅助 ----------------

    /// 解析脚本来源：content 写入临时文件（后缀按 binary_path 推断），否则用 script_path
    async fn resolve_script_source(
        &self,
        cfg: &ScriptTaskConfig,
    ) -> Result<(PathBuf, Option<tempfile::NamedTempFile>), TaskError> {
        if let Some(content) = &cfg.content {
            if content.len() > MAX_SCRIPT_CONTENT_SIZE {
                return Err(TaskError::ScriptNotFound(format!(
                    "脚本内容超过大小上限 {}", MAX_SCRIPT_CONTENT_SIZE
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

    /// 根据扩展名与 binary_path 构建命令（program, args）
    fn build_script_command(
        &self,
        cfg: &ScriptTaskConfig,
        script_file: &Path,
        ext: &str,
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
                    .unwrap_or_else(|| self.env.python_path().to_string_lossy().to_string());
                let mut args = vec![script];
                args.extend(cfg.args.clone());
                (python, args)
            }
            "ps1" => {
                let ps = cfg.binary_path.clone().unwrap_or_else(detect_powershell);
                let mut args = vec![
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-WindowStyle".to_string(),
                    "Hidden".to_string(),
                    "-File".to_string(),
                    script,
                ];
                args.extend(cfg.args.clone());
                (ps, args)
            }
            "bat" | "cmd" => {
                let cmd = cfg.binary_path.clone().unwrap_or_else(|| "cmd.exe".to_string());
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
            "js" => {
                let node = cfg
                    .binary_path
                    .clone()
                    .unwrap_or_else(|| "node".to_string());
                let mut args = vec![script];
                args.extend(cfg.args.clone());
                (node, args)
            }
            _ => (String::new(), Vec::new()),
        }
    }

    /// 解析工作目录：config.work_dir 优先，否则脚本所在目录
    fn resolve_work_dir(&self, cfg: &ScriptTaskConfig, script_file: &Path) -> PathBuf {
        if let Some(wd) = &cfg.work_dir {
            if Path::new(wd).is_absolute() {
                return PathBuf::from(wd);
            }
            return self.scripts_dir.join(wd);
        }
        script_file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.scripts_dir.clone())
    }

    /// 解析 Shell 路径与参数标志
    fn resolve_shell(&self, cfg: &ShellTaskConfig) -> (String, &'static str) {
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
            .envs(envs)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(TaskError::IoError)?;
        // 记录 PID：超时后需 taskkill /T 递归强杀整个进程树（kill_on_drop 只杀直接子进程）
        let pid = child.id();

        let timeout_dur = Duration::from_secs(timeout);
        let waited = tokio::time::timeout(timeout_dur, child.wait_with_output()).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match waited {
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let success = out.status.success();
                let exit_code = out.status.code().unwrap_or(-1);
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
                // 超时：child 已被 wait_with_output 的 future 持有，配合 kill_on_drop(true) 杀死直接子进程。
                // Windows 上 cmd.exe 启动的脚本子树可能响应直接 kill 后仍存活为孤儿（7.3），
                // 用 taskkill /T 递归强杀整个进程树兜底。
                #[cfg(windows)]
                if let Some(pid) = pid {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                    let _ = std::process::Command::new("taskkill")
                        .args(["/T", "/F", "/PID", &pid.to_string()])
                        .creation_flags(CREATE_NO_WINDOW)
                        .status();
                }
                Err(TaskError::ExecutionTimeout(timeout))
            }
        }
    }
}

/// 是否受支持的脚本扩展名
fn is_supported_ext(ext: &str) -> bool {
    matches!(ext, "exe" | "com" | "py" | "ps1" | "bat" | "cmd" | "sh" | "js")
}

/// 将 binary_path 推断出的临时文件后缀
fn binary_to_ext(binary: &str) -> &'static str {
    let b = binary.to_lowercase();
    if b.contains("python") || b.contains("py") {
        "py"
    } else if b.contains("node") {
        "js"
    } else if b.contains("powershell") || b.contains("pwsh") {
        "ps1"
    } else if b.contains("bash") || b.contains("sh") {
        "sh"
    } else {
        "py"
    }
}

/// 检测 PowerShell 可执行名（优先 pwsh）
fn detect_powershell() -> String {
    if find_in_path("pwsh") {
        "pwsh".to_string()
    } else {
        "powershell".to_string()
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
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}...(已截断)")
}

/// 构建最小环境变量（继承 PATH/HOME/TEMP 等）
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
    envs
}
