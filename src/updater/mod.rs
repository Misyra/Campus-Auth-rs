//! 更新器业务模块
//!
//! 负责后台版本检查、流式下载 + SHA256 校验、staging 暂存、写入 `pending.json`
//! 并 spawn 助手进程（`campus-auth-helper`）完成 exe 替换与重启。
//!
//! 助手进程契约：读取 `<base_path>/update/pending.json`，等待主进程（PID 由
//! `--pid` 传入）退出后，将 `staging_dir/extracted/<EXE_NAME>` 复制到 `target_exe`
//! 并以其 `original_args` 重启，最后清理 staging 与 pending 标记。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use semver::Version;
use tokio_util::sync::CancellationToken;

use crate::config::ConfigService;
use crate::status::{InstallProgress, LoginStatus, PartialSnapshot, StatusManager};

mod apply;
/// pub(crate)：environment/git.rs（MinGit sha256 校验，R3）复用
/// fetch_sha256_assoc 伴随文件查找模式
pub(crate) mod check;
mod download;
pub mod error;

pub use apply::PendingUpdate;
pub use check::{PlatformPackage, ReleaseManifest};
pub use download::StagedUpdate;
pub use error::UpdaterError;

/// 后台检查任务启动前的延迟（等待核心启动完成）
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(5);

/// 版本检查结果
///
/// 既作为 API 响应（`GET /api/check-update`），也携带 `apply_update` 所需的下载信息
/// （`url` / `sha256` / `size`）。
///
/// JSON 字段名对齐前端契约：`has_update` / `latest` / `current`。
#[derive(Clone, Debug, serde::Serialize)]
pub struct UpdateInfo {
    /// 当前版本（展示用，JSON 键 `current`）
    #[serde(rename = "current")]
    pub current_version: String,
    /// 远程最新版本（JSON 键 `latest`）
    #[serde(rename = "latest")]
    pub latest_version: String,
    /// 是否有更新（JSON 键 `has_update`）
    #[serde(rename = "has_update")]
    pub update_available: bool,
    /// 下载包 URL
    pub url: String,
    /// 预期 SHA256 hex
    pub sha256: String,
    /// 下载大小（字节）
    pub size: Option<u64>,
    /// 更新说明（changelog）
    pub notes: Option<String>,
    /// 发布日期
    pub release_date: Option<String>,
}

/// 更新器服务：封装版本检查、下载、暂存与助手替换
pub struct UpdaterService {
    /// 配置服务（读取 `global.updater` 段）
    config: Arc<ConfigService>,
    /// 状态管理器（推送 update_available / 下载进度）
    status: Arc<StatusManager>,
    /// 共享 HTTP 客户端（全局复用连接池）
    http_client: reqwest::Client,
    /// 项目根目录（用于构造 update/ staging 路径）
    base_path: PathBuf,
    /// 当前版本（`CARGO_PKG_VERSION` 解析）
    current_version: Version,
    /// 防止并发触发下载
    update_in_progress: AtomicBool,
}

/// Web 层消费的更新器抽象（M1 细粒度 state：updater 域）
///
/// handler 通过 `State<Arc<dyn UpdaterApi>>` 提取依赖（system 路由），
/// 不再触达 `state.container`，测试可注入内存实现。
#[async_trait::async_trait]
pub trait UpdaterApi: Send + Sync {
    /// 手动触发版本检查；有新版本返回 `Some(UpdateInfo)`。
    async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError>;
    /// 执行更新（下载 zip 到 staging 并触发助手替换）。
    async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError>;
}

#[async_trait::async_trait]
impl UpdaterApi for UpdaterService {
    async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        UpdaterService::check_update(self).await
    }

    async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        UpdaterService::apply_update(self, info).await
    }
}

