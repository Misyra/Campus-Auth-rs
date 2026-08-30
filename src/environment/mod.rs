//! 环境管理器：uv/Python/Git 引导

pub mod bootstrap;
pub mod git;
pub mod python;
pub mod uv;

pub use bootstrap::{bootstrap_capability, check_environment, retry_install};
pub use git::{check_git, download_mingit};
pub use python::{ensure_venv, install_playwright, install_playwright_browser};
pub use uv::{check_uv_on_path, download_uv, run_uv_sync, uv_exe_path, verify_sha256};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use reqwest::Client;
use tokio_util::sync::CancellationToken;

use crate::status::{InstallProgress, PartialSnapshot, StatusManager};

/// uv GitHub Releases 基础 URL（主站）
pub const UV_RELEASES_BASE: &str = "https://github.com/astral-sh/uv/releases/download";
/// GitHub 代理镜像列表（按优先级排序，用于国内网络环境）
pub const GITHUB_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://mirror.ghproxy.com/",
    "https://ghps.cc/",
];
/// GitHub API 代理镜像（用于获取版本号）
pub const GITHUB_API_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://mirror.ghproxy.com/",
];
/// uv 下载目标三元组（按操作系统 + 架构）
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const UV_TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const UV_TARGET: &str = "aarch64-pc-windows-msvc";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const UV_TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const UV_TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const UV_TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const UV_TARGET: &str = "aarch64-apple-darwin";
/// uv 可执行文件名
#[cfg(target_os = "windows")]
pub const UV_EXE_NAME: &str = "uv.exe";
#[cfg(not(target_os = "windows"))]
pub const UV_EXE_NAME: &str = "uv";
/// uv 下载超时
pub const UV_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// uv 下载重试次数
pub const UV_DOWNLOAD_MAX_RETRIES: u32 = 3;
/// uv 下载重试间隔
pub const UV_DOWNLOAD_RETRY_DELAY: Duration = Duration::from_secs(5);
/// uv 最低版本要求
pub const UV_MIN_VERSION: &str = "0.5.0";
/// uv 锁定版本（None = latest）
pub const UV_PINNED_VERSION: Option<&str> = None;
/// Python 版本约束
pub const PYTHON_VERSION_CONSTRAINT: &str = ">=3.12,<3.13";
/// uv sync 超时
pub const UV_SYNC_TIMEOUT: Duration = Duration::from_secs(600);
/// uv sync 重试次数
pub const UV_SYNC_MAX_RETRIES: u32 = 1;
/// playwright install 浏览器超时
pub const PLAYWRIGHT_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// playwright install 重试次数
pub const PLAYWRIGHT_INSTALL_MAX_RETRIES: u32 = 3;
/// playwright install 重试间隔
pub const PLAYWRIGHT_INSTALL_RETRY_DELAY: Duration = Duration::from_secs(10);
/// MinGit GitHub Releases 基础 URL
pub const MINGIT_RELEASES_BASE: &str = "https://github.com/git-for-windows/git/releases/download";
/// MinGit 下载目标
pub const MINGIT_TARGET: &str = "64-bit";
/// MinGit 下载超时
pub const MINGIT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// environment/ 子目录名
pub const ENV_DIR: &str = "environment";
/// python_worker/ 子目录名
pub const WORKER_PROJECT_DIR: &str = "python_worker";
/// 虚拟环境目录名（相对于 worker_project_path）
pub const VENV_DIR: &str = ".venv";
/// Python 解释器相对路径（相对于 worker_project_path）
///
/// venv 目录布局因平台而异：Windows 为 `Scripts/python.exe`，
/// macOS / Linux 为 `bin/python`——硬编码 Windows 布局会让 unix 上
/// venv 检测、引导与 Bridge spawn 全链路误判"未安装"。
#[cfg(target_os = "windows")]
pub const PYTHON_EXE_RELATIVE: &str = ".venv/Scripts/python.exe";
#[cfg(not(target_os = "windows"))]
pub const PYTHON_EXE_RELATIVE: &str = ".venv/bin/python";

