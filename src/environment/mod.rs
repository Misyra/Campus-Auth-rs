//! 环境管理器：uv/Python/Git 引导

pub mod bootstrap;
pub mod git;
pub mod python;
pub mod uv;

pub use bootstrap::{bootstrap_capability, check_environment, retry_install};
pub use git::{check_git, download_mingit};
pub use python::{ensure_venv, install_playwright};
pub use uv::{
    check_uv_on_path, download_uv, run_uv_sync, uv_exe_path, verify_sha256,
};

use std::path::PathBuf;
use std::sync::Arc;
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
/// playwright install chromium 超时
pub const PLAYWRIGHT_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// playwright install 重试次数
pub const PLAYWRIGHT_INSTALL_MAX_RETRIES: u32 = 3;
/// playwright install 重试间隔
pub const PLAYWRIGHT_INSTALL_RETRY_DELAY: Duration = Duration::from_secs(10);
/// MinGit GitHub Releases 基础 URL
pub const MINGIT_RELEASES_BASE: &str =
    "https://github.com/git-for-windows/git/releases/download";
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
pub const PYTHON_EXE_RELATIVE: &str = ".venv/Scripts/python.exe";
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
    UvSyncFailed { exit_code: Option<i32>, stderr: String },

    /// uv sync 超时
    #[error("uv sync 超时 (>{timeout_secs}s)")]
    UvSyncTimeout { timeout_secs: u64 },

    /// Playwright 安装失败
    #[error("Playwright 安装失败 (重试 {retries} 次): {message}")]
    PlaywrightInstallFailed { retries: u32, message: String },

    /// Playwright 安装超时
    #[error("Playwright 安装超时 (>{timeout_secs}s)")]
    PlaywrightInstallTimeout { timeout_secs: u64 },

    /// .venv 损坏，需要重建
    #[error(".venv 损坏，需要重建")]
    VenvCorrupted,

    /// python_worker/ 目录不存在
    #[error("python_worker/ 目录不存在: {path}")]
    WorkerProjectNotFound { path: PathBuf },

    /// MinGit 下载失败
    #[error("MinGit 下载失败: {0}")]
    MinGitDownloadFailed(String),

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
    /// 取消令牌
    cancel_token: CancellationToken,
    /// 是否允许下载 MinGit（仅开发者模式启用）
    git_download_enabled: bool,
    /// 引导完成回调（成功重建环境时触发，用于复位 Bridge 熔断计数 B3）
    on_bootstrap_done: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
}

impl EnvironmentManager {
    /// 构造环境管理器
    pub fn new(base_path: PathBuf, status_manager: Arc<StatusManager>, git_download_enabled: bool) -> Arc<Self> {
        let env_path = base_path.join(ENV_DIR);
        let worker_project_path = base_path.join(WORKER_PROJECT_DIR);
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
            cancel_token: CancellationToken::new(),
            git_download_enabled,
            on_bootstrap_done: Mutex::new(None),
        })
    }

    /// 注册引导完成回调（成功重建环境后触发）
    ///
    /// 用于复位 Bridge 的连续 spawn 失败熔断计数（B3）：环境修复后 Worker
    /// 才有重新 spawn 成功的可能，此时解除熔断。
    pub fn set_on_bootstrap_done(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        *self.on_bootstrap_done.lock().expect("on_bootstrap_done 锁中毒") = Some(cb);
    }

    /// 触发引导完成回调（内部，引导成功路径调用）
    pub(crate) fn fire_bootstrap_done(&self) {
        let cb = self.on_bootstrap_done.lock().expect("on_bootstrap_done 锁中毒").clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    /// 能力是否就绪
    pub fn is_ready(&self) -> bool {
        self.status.read().expect("EnvironmentStatus 读锁中毒").capability_ready
    }

    /// 能力是否就绪（capability_ready 别名，供 Bridge 等调用）
    pub fn capability_ready(&self) -> bool {
        self.status.read().expect("EnvironmentStatus 读锁中毒").capability_ready
    }

    /// 返回当前环境状态快照
    pub fn status(&self) -> EnvironmentStatus {
        self.status.read().expect("EnvironmentStatus 读锁中毒").clone()
    }

    /// Python 解释器绝对路径
    pub fn python_path(&self) -> PathBuf {
        self.env_path.join(PYTHON_EXE_RELATIVE)
    }

    /// 确保浏览器自动化能力就绪；若未就绪则触发引导
    ///
    /// 就绪但 OCR 依赖（ddddocr，optional extra）缺失时也触发引导补装
    /// （ensure_venv 幂等：仅增量补 ddddocr，不重建 venv）。
    pub async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
        if self.is_ready() {
            if crate::environment::python::ddddocr_installed(self) {
                return Ok(());
            }
            tracing::info!("OCR 依赖（ddddocr）缺失，触发增量补装...");
        }
        bootstrap_capability(self).await
    }

    /// 重新安装环境能力
    pub async fn retry_install(&self) -> Result<(), EnvironmentError> {
        retry_install(self).await
    }

    /// 取消在途安装
    pub fn cancel(&self) {
        self.cancel_token.cancel();
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

    /// 取消令牌
    pub(crate) fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }

    /// 是否允许下载 MinGit
    pub(crate) fn git_download_enabled(&self) -> bool {
        self.git_download_enabled
    }
}