impl UpdaterService {
    /// 构造更新器服务
    ///
    /// 解析 `CARGO_PKG_VERSION` 为 `semver::Version`（解析失败时回退 `0.0.0`，
    /// 该情况下不会误报更新）；返回 `Arc<Self>` 以便共享。参数顺序与类型由容器调用约定固定。
    pub fn new(
        config: Arc<ConfigService>,
        status: Arc<StatusManager>,
        base_path: PathBuf,
    ) -> Arc<Self> {
        let current_version = match Version::parse(env!("CARGO_PKG_VERSION")) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("当前版本号解析失败，回退 0.0.0: {}", e);
                Version::new(0, 0, 0)
            }
        };
        let http_client = reqwest::Client::new();

        Arc::new(Self {
            config,
            status,
            http_client,
            base_path,
            current_version,
            update_in_progress: AtomicBool::new(false),
        })
    }

    /// 启动后台版本检查任务（循环：启动时检查一次，之后按 check_interval_hours 定时检查）
    ///
    /// 延迟 [`STARTUP_CHECK_DELAY`] 后拉取清单，发现更新则
    /// `merge(PartialSnapshot::Update { available: true })`；失败静默忽略。
    /// `cancel` 用于优雅中止。
    ///
    /// 语义（U6 修复）：`check_on_startup` 只决定"启动是否立即检查一次"（循环外读一次），
    /// 循环内的周期检查不受其影响——否则关闭该开关会连定时检查一并消失。
    pub fn start_background_check(&self, cancel: CancellationToken) {
        let config = self.config.clone();
        let status = self.status.clone();
        let http_client = self.http_client.clone();
        let current_version = self.current_version.clone();

        tokio::spawn(async move {
            tokio::time::sleep(STARTUP_CHECK_DELAY).await;
            // 启动即查：读一次决定，不随循环迭代变化
            let check_on_startup = config.load_settings().global.updater.check_on_startup;
            if check_on_startup {
                if let Err(e) =
                    perform_update_check(&config, &status, &http_client, &current_version).await
                {
                    tracing::warn!("启动时更新检查失败: {e}");
                }
            }
            loop {
                // 每次迭代重新读取配置（支持运行时修改）
                let settings = config.load_settings().global.updater;
                // check_interval_hours 为 0 时禁用定时检查（仅保留启动时检查）
                if settings.check_interval_hours == 0 {
                    cancel.cancelled().await;
                    break;
                }
                let interval_secs = (settings.check_interval_hours as u64).saturating_mul(3600);
                let interval = std::time::Duration::from_secs(interval_secs.max(300)); // 最少 5 分钟
                // 每周期先执行一次检查，再等待下一间隔
                if let Err(e) =
                    perform_update_check(&config, &status, &http_client, &current_version).await
                {
                    tracing::warn!("定期更新检查失败: {e}");
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {},
                }
            }
        });
    }

    /// 手动触发版本检查（API 端点调用）
    ///
    /// 拉取清单 → 平台选择 → 版本比较；有新版本返回 `Some(UpdateInfo)`，否则 `None`。
    pub async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        let settings = self.config.load_settings().global.updater;
        let manifest =
            check::fetch_manifest(&self.http_client, &settings.release_source_url).await?;

        let pkg = match check::select_platform(&manifest) {
            Some(p) => p,
            None => {
                tracing::info!("当前平台无可用更新包: {}", check::CURRENT_PLATFORM_KEY);
                return Ok(None);
            }
        };

        if !check::compare_versions(&self.current_version, &manifest.version) {
            return Ok(None);
        }

        Ok(Some(UpdateInfo {
            current_version: self.current_version.to_string(),
            latest_version: manifest.version.to_string(),
            update_available: true,
            url: pkg.url.clone(),
            sha256: pkg.sha256.clone(),
            size: pkg.size,
            notes: manifest.changelog.clone(),
            release_date: manifest.release_date.clone(),
        }))
    }

    /// 暂存新二进制并触发助手进程
    ///
    /// 流程：并发互斥 → 拒绝登录中 → 下载校验 → 解压 → 写 `pending.json`
    /// → spawn 助手进程（助手等待本进程退出后完成替换与重启）。
    /// 调用方在收到 `Ok` 后应执行优雅关闭并使主进程退出，以放行助手替换。
    pub async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        if self
            .update_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(UpdaterError::UpdateInProgress);
        }

        // 前置检查：登录进行中则拒绝
        let snapshot = self.status.borrow();
        if snapshot.login_status == LoginStatus::Running {
            self.update_in_progress.store(false, Ordering::SeqCst);
            return Err(UpdaterError::LoginInProgress);
        }

        let result = self.download_stage_and_pending(info).await;
        // 无论成败均释放互斥，允许后续重试
        self.update_in_progress.store(false, Ordering::SeqCst);
        result?;

        self.spawn_helper()
    }

    /// 下载 → 校验 → 解压 → 写 pending.json
    async fn download_stage_and_pending(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        let staging_dir = self.base_path.join(apply::STAGING_DIR_NAME);
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(UpdaterError::StagingDirCreateFailed)?;

        let zip_path = download::download_and_verify(
            &self.http_client,
            info,
            &staging_dir,
            Some(&|percent| {
                self.status.merge(PartialSnapshot::Environment {
                    progress: Some(InstallProgress {
                        phase: "downloading_update".into(),
                        percent,
                        message: format!("下载更新 {}%", percent),
                    }),
                });
            }),
        )
        .await?;

        let staged =
            download::extract_to_staging(&zip_path, &staging_dir, &info.latest_version).await?;
        // 校验解压产物确实存在后再写 pending，避免写入无效的待应用更新
        if !staged.extracted_exe.exists() {
            return Err(UpdaterError::ExtractFailed("解压产物缺失可执行文件".into()));
        }
        tracing::info!(
            "更新包已暂存：版本 {}，可执行文件 {}",
            staged.version,
            staged.extracted_exe.display()
        );

        let target_exe = std::env::current_exe().map_err(UpdaterError::CurrentExeResolveFailed)?;
        let pending = PendingUpdate {
            version: info.latest_version.clone(),
            staging_dir: staging_dir.to_string_lossy().into_owned(),
            target_exe: target_exe.to_string_lossy().into_owned(),
            original_args: std::env::args().skip(1).collect(),
            // G13：helper 替换前据此复核 staging exe 完整性（空 = 降级跳过复核）
            sha256: info.sha256.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        apply::write_pending(&pending, &self.base_path)?;
        Ok(())
    }

    /// spawn 助手进程（不在此处退出主进程）
    fn spawn_helper(&self) -> Result<(), UpdaterError> {
        let current_exe = std::env::current_exe().map_err(UpdaterError::CurrentExeResolveFailed)?;
        let helper_path = current_exe
            .parent()
            .map(|p| p.join(apply::HELPER_EXE_NAME))
            .ok_or_else(|| {
                UpdaterError::HelperSpawnFailed(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "无法确定助手程序路径",
                ))
            })?;

        if !helper_path.exists() {
            return Err(UpdaterError::HelperSpawnFailed(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "更新助手程序缺失",
            )));
        }

        let pid = std::process::id();
        let staging_dir = self.base_path.join(apply::STAGING_DIR_NAME);
        let mut cmd = std::process::Command::new(&helper_path);
        cmd.arg("--apply-update")
            .arg("--pid")
            .arg(pid.to_string())
            .arg("--staging")
            .arg(&staging_dir)
            .arg("--base-path")
            .arg(&self.base_path);
        // U4：helper 内有多行 println，Windows 上隐藏控制台窗口避免闪黑窗
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.spawn().map_err(UpdaterError::HelperSpawnFailed)?;
        Ok(())
    }

    /// 启动时检测并应用待处理更新
    ///
    /// 若 `pending.json` 存在且 staging/extracted exe 完好，则直接 `self_replace`
    /// 替换当前运行中的 exe 并清理；否则清理残留并返回 `false`。
    ///
    /// F9：与手动 `apply_update` 统一走 `update_in_progress` 原子标记互斥——
    /// 后台路径抢不到标记说明手动"立即更新"正在进行（可能正在重写
    /// pending.json / 重复 spawn helper），此时跳过本次后台应用并记日志，
    /// pending.json 留待下次启动处理，不再依赖 sleep 错峰。
    pub async fn apply_pending_on_startup(&self) -> Result<bool, UpdaterError> {
        if !apply::has_pending_update(&self.base_path) {
            return Ok(false);
        }
        // F9：抢不到标记 = 手动更新正在进行 → 跳过（不清理、不替换）
        if self
            .update_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::info!("手动更新进行中，跳过后台待定更新应用（下次启动再试）");
            return Ok(false);
        }
        let result = self.apply_pending_locked().await;
        // 无论成败均释放互斥（后台应用为一次性启动动作，手动路径可继续）
        self.update_in_progress.store(false, Ordering::SeqCst);
        result
    }

    /// apply_pending_on_startup 的实际执行体（调用方已持有互斥标记）
    async fn apply_pending_locked(&self) -> Result<bool, UpdaterError> {
        let pending = match apply::read_pending(&self.base_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("读取 pending.json 失败，清理残留: {}", e);
                apply::cleanup_after_apply(&self.base_path).await;
                return Ok(false);
            }
        };

        let staging_dir = PathBuf::from(&pending.staging_dir);
        let extracted_exe = staging_dir.join("extracted").join(apply::EXE_NAME);

        if !extracted_exe.exists() {
            // staging 缺失，清理后继续正常启动
            apply::cleanup_after_apply(&self.base_path).await;
            return Ok(false);
        }

        // U3 二次校验：pending 版本不高于当前版本则跳过并清理（下载与启动之间的时间窗内
        // staging 产物或版本可能已过期/被替换）
        if let Ok(pending_ver) = Version::parse(&pending.version) {
            if pending_ver <= self.current_version {
                tracing::warn!(
                    "pending 版本 {pending_ver} 不高于当前 {}，跳过应用并清理",
                    self.current_version
                );
                apply::cleanup_after_apply(&self.base_path).await;
                return Ok(false);
            }
        }
        // 替换前备份当前 exe 到 config/.backup_exe，用于失败回滚
        let current_exe = std::env::current_exe().map_err(UpdaterError::CurrentExeResolveFailed)?;
        let backup_path = self.base_path.join(".backup_exe");
        if let Err(e) = std::fs::copy(&current_exe, &backup_path) {
            tracing::warn!("备份当前 exe 失败，跳过回滚保护: {}", e);
        }

        match self_replace::self_replace(extracted_exe.as_path()) {
            Ok(()) => {
                // 替换成功，删除备份并清理 staging
                let _ = std::fs::remove_file(&backup_path);
                apply::cleanup_after_apply(&self.base_path).await;
                tracing::info!("启动时已应用更新: v{}", pending.version);
                Ok(true)
            }
            Err(e) => {
                tracing::error!("启动时替换失败，回退旧版本: {}", e);
                // 从备份回滚当前 exe
                if backup_path.exists() {
                    if let Err(re) = std::fs::copy(&backup_path, &current_exe) {
                        tracing::error!(
                            "回滚失败: {}",
                            UpdaterError::RollbackFailed(re.to_string())
                        );
                    }
                    let _ = std::fs::remove_file(&backup_path);
                }
                apply::cleanup_after_apply(&self.base_path).await;
                Err(UpdaterError::SelfReplaceFailed(e.to_string()))
            }
        }
    }
}