/// 解析 python_worker 工程目录（单一事实源，Bridge spawn 检查与 EnvironmentManager 共用）
///
/// 主路径为 `<base_path>/python_worker`；开发模式（如 cargo run 时 base_path=target/debug）
/// 该目录不存在，回退到仓库根 / CARGO_MANIFEST_DIR 下的 python_worker（与 docs 背景图的多路径兜底一致）。
/// Bridge 的 spawn 前检查必须使用本函数结果，否则 dev 模式会误报"Worker 环境未安装"。
pub(crate) fn resolve_worker_project_path(base_path: &std::path::Path) -> PathBuf {
    let candidate = base_path.join(WORKER_PROJECT_DIR);
    if candidate.exists() {
        return candidate;
    }
    if let Some(repo) = base_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(WORKER_PROJECT_DIR))
    {
        if repo.exists() {
            return repo;
        }
    }
    let mf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(WORKER_PROJECT_DIR);
    if mf.exists() { mf } else { candidate }
}
/// 进度百分比区间
pub const PROGRESS_UV_DOWNLOAD: (u8, u8) = (0, 20);
/// 进度百分比区间
pub const PROGRESS_VENV_SYNC: (u8, u8) = (20, 60);
/// 进度百分比区间
pub const PROGRESS_PLAYWRIGHT: (u8, u8) = (60, 85);
/// 进度百分比区间（可选 MinGit）
pub const PROGRESS_MINGIT: (u8, u8) = (85, 100);

/// 环境安装错误
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    /// environment 目录无写权限
    #[error("environment 目录无写权限: {path}")]
    DirectoryNotWritable { path: PathBuf },

    /// 并发等待者复用前一轮引导的失败结果（F1）
    ///
    /// `EnvironmentError` 内含 reqwest/io 错误不可 Clone，BootstrapGate 的
    /// 失败复用改为存 Display 字符串、经本变体重构，消息保真、变体信息有损。
    #[error("环境引导失败（复用并发前一轮结果）: {0}")]
    BootstrapFailedShared(String),

    /// uv 下载失败（HTTP 层错误）
    #[error("uv 下载失败 (重试 {retries} 次): {source}")]
    UvDownloadFailed {
        retries: u32,
        source: reqwest::Error,
    },

    /// uv 下载失败（超时/IO 等非 HTTP 错误）
    #[error("uv 下载失败 (重试 {retries} 次): {message}")]
    UvDownloadIoFailed { retries: u32, message: String },

    /// uv 下载文件 SHA256 校验失败
    #[error("uv 下载文件 SHA256 校验失败: expected={expected}, got={got}")]
    UvChecksumMismatch { expected: String, got: String },

    /// uv 解压失败
    #[error("uv 解压失败: {0}")]
    UvExtractFailed(#[from] std::io::Error),

    /// GitHub API 请求失败
    #[error("GitHub API 请求失败 (获取 uv 版本): {0}")]
    GitHubApiError(String),

    /// uv sync 失败
    #[error("uv sync 失败 (exit code={exit_code:?}): {stderr}")]
    UvSyncFailed {
        exit_code: Option<i32>,
        stderr: String,
    },

    /// uv sync 超时
    #[error("uv sync 超时 (>{timeout_secs}s)")]
    UvSyncTimeout { timeout_secs: u64 },

    /// Playwright 安装失败
    #[error("Playwright 安装失败 (重试 {retries} 次): {message}")]
    PlaywrightInstallFailed { retries: u32, message: String },

    /// Playwright 安装超时
    #[error("Playwright 安装超时 (>{timeout_secs}s)")]
    PlaywrightInstallTimeout { timeout_secs: u64 },

    /// 请求安装了不受支持的 Playwright 浏览器
    #[error("不支持的 Playwright 浏览器: {browser}")]
    UnsupportedPlaywrightBrowser { browser: String },

    /// .venv 损坏，需要重建
    #[error(".venv 损坏，需要重建")]
    VenvCorrupted,

    /// python_worker/ 目录不存在
    #[error("python_worker/ 目录不存在: {}", path.display())]
    WorkerProjectNotFound { path: PathBuf },

    /// MinGit 下载失败
    #[error("MinGit 下载失败: {0}")]
    MinGitDownloadFailed(String),

    /// MinGit 下载文件 SHA256 校验失败
    #[error("MinGit 下载文件 SHA256 校验失败: expected={expected}, got={got}")]
    MinGitChecksumMismatch { expected: String, got: String },

    /// 安装被取消
    #[error("安装被取消")]
    Cancelled,
}

/// 引导阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStage {
    /// 未开始 / 空闲
    Idle,
    /// 正在下载 uv
    DownloadingUv,
    /// 正在创建虚拟环境 + 安装依赖
    SyncingVenv,
    /// 正在安装 Playwright 浏览器
    InstallingPlaywright,
    /// 正在下载 MinGit（可选）
    DownloadingMinGit,
    /// 全部完成
    Done,
    /// 安装失败
    Error,
}

/// 环境安装状态
#[derive(Debug, Clone)]
pub struct EnvironmentStatus {
    /// uv 是否已下载就绪
    pub uv_ready: bool,
    /// Python 虚拟环境是否就绪
    pub python_ready: bool,
    /// Playwright 浏览器是否已安装
    pub playwright_ready: bool,
    /// Git 是否可用（仅开发者模式）
    pub git_ready: bool,
    /// 浏览器自动化能力是否完全就绪
    pub capability_ready: bool,
    /// 当前安装阶段
    pub stage: BootstrapStage,
    /// 安装进度
    pub progress: Option<InstallProgress>,
    /// 最后的错误消息
    pub last_error: Option<String>,
}

/// 引导互斥门（F1）：串行化并发的 `ensure_capability` / `ensure_python_runtime` /
/// `retry_install` / OCR 依赖同步 / 显式 Playwright 浏览器安装
///
/// 此前三处调用（tasks/executor、web/routes/ocr、web/routes/system）可并发
/// check-then-act 触发 bootstrap，踩踏同一 `.venv` 与固定临时名（uv.zip.tmp /
/// uv.exe.tmp）。本门以单把 tokio Mutex 串行化，语义：
/// - 快速路径：已就绪直接返回（无锁开销）；
/// - 并发等待者获得锁后**重新检查就绪状态**（双检），已就绪则跳过引导；
/// - 等待期间若已有一轮引导结束（无论成败，以代数判断）且仍未就绪，
///   直接复用该轮的失败结果，避免每个等待者各自重跑完整下载/引导流程；
/// - `run_exclusive` 供显式重装（retry_install）使用：不双检、不复用，
///   但与 ensure 共享同一把锁，防止重装与引导并发踩踏。
pub(crate) struct BootstrapGate {
    /// 引导互斥锁：持锁跨越整个 bootstrap 过程（含全部下载/解压/同步 await）
    lock: tokio::sync::Mutex<()>,
    /// 引导完成代数：每次 bootstrap 结束（无论成败）单调递增
    generation: AtomicU64,
    /// 上一次引导的失败结果（成功时清除），供并发等待者复用；
    /// 存 Display 字符串而非错误值（EnvironmentError 含不可 Clone 的源错误）
    last_error: Mutex<Option<String>>,
}