/// 拉取清单并判断是否存在对当前版本"感兴趣"的更新；有则推送状态快照
async fn perform_update_check(
    config: &ConfigService,
    status: &StatusManager,
    http_client: &reqwest::Client,
    current_version: &Version,
) -> Result<(), UpdaterError> {
    let settings = config.load_settings().global.updater;
    let manifest = check::fetch_manifest(http_client, &settings.release_source_url).await?;
    if check::select_platform(&manifest).is_some()
        && check::compare_versions(current_version, &manifest.version)
    {
        status.merge(PartialSnapshot::Update { available: true });
        tracing::info!("发现新版本: {} → {}", current_version, manifest.version);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试用 UpdaterService（base_path = tempdir）
    async fn make_service(base_path: &std::path::Path) -> Arc<UpdaterService> {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let config = crate::config::ConfigService::new(base_path.to_path_buf(), tx)
            .await
            .expect("构造 ConfigService 失败");
        let status = Arc::new(StatusManager::new());
        UpdaterService::new(config, status, base_path.to_path_buf())
    }

    /// F9：无 pending.json 时直接跳过，且不遗留占用互斥标记
    #[tokio::test]
    async fn test_apply_pending_skips_without_pending() {
        let dir = tempfile::tempdir().unwrap();
        let svc = make_service(dir.path()).await;
        assert!(matches!(svc.apply_pending_on_startup().await, Ok(false)));
        assert!(
            !svc.update_in_progress.load(Ordering::SeqCst),
            "跳过路径不得遗留占用标记"
        );
    }

    /// F9：手动更新进行中（标记被占）时后台路径跳过，
    /// pending.json 与 staging 保持原样（不清理、不替换、不释放他人标记）
    #[tokio::test]
    async fn test_apply_pending_skips_when_update_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let svc = make_service(dir.path()).await;

        // 伪造合法待应用更新：版本更高 + staging exe 存在
        // （抢不到标记时二者均不应被触碰）
        let staging = dir.path().join("update/staging/extracted");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join(apply::EXE_NAME), b"fake-exe").unwrap();
        let pending = PendingUpdate {
            version: "999.0.0".into(),
            staging_dir: dir
                .path()
                .join("update/staging")
                .to_string_lossy()
                .into_owned(),
            target_exe: dir
                .path()
                .join("campus-auth.exe")
                .to_string_lossy()
                .into_owned(),
            original_args: vec![],
            sha256: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        apply::write_pending(&pending, dir.path()).unwrap();

        // 模拟手动"立即更新"持有互斥标记
        svc.update_in_progress.store(true, Ordering::SeqCst);
        let result = svc.apply_pending_on_startup().await;
        assert!(matches!(result, Ok(false)), "抢不到标记应跳过而非执行");
        // pending 与 staging 均未被清理
        assert!(apply::has_pending_update(dir.path()));
        assert!(staging.join(apply::EXE_NAME).exists());
        // 手动路径持有的标记未被后台路径释放
        assert!(svc.update_in_progress.load(Ordering::SeqCst));
    }

    /// F9：标记被占时手动 apply_update 立即拒绝（与后台路径同一互斥）
    #[tokio::test]
    async fn test_apply_update_rejected_when_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let svc = make_service(dir.path()).await;
        svc.update_in_progress.store(true, Ordering::SeqCst);
        let info = UpdateInfo {
            current_version: "5.0.0".into(),
            latest_version: "5.0.1".into(),
            update_available: true,
            url: "https://example.com/x.zip".into(),
            sha256: String::new(),
            size: None,
            notes: None,
            release_date: None,
        };
        assert!(matches!(
            svc.apply_update(&info).await,
            Err(UpdaterError::UpdateInProgress)
        ));
    }
}