impl BootstrapGate {
    /// 构造初始门（空闲、零代、无失败记录）
    fn new() -> Self {
        Self {
            lock: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// 串行化执行引导（见类型级注释的完整语义）
    pub(crate) async fn ensure<B, F>(
        &self,
        is_ready: impl Fn() -> bool,
        bootstrap: B,
    ) -> Result<(), EnvironmentError>
    where
        B: FnOnce() -> F,
        F: std::future::Future<Output = Result<(), EnvironmentError>>,
    {
        // 快速路径：已就绪直接返回，不触碰互斥锁
        if is_ready() {
            return Ok(());
        }
        // 记录进入时的代数：等待期间已有引导结束则以代数差识别
        let entry_generation = self.generation.load(Ordering::Acquire);
        let _guard = self.lock.lock().await;
        // 双检：持锁期间可能已被前一个执行者完成引导
        if is_ready() {
            return Ok(());
        }
        if self.generation.load(Ordering::Acquire) > entry_generation {
            // 等待期间已有一轮引导结束且仍未就绪 → 复用该轮失败结果
            if let Some(message) = self
                .last_error
                .lock()
                .expect("BootstrapGate last_error 锁中毒")
                .clone()
            {
                return Err(EnvironmentError::BootstrapFailedShared(message));
            }
            // 防御性兜底：成功引导必然置 ready，理论上不可达
            return Ok(());
        }
        let result = bootstrap().await;
        // 记录失败结果供后续等待者复用（成功时清除）；存 Display 字符串
        *self
            .last_error
            .lock()
            .expect("BootstrapGate last_error 锁中毒") =
            result.as_ref().err().map(|e| e.to_string());
        self.generation.fetch_add(1, Ordering::Release);
        result
    }

    /// 仅持锁执行（显式重装路径）：与 ensure 互斥但不做双检/结果复用
    pub(crate) async fn run_exclusive<T>(&self, f: impl std::future::Future<Output = T>) -> T {
        let _guard = self.lock.lock().await;
        f.await
    }
}

/// 环境管理器：管理 uv/Python/Playwright 浏览器能力
pub struct EnvironmentManager {
    /// 基准路径（exe 所在目录）
    base_path: PathBuf,
    /// environment/ 目录绝对路径
    env_path: PathBuf,
    /// python_worker/ 目录绝对路径
    worker_project_path: PathBuf,
    /// 当前安装状态（原子可写）
    status: Arc<RwLock<EnvironmentStatus>>,
    /// StatusManager 引用（推送安装进度）
    status_manager: Arc<StatusManager>,
    /// HTTP 客户端
    http_client: Client,
    /// 当前正在执行的安装 generation 的取消令牌。
    ///
    /// 每个真正获得 BootstrapGate 执行权的 operation 都会替换它，并把自己的
    /// token clone 显式传入整个调用链；旧 operation 因此永远不会读取到新代 token。
    current_cancel_token: RwLock<CancellationToken>,
    /// 是否允许下载 MinGit（仅开发者模式启用）
    git_download_enabled: bool,
    /// 引导完成回调（成功重建环境时触发，用于复位 Bridge 熔断计数 B3）
    on_bootstrap_done: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// 引导互斥门（F1）：串行化并发 ensure_capability / retry_install
    pub(crate) bootstrap_gate: BootstrapGate,
}

/// Web 层消费的环境能力抽象（M1 细粒度 state：environment 域）
///
/// handler 通过 `State<Arc<dyn EnvironmentApi>>` 提取依赖（ocr/scripts/system
/// 路由），不再触达 `state.container`，测试可注入内存实现。
#[async_trait::async_trait]
pub trait EnvironmentApi: Send + Sync {
    /// 返回当前环境状态快照。
    fn status(&self) -> EnvironmentStatus;
    /// Python 解释器绝对路径。
    fn python_path(&self) -> PathBuf;
    /// 确保浏览器自动化能力就绪；若未就绪则触发引导。
    async fn ensure_capability(&self) -> Result<(), EnvironmentError>;
    /// 显式安装指定 Playwright 管理浏览器（chromium/firefox/webkit）。
    async fn install_playwright_browser(&self, browser: &str) -> Result<(), EnvironmentError>;
    /// 安装 OCR optional extra，并持久记录用户启用偏好。
    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// 卸载 OCR optional extra，并清除用户启用偏好。
    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// OCR 依赖（ddddocr）是否已安装在 venv 内。
    fn ocr_ready(&self) -> bool;
    /// 项目是否声明支持 `ocr` optional extra。
    fn ocr_declared(&self) -> bool;
}

#[async_trait::async_trait]
impl EnvironmentApi for EnvironmentManager {
    fn status(&self) -> EnvironmentStatus {
        EnvironmentManager::status(self)
    }

    fn python_path(&self) -> PathBuf {
        EnvironmentManager::python_path(self)
    }

    async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
        EnvironmentManager::ensure_capability(self).await
    }

    async fn install_playwright_browser(&self, browser: &str) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(async {
                let cancel = self.begin_install_generation();
                crate::environment::python::install_playwright_browser(self, browser, &cancel).await
            })
            .await
    }

    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(async {
                let cancel = self.begin_install_generation();
                crate::environment::uv::install_ocr_dep(self, &cancel).await
            })
            .await
    }

    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(async {
                let cancel = self.begin_install_generation();
                crate::environment::uv::remove_ocr_dep(self, &cancel).await
            })
            .await
    }

    fn ocr_ready(&self) -> bool {
        crate::environment::python::ddddocr_installed(self)
    }

    fn ocr_declared(&self) -> bool {
        crate::environment::python::ocr_declared(self)
    }
}

impl EnvironmentManager {
    /// 构造环境管理器
    pub fn new(
        base_path: PathBuf,
        status_manager: Arc<StatusManager>,
        git_download_enabled: bool,
    ) -> Arc<Self> {
        let env_path = base_path.join(ENV_DIR);
        let worker_project_path = resolve_worker_project_path(&base_path);
        Arc::new(Self {
            base_path,
            env_path,
            worker_project_path,
            status: Arc::new(RwLock::new(EnvironmentStatus {
                uv_ready: false,
                python_ready: false,
                playwright_ready: false,
                git_ready: false,
                capability_ready: false,
                stage: BootstrapStage::Idle,
                progress: None,
                last_error: None,
            })),
            status_manager,
            http_client: Client::new(),
            current_cancel_token: RwLock::new(CancellationToken::new()),
            git_download_enabled,
            on_bootstrap_done: Mutex::new(None),
            bootstrap_gate: BootstrapGate::new(),
        })
    }

    /// 注册引导完成回调（成功重建环境后触发）
    ///
    /// 用于复位 Bridge 的连续 spawn 失败熔断计数（B3）：环境修复后 Worker
    /// 才有重新 spawn 成功的可能，此时解除熔断。
    pub fn set_on_bootstrap_done(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        *self
            .on_bootstrap_done
            .lock()
            .expect("on_bootstrap_done 锁中毒") = Some(cb);
    }

    /// 触发引导完成回调（内部，引导成功路径调用）
    pub(crate) fn fire_bootstrap_done(&self) {
        let cb = self
            .on_bootstrap_done
            .lock()
            .expect("on_bootstrap_done 锁中毒")
            .clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    /// 能力是否就绪
    pub fn is_ready(&self) -> bool {
        self.status
            .read()
            .expect("EnvironmentStatus 读锁中毒")
            .capability_ready
    }

    /// 能力是否就绪（capability_ready 别名，供 Bridge 等调用）
    pub fn capability_ready(&self) -> bool {
        self.status
            .read()
            .expect("EnvironmentStatus 读锁中毒")
            .capability_ready
    }

    /// 返回当前环境状态快照
    pub fn status(&self) -> EnvironmentStatus {
        self.status
            .read()
            .expect("EnvironmentStatus 读锁中毒")
            .clone()
    }

    /// Python 解释器绝对路径
    pub fn python_path(&self) -> PathBuf {
        self.worker_project_path.join(PYTHON_EXE_RELATIVE)
    }

    /// 项目内 Python 运行时是否就绪。
    pub fn python_runtime_ready(&self) -> bool {
        self.status
            .read()
            .expect("EnvironmentStatus 读锁中毒")
            .python_ready
    }

    /// 确保项目内 Python 运行时就绪，只准备 uv + venv，不安装 Playwright 浏览器。
    ///
    /// 与完整浏览器引导共用 BootstrapGate：若两类首次使用并发发生，只允许一轮
    /// 环境写操作进入 `.venv`，等待者获得锁后会按 python_ready 再次双检。
    pub async fn ensure_python_runtime(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .ensure(
                || self.python_runtime_ready(),
                || async {
                    let cancel = self.begin_install_generation();
                    bootstrap::bootstrap_python_runtime(self, &cancel).await?;
                    self.write_status(|s| s.stage = BootstrapStage::Done);
                    self.report_progress("python_ready", PROGRESS_VENV_SYNC.1, "Python 环境就绪");
                    Ok(())
                },
            )
            .await
    }

    /// 确保浏览器自动化能力就绪；若未就绪则触发引导
    ///
    /// OCR 依赖由前端显式安装/卸载；用户启用后会写入持久标记，后续环境
    /// 修复通过 `uv sync --extra ocr` 保留该选择，未启用时只同步基础依赖。
    ///
    /// F1：经 BootstrapGate 串行化——并发调用者等待锁后二次检查就绪状态，
    /// 只有一个调用者真正执行引导，其余复用其结果，避免并发 bootstrap
    /// 踩踏同一 .venv 与固定临时名（uv.zip.tmp / uv.exe.tmp）。
    pub async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .ensure(
                || self.is_ready(),
                || async {
                    // 只有真正获得本轮 bootstrap 执行权的调用者才创建新 generation。
                    // token 随后按值 clone/按引用显式传入调用链，等待者不会触碰它。
                    let cancel = self.begin_install_generation();
                    bootstrap_capability(self, &cancel).await
                },
            )
            .await
    }

    /// 重新安装环境能力
    ///
    /// 经 BootstrapGate 的排他执行与 ensure_capability 互斥（F1）：
    /// 显式重装不允许与引导并发进行。
    pub async fn retry_install(&self) -> Result<(), EnvironmentError> {
        retry_install(self).await
    }

    /// 取消当前在途安装 generation。
    ///
    /// 新一轮 operation 只有在获得 BootstrapGate 排他执行权后才会注册全新 token，
    /// 因此这里永远不会把已排队但尚未开始的下一轮误取消。
    pub fn cancel(&self) {
        self.current_cancel_token
            .read()
            .expect("Environment current_cancel_token 读锁中毒")
            .cancel();
    }

    /// 读取环境状态（供 uv.rs/python.rs/git.rs 复用）
    pub(crate) fn read_status(&self) -> std::sync::RwLockReadGuard<'_, EnvironmentStatus> {
        self.status.read().expect("EnvironmentStatus 读锁中毒")
    }

    /// 更新环境状态（供 uv.rs/python.rs/git.rs 复用）
    pub(crate) fn write_status<F: FnOnce(&mut EnvironmentStatus)>(&self, f: F) {
        let mut guard = self.status.write().expect("EnvironmentStatus 写锁中毒");
        f(&mut guard);
    }

    /// 推送安装进度到 StatusManager
    pub(crate) fn report_progress(&self, phase: &str, percent: u8, message: &str) {
        self.write_status(|s| {
            s.progress = Some(InstallProgress {
                phase: phase.to_string(),
                percent,
                message: message.to_string(),
            });
        });
        self.status_manager.merge(PartialSnapshot::Environment {
            progress: Some(InstallProgress {
                phase: phase.to_string(),
                percent,
                message: message.to_string(),
            }),
        });
    }

    /// 基准路径（exe 所在目录）
    pub(crate) fn base_path(&self) -> &PathBuf {
        &self.base_path
    }

    /// 读取/写入基准路径
    pub(crate) fn env_path(&self) -> &PathBuf {
        &self.env_path
    }

    /// python_worker 路径
    pub(crate) fn worker_project_path(&self) -> &PathBuf {
        &self.worker_project_path
    }

    /// HTTP 客户端
    pub(crate) fn http_client(&self) -> &Client {
        &self.http_client
    }

    /// 开始一轮新的安装 generation，并返回该轮独占的取消令牌 clone。
    ///
    /// 必须仅在持有 BootstrapGate 排他执行权时调用。调用方把返回值显式传给
    /// 本轮所有子操作，禁止子操作再回到 EnvironmentManager 动态读取当前 token。
    pub(crate) fn begin_install_generation(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self
            .current_cancel_token
            .write()
            .expect("Environment current_cancel_token 写锁中毒") = token.clone();
        token
    }

    /// 是否允许下载 MinGit
    pub(crate) fn git_download_enabled(&self) -> bool {
        self.git_download_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// 脚本执行器与 Bridge 必须引用同一个 python_worker/.venv。
    #[test]
    fn test_python_path_points_to_worker_project_venv() {
        let dir = tempfile::TempDir::new().unwrap();
        // 主路径前提：base_path 下须存在 python_worker/，否则会回退到
        // 仓库根 / CARGO_MANIFEST_DIR 的兜底目录（dev 环境下命中仓库）
        std::fs::create_dir(dir.path().join(WORKER_PROJECT_DIR)).unwrap();
        let manager = EnvironmentManager::new(
            dir.path().to_path_buf(),
            Arc::new(StatusManager::new()),
            false,
        );
        assert_eq!(
            manager.python_path(),
            dir.path()
                .join(WORKER_PROJECT_DIR)
                .join(PYTHON_EXE_RELATIVE)
        );
    }

    /// 每轮安装持有独立 token：取消旧轮后，新 generation 必须自动获得全新 scope。
    #[test]
    fn test_install_generation_uses_independent_cancellation_scopes() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = EnvironmentManager::new(
            dir.path().to_path_buf(),
            Arc::new(StatusManager::new()),
            false,
        );

        let first = manager.begin_install_generation();
        assert!(!first.is_cancelled());
        manager.cancel();
        assert!(first.is_cancelled());

        let second = manager.begin_install_generation();
        assert!(!second.is_cancelled(), "新 generation 不得继承旧轮取消状态");
        assert!(first.is_cancelled(), "创建新 generation 不得复活旧 token");

        manager.cancel();
        assert!(second.is_cancelled(), "cancel 只能命中当前 generation");
        assert!(first.is_cancelled());
    }

    /// F1：并发 ensure 只触发一次真实引导——抢到锁的执行者跑引导，
    /// 其余等待者复用其失败结果，避免并发 bootstrap 踩踏
    /// 同一 .venv 与固定临时名（uv.zip.tmp / uv.exe.tmp）
    #[tokio::test]
    async fn test_bootstrap_gate_concurrent_single_execution() {
        let gate = Arc::new(BootstrapGate::new());
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let gate = gate.clone();
            let attempts = attempts.clone();
            handles.push(tokio::spawn(async move {
                // 模拟环境未就绪（is_ready 恒 false），强制走引导路径
                gate.ensure(
                    || false,
                    move || {
                        let attempts = attempts.clone();
                        async move {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            // 模拟一次耗时的引导过程（下载/sync）
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            Err(EnvironmentError::Cancelled)
                        }
                    },
                )
                .await
            }));
        }

        let mut failures = 0;
        for h in handles {
            assert!(
                matches!(
                    h.await.unwrap(),
                    Err(EnvironmentError::Cancelled)
                        | Err(EnvironmentError::BootstrapFailedShared(_))
                ),
                "所有并发调用者应复用首个执行者的失败结果（首个拿到原始错误，\
                 等待者经 BootstrapFailedShared 复用其摘要）"
            );
            failures += 1;
        }
        assert_eq!(failures, 8);
        // 8 个并发请求只触发一次真实引导
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "并发 ensure 只允许触发一次 bootstrap"
        );
    }

    /// F1：已就绪时走无锁快速路径，不触发引导
    #[tokio::test]
    async fn test_bootstrap_gate_ready_short_circuit() {
        let gate = Arc::new(BootstrapGate::new());
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_in_bootstrap = attempts.clone();

        let result = gate
            .ensure(
                || true, // 已就绪
                move || {
                    let attempts = attempts_in_bootstrap.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 0, "就绪时不得触发引导");
    }

    /// F1：首个执行者成功置就绪后，并发等待者双检直接返回成功
    #[tokio::test]
    async fn test_bootstrap_gate_success_double_check() {
        let gate = Arc::new(BootstrapGate::new());
        let ready = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..3 {
            let gate = gate.clone();
            let ready_check = ready.clone();
            let ready_boot = ready.clone();
            let attempts_boot = attempts.clone();
            handles.push(tokio::spawn(async move {
                gate.ensure(
                    move || ready_check.load(Ordering::SeqCst),
                    move || {
                        let ready = ready_boot;
                        let attempts = attempts_boot;
                        async move {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            ready.store(true, Ordering::SeqCst);
                            Ok(())
                        }
                    },
                )
                .await
            }));
        }

        for h in handles {
            assert!(h.await.unwrap().is_ok());
        }
        // 引导只执行一次，三个并发调用者全部成功
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(ready.load(Ordering::SeqCst));
    }
}
