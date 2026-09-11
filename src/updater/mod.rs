//! 更新器业务模块
//!
//! 负责后台版本检查、流式下载 + SHA256 校验、staging 暂存、写入 `pending.json`
//! 并 spawn 助手进程（`campus-auth-helper`）完成 exe 替换与重启。
//!
//! 助手进程契约：读取 `<base_path>/update/pending.json`，等待主进程（PID 由
//! `--pid` 传入）退出后，将 `staging_dir/extracted/<EXE_NAME>` 复制到 `target_exe`
//! 并以其 `original_args` 重启，最后清理 staging 与 pending 标记。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use semver::Version;
use tokio_util::sync::CancellationToken;

use crate::config::ConfigService;
use crate::status::{InstallProgress, LoginStatus, PartialSnapshot, StatusManager};

mod apply;
pub(crate) mod check;
mod download;
pub mod error;

pub use apply::PendingUpdate;
pub use check::{PlatformPackage, ReleaseManifest};
pub use download::StagedUpdate;
pub use error::UpdaterError;

/// 后台检查任务启动前的延迟（等待核心启动完成）
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(5);

/// 上次检查状态文件路径（相对 base_path，与 staging 同级不被清理波及）
const LAST_CHECK_FILE_NAME: &str = "update/last_check.json";

/// 确认应用内更新可以安全覆盖当前 Worker，并返回明确目标目录。
///
/// Docker、系统包或显式外置 Worker 应由各自部署系统更新；若仍把发布包
/// overlay 到 `<base_path>/python_worker`，会造成主程序与实际执行 Worker 版本分裂。
fn self_update_worker_dir(base_path: &Path) -> Result<PathBuf, UpdaterError> {
    let bundled = base_path.join("python_worker");
    let resolved = crate::utils::paths::worker_project_dir(base_path);
    if bundled.is_dir() && crate::utils::paths::same_existing_path(&bundled, &resolved) {
        return Ok(bundled);
    }
    Err(UpdaterError::UnsupportedSelfUpdateLayout(format!(
        "运行时解析到 {}；应用内更新仅支持 {}",
        resolved.display(),
        bundled.display()
    )))
}

/// 上次更新检查结果（`update/last_check.json`，跨重启持久）
///
/// 手动"立即检查"与后台自动检查共用一份记录：成功时写 `latest_version` /
/// `has_update`，失败时仅写 `error`（时间照常刷新）。
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LastCheckState {
    /// 上次检查时间（UTC RFC3339，前端转本地时区展示）
    pub last_check_at: String,
    /// 是否发现更新（检查失败时为 false）
    pub has_update: bool,
    /// 远程最新版本号（检查失败时为空）
    #[serde(default)]
    pub latest_version: String,
    /// 上次检查失败原因（成功时为空）
    #[serde(default)]
    pub error: String,
    /// 远程发布是否缺少当前平台的安装包（区分"已是最新"与"无本平台包"）
    #[serde(default)]
    pub platform_unavailable: bool,
}

/// 将检查结果写入状态文件（best-effort：失败仅 warn，不影响检查流程本身）
fn record_last_check(base_path: &std::path::Path, state: &LastCheckState) {
    let path = base_path.join(LAST_CHECK_FILE_NAME);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("创建更新状态目录失败: {e}");
            return;
        }
    }
    if let Err(e) = crate::utils::io::atomic_write_json(&path, state) {
        tracing::warn!("写入上次检查状态失败: {e}");
    }
}

/// 以当前时刻构造检查记录（RFC3339 UTC）
fn last_check_now() -> LastCheckState {
    LastCheckState {
        last_check_at: chrono::Utc::now().to_rfc3339(),
        ..Default::default()
    }
}

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
    /// 远程发布缺少当前平台安装包（此时 has_update=false 且无下载信息）
    pub platform_unavailable: bool,
}

/// 更新器服务：封装版本检查、下载、暂存与助手替换
pub struct UpdaterService {
    /// 配置服务（读取 `global.updater` 段）
    config: Arc<ConfigService>,
    /// 状态管理器（推送 update_available / 下载进度）
    status: Arc<StatusManager>,
    /// 跟随系统/环境代理的客户端（未配置显式代理时的回退）
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
    /// 手动触发版本检查（网络路径同下载：显式代理优先，未配置跟随系统代理）；
    /// 有新版本返回 `Some(UpdateInfo)`。
    async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError>;
    /// 执行更新（下载 zip 到 staging 并触发助手替换）。
    async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError>;
    /// 读取上次检查状态（文件缺失或损坏返回 `None`）。
    fn last_check_state(&self) -> Option<LastCheckState>;
}

#[async_trait::async_trait]
impl UpdaterApi for UpdaterService {
    async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        UpdaterService::check_update(self).await
    }

    async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        UpdaterService::apply_update(self, info).await
    }

    fn last_check_state(&self) -> Option<LastCheckState> {
        UpdaterService::last_check_state(self)
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
        // 回退客户端跟随系统/环境代理（system-proxy feature 已启用）：
        // 显式代理未启用/构建失败时使用。连接超时与下载停滞超时同源配置，
        // 避免握手挂死（总超时已移除，见 download.rs）
        let http_client = reqwest::Client::builder()
            .connect_timeout(download::DOWNLOAD_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("默认 HTTP 客户端构建失败，回退无配置客户端: {e}");
                reqwest::Client::new()
            });

        Arc::new(Self {
            config,
            status,
            http_client,
            base_path,
            current_version,
            update_in_progress: AtomicBool::new(false),
        })
    }

    /// 更新网络路径客户端（检查与下载共用）
    ///
    /// 委托 [`effective_client_for`]：显式代理优先，未配置/构建失败回退系统代理。
    fn effective_client(&self) -> reqwest::Client {
        let settings = self.config.load_settings().global.updater;
        effective_client_for(&settings, self.http_client.clone())
    }

    /// 启动后台版本检查任务（循环：启动时检查一次，之后按 check_interval_hours 定时检查）
    ///
    /// 延迟 [`STARTUP_CHECK_DELAY`] 后拉取清单，发现更新则
    /// `merge(PartialSnapshot::Update { .. })`；失败静默忽略。
    /// `cancel` 用于优雅中止。
    ///
    /// 语义（U6 修复）：`check_on_startup` 只决定"启动是否立即检查一次"（循环外读一次），
    /// 循环内的周期检查不受其影响——否则关闭该开关会连定时检查一并消失。
    /// 关闭该开关后循环内不再补"启动首查"（旧实现的 `due_now = !check_on_startup`
    /// 会让开关形同虚设），首轮检查按 `check_interval_hours` 周期等待。
    ///
    /// 双查修复：循环改为"先等待再检查"。旧实现每轮"先查再睡"，与启动检查
    /// 相邻执行造成同一时刻 2×清单 + 2N×伴随 sha 拉取。
    ///
    /// `auto_check_enabled` 为总开关（设置页"自动检查更新"）：关闭后启动检查与
    /// 周期检查全部静默，仅保留手动"立即检查"；循环低频轮询该值，重新打开无需重启，
    /// 且由关到开的跃迁会立即补查一次（不等完整周期）。
    pub fn start_background_check(&self, cancel: CancellationToken) {
        let config = self.config.clone();
        let status = self.status.clone();
        // 回退客户端（跟随系统代理）；每次检查按当前配置构建实际客户端，
        // 运行时修改 use_proxy / proxy_url 无需重启即生效
        let fallback_client = self.http_client.clone();
        let current_version = self.current_version.clone();
        let base_path = self.base_path.clone();

        tokio::spawn(async move {
            tokio::time::sleep(STARTUP_CHECK_DELAY).await;
            // 启动即查：读一次决定，不随循环迭代变化
            let startup_settings = config.load_settings().global.updater;
            if startup_settings.auto_check_enabled && startup_settings.check_on_startup {
                let client = effective_client_for(&startup_settings, fallback_client.clone());
                if let Err(e) =
                    perform_update_check(&config, &status, &client, &current_version, &base_path)
                        .await
                {
                    log_check_failure("启动时", &e);
                    record_last_check(
                        &base_path,
                        &LastCheckState {
                            error: e.to_string(),
                            ..last_check_now()
                        },
                    );
                }
            }
            // 启动是否检查完全由 check_on_startup 决定（上方循环外分支）；
            // 循环内 due_now 仅用于"总开关由关到开"的立即补查
            let mut due_now = false;
            let mut prev_auto_enabled = startup_settings.auto_check_enabled;
            loop {
                // 每次迭代重新读取配置（支持运行时修改）
                let settings = config.load_settings().global.updater;
                // 总开关关闭：不自动检查，低频轮询配置等待重新打开
                if !settings.auto_check_enabled {
                    prev_auto_enabled = false;
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => continue,
                    }
                }
                // 总开关由关到开：立即补查一次，无需等待完整周期
                if !prev_auto_enabled {
                    due_now = true;
                }
                prev_auto_enabled = true;
                if !due_now {
                    if settings.check_interval_hours == 0 {
                        // 定时检查已禁用（启动检查已完成或未要求）：低频轮询配置，
                        // 运行时改回非 0 无需重启。原实现在此永久阻塞 cancelled()，
                        // 导致 0 → 非 0 的热改永远不生效（除非重启）。
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => continue,
                        }
                    }
                    let interval_secs = (settings.check_interval_hours as u64).saturating_mul(3600);
                    let interval = std::time::Duration::from_secs(interval_secs.max(300)); // 最少 5 分钟
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(interval) => {},
                    }
                }
                due_now = false;
                // 每周期等待结束后执行一次检查
                let client = effective_client_for(&settings, fallback_client.clone());
                if let Err(e) =
                    perform_update_check(&config, &status, &client, &current_version, &base_path)
                        .await
                {
                    log_check_failure("定期", &e);
                    record_last_check(
                        &base_path,
                        &LastCheckState {
                            error: e.to_string(),
                            ..last_check_now()
                        },
                    );
                }
            }
        });
    }

    /// 手动触发版本检查（API 端点调用）
    ///
    /// 拉取清单 → 平台选择 → 版本比较；有新版本返回 `Some(UpdateInfo)`，否则 `None`。
    /// 网络路径同下载：显式代理（use_proxy）优先，未配置跟随系统代理。
    /// 无论成败均刷新 `update/last_check.json`（设置页"上次检查时间"数据源）。
    pub async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        let settings = self.config.load_settings().global.updater;
        let client = self.effective_client();
        let manifest = match check::fetch_manifest_for_channel(
            &client,
            &settings.release_source_url,
            settings.channel,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                record_last_check(
                    &self.base_path,
                    &LastCheckState {
                        error: e.to_string(),
                        ..last_check_now()
                    },
                );
                return Err(e);
            }
        };

        let pkg = match check::select_platform(&manifest) {
            Some(p) => p,
            None => {
                // 手动检查路径的预期情况（远程发布可能没有当前平台安装包），仅 debug
                tracing::debug!("当前平台无可用更新包: {}", check::CURRENT_PLATFORM_KEY);
                record_last_check(
                    &self.base_path,
                    &LastCheckState {
                        latest_version: manifest.version.to_string(),
                        platform_unavailable: true,
                        ..last_check_now()
                    },
                );
                // 与后台检查同样 merge 快照：托盘"发现新版本"文案保持一致
                self.status.merge(PartialSnapshot::Update {
                    available: false,
                    progress: None,
                });
                // 返回带标记的结果（而非 Ok(None)）：让前端区分
                // "当前已是最新"与"远程无当前平台的安装包"
                return Ok(Some(UpdateInfo {
                    current_version: self.current_version.to_string(),
                    latest_version: manifest.version.to_string(),
                    update_available: false,
                    url: String::new(),
                    sha256: String::new(),
                    size: None,
                    notes: manifest.changelog.clone(),
                    release_date: manifest.release_date.clone(),
                    platform_unavailable: true,
                }));
            }
        };

        let has_update = check::compare_versions(&self.current_version, &manifest.version);
        record_last_check(
            &self.base_path,
            &LastCheckState {
                has_update,
                latest_version: manifest.version.to_string(),
                ..last_check_now()
            },
        );
        // 与后台检查同样 merge 快照：手动发现新版本后托盘菜单文本即时更新
        self.status.merge(PartialSnapshot::Update {
            available: has_update,
            progress: None,
        });
        if !has_update {
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
            platform_unavailable: false,
        }))
    }

    /// 读取上次检查状态（`update/last_check.json`；缺失/损坏返回 `None`）
    pub fn last_check_state(&self) -> Option<LastCheckState> {
        let path = self.base_path.join(LAST_CHECK_FILE_NAME);
        let content = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&content) {
            Ok(state) => Some(state),
            Err(e) => {
                tracing::warn!("上次检查状态文件解析失败，忽略: {e}");
                None
            }
        }
    }

    /// 暂存新二进制并触发助手进程
    ///
    /// 流程：已有 pending 则幂等返回 → 并发互斥 → 拒绝登录中 → 下载校验 →
    /// 解压 → 写 `pending.json` → spawn 助手进程（助手等待本进程退出后完成替换与重启）。
    /// 调用方在收到 `Ok` 后应执行优雅关闭并使主进程退出，以放行助手替换。
    pub async fn apply_update(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        // 已有待应用更新（本进程此前发起或上次会话遗留）：不重复下载，
        // 补唤醒 helper（上次 spawn 的 helper 等待本进程退出超时 60s 后可能已退出）
        // 并按成功返回——前端继续展示"更新已就绪，重启后生效"。
        // 互斥语义由 pending 文件承载：存在即"已暂存待重启"，进程内
        // AtomicBool 只防真正的并发下载窗口。
        if apply::has_pending_update(&self.base_path) {
            if let Err(e) = self.spawn_helper() {
                // 补唤醒失败不报错：关机时 ensure_helper_for_shutdown 会再次尝试
                tracing::warn!("待应用更新已存在，按需补唤醒 helper 失败（关机时会重试）: {e}");
            }
            return Ok(());
        }

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

        if let Err(e) = self.download_stage_and_pending(info).await {
            // 失败释放互斥，允许后续重试
            self.update_in_progress.store(false, Ordering::SeqCst);
            self.clear_update_progress();
            return Err(e);
        }
        // 下载暂存完成即"更新已就绪"：释放进程内互斥（后续重复请求由
        // pending 存在性幂等接管），spawn 失败同样释放——关机时
        // ensure_helper_for_shutdown 会按需补唤醒，允许重新发起。
        if let Err(e) = self.spawn_helper() {
            self.update_in_progress.store(false, Ordering::SeqCst);
            self.clear_update_progress();
            return Err(e);
        }
        self.update_in_progress.store(false, Ordering::SeqCst);
        self.clear_update_progress();
        Ok(())
    }

    /// 清空更新下载进度（下载结束/失败后调用，避免残留在状态快照中）
    fn clear_update_progress(&self) {
        let available = self.status.borrow().update_available;
        self.status.merge(PartialSnapshot::Update {
            available,
            progress: None,
        });
    }

    /// 下载 → 校验 → 解压 → 写 pending.json
    async fn download_stage_and_pending(&self, info: &UpdateInfo) -> Result<(), UpdaterError> {
        let worker_target_dir = self_update_worker_dir(&self.base_path)?;
        let staging_dir = self.base_path.join(apply::STAGING_DIR_NAME);
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(UpdaterError::StagingDirCreateFailed)?;

        let download_client = self.effective_client();
        let zip_path = download::download_and_verify(
            &download_client,
            info,
            &staging_dir,
            Some(&|percent| {
                // 进度走 Update 自有通道（此前蹭 Environment 通道，与环境安装
                // 进度互相覆盖且无消费方）；available 置 true：正在应用的更新必然可用
                self.status.merge(PartialSnapshot::Update {
                    available: true,
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
        // 修复：pending 的 sha256 应为解压后 exe 的哈希，而非 zip 的哈希。
        // 之前直接克隆 info.sha256（zip sha）导致 helper 对 extracted_exe 的复核恒失败（P0 阻断）。
        // 现在下载阶段已校验 zip 完整性，此处额外计算 exe sha 存入 pending 供 helper 二次复核。
        let exe_sha256 = tokio::task::spawn_blocking({
            let exe_path = staged.extracted_exe.clone();
            move || crate::utils::io::file_sha256(&exe_path)
        })
        .await
        .map_err(|e| UpdaterError::ExtractFailed(format!("计算 exe SHA 失败: {e}")))?
        .map_err(|e| UpdaterError::ExtractFailed(format!("计算 exe SHA 失败: {e}")))?;
        let pending = PendingUpdate {
            version: info.latest_version.clone(),
            staging_dir: staging_dir.to_string_lossy().into_owned(),
            target_exe: target_exe.to_string_lossy().into_owned(),
            worker_target_dir: worker_target_dir.to_string_lossy().into_owned(),
            original_args: std::env::args().skip(1).collect(),
            sha256: exe_sha256,
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
            // 显式传 target：让 helper 的 CLI 分支在生产路径上真正生效，
            // pending.json 的同名字段降级为回退（helper 侧仍会与推导值比对）
            .arg("--target")
            .arg(&current_exe)
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
    /// 关机时补唤醒 helper（幂等 best-effort）
    ///
    /// 场景：`apply_update` 时 `spawn_helper` 失败（如 helper.exe 被占用/杀软拦截），
    /// 但 `pending.json` 已落盘，主进程随后收到 `shutdown` 退出后无人替换，下次启动
    /// 只能走 `self_replace` 兜底产生 `.__relocated__.exe`。此处在优雅关闭入口再次
    /// 尝试 `spawn_helper`，确保至少有一个 helper 在等待本 PID 退出。
    /// 重复 spawn 的双 helper 由 `<base>/update/helper.lock` 文件锁互斥（helper
    /// 启动即抢锁，后到者安静退出），且 helper 侧有"目标已是新版内容"的幂等跳过。
    pub(crate) fn ensure_helper_for_shutdown(&self) {
        if !apply::has_pending_update(&self.base_path) {
            return;
        }
        // 校验 staging 实物，避免 pending 残留但文件已删时无意义 spawn
        match apply::read_pending(&self.base_path) {
            Ok(p) => {
                let exe = PathBuf::from(&p.staging_dir)
                    .join("extracted")
                    .join(apply::EXE_NAME);
                if !exe.exists() {
                    tracing::warn!("关机时 pending 存在但 staging exe 缺失，跳过 helper 唤醒");
                    return;
                }
            }
            Err(e) => {
                tracing::warn!("关机时读取 pending.json 失败，跳过 helper 唤醒: {e}");
                return;
            }
        }
        match self.spawn_helper() {
            Ok(()) => tracing::info!("关机时已补唤醒 helper，等待主进程退出后替换"),
            Err(e) => tracing::warn!("关机时补唤醒 helper 失败，下次启动走 self_replace 兜底: {e}"),
        }
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
            // 无 pending 时顺带清理长期残留的 staging（用户点了"立即更新"却
            // 长期不重启时，旧版本压缩包会一直堆积在 update/staging/）
            self.cleanup_stale_staging().await;
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

    /// 清理超过 [`STALE_STAGING_AGE`] 未变动的 staging 残留（best-effort）
    ///
    /// staging 只在与 pending.json 配对时才有意义；目录最后修改时间久远
    /// 说明是历史更新遗留（近期产物可能属于进行中的下载，保守保留）。
    async fn cleanup_stale_staging(&self) {
        const STALE_STAGING_AGE: Duration = Duration::from_secs(3 * 24 * 3600);
        let staging = self.base_path.join(apply::STAGING_DIR_NAME);
        let Ok(meta) = tokio::fs::metadata(&staging).await else {
            return;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= STALE_STAGING_AGE);
        if !stale {
            return;
        }
        tracing::info!("清理超过 3 天未变动的 staging 残留: {}", staging.display());
        if let Err(e) = tokio::fs::remove_dir_all(&staging).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("清理 staging 残留失败: {e}");
            }
        }
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
        let target_exe = PathBuf::from(&pending.target_exe);
        // 替换与备份都基于运行时取到的 current_exe，提前解析供校验与后续复用
        let current_exe = std::env::current_exe().map_err(UpdaterError::CurrentExeResolveFailed)?;
        // G13 对齐 helper 的防篡改基线：
        // - staging 是 remove_dir_all 的目标，必须锁在 base 之内，否则被篡改的
        //   pending 能把任意目录变成清理对象；
        // - target 的合法值恒等于「当前进程自身」（self_replace 替换的就是
        //   current_exe），与 base_path 无关，故直接与 current_exe 比对——
        //   比"位于 base 内"更强，且允许 --base-path / CAMPUS_AUTH_BASE_PATH
        //   与 exe 目录分离（旧逻辑下 target 恒越界，更新会被静默作废）。
        if !is_pending_path_valid(&staging_dir, &target_exe, &self.base_path, &current_exe) {
            tracing::error!(
                staging = %staging_dir.display(),
                target = %target_exe.display(),
                "pending 路径校验失败（staging 不在 base 内 / target 非当前进程），已拒绝应用并清理"
            );
            apply::cleanup_after_apply(&self.base_path).await;
            return Ok(false);
        }
        // 缺失 SHA256 直接拒绝（与下载/helper 一致，不降级）
        if pending.sha256.is_empty() {
            tracing::error!("pending 缺失 SHA256，已拒绝应用并清理");
            apply::cleanup_after_apply(&self.base_path).await;
            return Ok(false);
        }
        let extracted_exe = staging_dir.join("extracted").join(apply::EXE_NAME);

        if !extracted_exe.exists() {
            // staging 缺失，清理后继续正常启动
            apply::cleanup_after_apply(&self.base_path).await;
            return Ok(false);
        }
        // 复核 staging exe 摘要（与 helper 同逻辑）
        match crate::utils::io::file_sha256(&extracted_exe) {
            Ok(actual) if actual.eq_ignore_ascii_case(&pending.sha256) => {}
            Ok(actual) => {
                tracing::error!(
                    expected = %pending.sha256, %actual,
                    "staging SHA256 不匹配，已拒绝应用并清理"
                );
                apply::cleanup_after_apply(&self.base_path).await;
                return Ok(false);
            }
            Err(e) => {
                tracing::error!("计算 staging SHA256 失败，已拒绝应用并清理: {e}");
                apply::cleanup_after_apply(&self.base_path).await;
                return Ok(false);
            }
        }

        // U3 二次校验：pending 版本不高于当前版本则跳过并清理（下载与启动之间的时间窗内
        // staging 产物或版本可能已过期/被替换）；版本号无法解析同样拒绝——
        // 故障模式须 fail-closed，不给被篡改的 pending 留静默放行通道
        let pending_ver = match Version::parse(&pending.version) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "pending 版本号无法解析（{}）：{e}，拒绝应用并清理",
                    pending.version
                );
                apply::cleanup_after_apply(&self.base_path).await;
                return Ok(false);
            }
        };
        if pending_ver <= self.current_version {
            tracing::warn!(
                "pending 版本 {pending_ver} 不高于当前 {}，跳过应用并清理",
                self.current_version
            );
            apply::cleanup_after_apply(&self.base_path).await;
            return Ok(false);
        }
        let backup_path = self.base_path.join(".backup_exe");
        if let Err(e) = std::fs::copy(&current_exe, &backup_path) {
            tracing::warn!("备份当前 exe 失败，跳过回滚保护: {}", e);
        }

        match self_replace::self_replace(extracted_exe.as_path()) {
            Ok(()) => {
                // 替换成功，删除备份并清理 staging
                if let Err(e) = std::fs::remove_file(&backup_path) {
                    tracing::debug!("删除更新前 exe 备份失败（忽略）: {e}");
                }
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

/// pending 路径校验（G13 防篡改基线，纯函数便于单测）
///
/// - `staging_dir` 是 `remove_dir_all` 的目标，必须锁在 `base_path` 之内，
///   否则被篡改的 pending 能把任意目录变成清理对象；
/// - `target_exe` 的合法值恒等于「当前进程自身」（`self_replace` 替换的就是
///   `current_exe`），与 `base_path` 无关——直接比对真实路径既比"位于 base 内"
///   更强，也允许 `--base-path` / `CAMPUS_AUTH_BASE_PATH` 与 exe 目录分离
///   （旧逻辑下 target 恒越界，更新会被静默作废）。
fn is_pending_path_valid(
    staging_dir: &std::path::Path,
    target_exe: &std::path::Path,
    base_path: &std::path::Path,
    current_exe: &std::path::Path,
) -> bool {
    is_within_base(staging_dir, base_path)
        && crate::utils::paths::same_existing_path(target_exe, current_exe)
}

/// 校验路径位于 base_path 之内（与 helper 同逻辑，防 pending 篡改逃逸）
///
/// 双方 canonicalize 后做前缀比较；任一不存在均视为不合法。
fn is_within_base(path: &std::path::Path, base_path: &std::path::Path) -> bool {
    let (Ok(canonical), Ok(base_canonical)) = (path.canonicalize(), base_path.canonicalize())
    else {
        return false;
    };
    canonical.starts_with(&base_canonical)
}

/// 代理地址脱敏：仅保留 scheme + host——代理 URL 可能内嵌 `user:pass` 凭据，
/// 整串入日志会泄露凭证
fn sanitize_proxy_url(proxy_url: &str) -> String {
    let (scheme, rest) = match proxy_url.split_once("://") {
        Some((s, r)) => (s, r),
        None => return proxy_url.to_string(),
    };
    match rest.split_once('@') {
        Some((_credentials, host)) => format!("{scheme}://{host}"),
        None => proxy_url.to_string(),
    }
}

/// 构建走显式代理的 HTTP 客户端
///
/// 每次调用新建（Client 构造纯配置无 I/O，开销可忽略）；检查/下载为低频操作，
/// 连接池复用收益有限。仅接受 http/https 代理地址。
fn build_proxied_client(proxy_url: &str) -> Result<reqwest::Client, String> {
    let lower = proxy_url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!(
            "代理地址必须是 http:// 或 https:// 开头：{proxy_url}"
        ));
    }
    let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| e.to_string())?;
    reqwest::Client::builder()
        .proxy(proxy)
        .connect_timeout(download::DOWNLOAD_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

/// 按更新器配置选择客户端（后台检查任务用，与 [`UpdaterService::effective_client`] 同语义）
///
/// `use_proxy` 开启且代理地址有效 → 显式代理客户端；否则返回 `fallback`
/// （跟随系统代理的共享客户端）。
fn effective_client_for(
    settings: &crate::config::UpdaterSettings,
    fallback: reqwest::Client,
) -> reqwest::Client {
    if settings.use_proxy {
        let proxy_url = settings.resolved_proxy_url();
        if !proxy_url.is_empty() {
            match build_proxied_client(&proxy_url) {
                Ok(c) => return c,
                // 脱敏后仅记录 scheme+host，避免 user:pass 凭据进日志
                Err(e) => tracing::warn!(
                    "构建代理客户端失败（{}）：{e}，回退系统代理",
                    sanitize_proxy_url(&proxy_url)
                ),
            }
        }
    }
    fallback
}

/// 拉取清单并判断是否存在对当前版本"感兴趣"的更新；有则推送状态快照
///
/// 成功时同步刷新 `update/last_check.json`（失败由调用方记录 error 态）。
async fn perform_update_check(
    config: &ConfigService,
    status: &StatusManager,
    http_client: &reqwest::Client,
    current_version: &Version,
    base_path: &std::path::Path,
) -> Result<(), UpdaterError> {
    let settings = config.load_settings().global.updater;
    let manifest = check::fetch_manifest_for_channel(
        http_client,
        &settings.release_source_url,
        settings.channel,
    )
    .await?;
    let platform_available = check::select_platform(&manifest).is_some();
    let has_update =
        platform_available && check::compare_versions(current_version, &manifest.version);
    record_last_check(
        base_path,
        &LastCheckState {
            has_update,
            latest_version: manifest.version.to_string(),
            platform_unavailable: !platform_available,
            ..last_check_now()
        },
    );
    if has_update {
        status.merge(PartialSnapshot::Update {
            available: true,
            progress: None,
        });
        tracing::info!("发现新版本: {} → {}", current_version, manifest.version);
    } else {
        // 无更新时显式清 false：同进程内一次置 true 后若不清除，
        // 快照会跨"无更新"检查残留（重启归零，但长跑进程会一直误报）
        status.merge(PartialSnapshot::Update {
            available: false,
            progress: None,
        });
    }
    Ok(())
}

/// 更新检查失败的分级日志：远程发布没有当前平台的安装包或校验缺失
/// 均为预期情况（未发布该平台包 / 未附校验文件），均按 info 汇报；其余
/// （网络 / 解析 / 限流）才是真异常，记 warn。
fn log_check_failure(stage: &str, e: &UpdaterError) {
    match e {
        UpdaterError::PlatformNotAvailable(_) => {
            tracing::info!("更新检查（{stage}）：远程发布无当前平台的安装包，跳过");
        }
        UpdaterError::NoMatchingRelease => {
            tracing::info!("更新检查（{stage}）：远程无符合通道的发布，跳过");
        }
        UpdaterError::ManifestFetchFailed(_) | UpdaterError::ManifestParseFailed(_) => {
            tracing::info!("更新检查（{stage}）：清单不可用，跳过: {e}");
        }
        other => tracing::warn!("更新检查（{stage}）失败: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 应用内更新只允许覆盖 base_path 自带的 Worker，防止外置部署版本分裂。
    #[test]
    fn test_self_update_worker_dir_requires_bundled_worker() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            self_update_worker_dir(dir.path()),
            Err(UpdaterError::UnsupportedSelfUpdateLayout(_))
        ));

        let bundled = dir.path().join("python_worker");
        std::fs::create_dir_all(&bundled).unwrap();
        assert_eq!(
            self_update_worker_dir(dir.path()).unwrap(),
            bundled,
            "存在随程序分发的 Worker 时应明确返回该目录"
        );
    }

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
            worker_target_dir: dir
                .path()
                .join("python_worker")
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
            platform_unavailable: false,
        };
        assert!(matches!(
            svc.apply_update(&info).await,
            Err(UpdaterError::UpdateInProgress)
        ));
    }

    /// #2：pending 已存在时 apply_update 幂等返回成功（不占互斥、不重复下载）
    #[tokio::test]
    async fn test_apply_update_idempotent_when_pending_exists() {
        let dir = tempfile::tempdir().unwrap();
        let svc = make_service(dir.path()).await;

        // 伪造待应用更新（helper 不存在，spawn 会失败但幂等路径只 warn 不报错）
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
            worker_target_dir: dir
                .path()
                .join("python_worker")
                .to_string_lossy()
                .into_owned(),
            original_args: vec![],
            sha256: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        apply::write_pending(&pending, dir.path()).unwrap();

        let info = UpdateInfo {
            current_version: "5.0.0".into(),
            latest_version: "5.0.1".into(),
            update_available: true,
            url: "https://example.com/x.zip".into(),
            sha256: String::new(),
            size: None,
            notes: None,
            release_date: None,
            platform_unavailable: false,
        };
        assert!(matches!(svc.apply_update(&info).await, Ok(())));
        assert!(
            !svc.update_in_progress.load(Ordering::SeqCst),
            "幂等路径不得遗留互斥标记"
        );
        assert!(apply::has_pending_update(dir.path()), "pending 不被破坏");
    }

    /// G13 基线：staging 在 base 内 + target 为当前进程 → 放行；任一不满足 → 拒绝
    ///
    /// 关键回归：`current_exe` 天然不在测试用 base（tempdir）之内，正是
    /// `--base-path` 与 exe 目录分离的场景——旧逻辑（target 也要求在 base 内）
    /// 会在此拒绝并清理，导致下载作废。
    #[test]
    fn test_is_pending_path_valid() {
        let base = tempfile::tempdir().unwrap();
        let staging = base.path().join("update").join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let current_exe = std::env::current_exe().unwrap();

        // 合法：staging 在 base 内 + target 为当前进程（base 与 exe 目录分离亦成立）
        assert!(is_pending_path_valid(
            &staging,
            &current_exe,
            base.path(),
            &current_exe
        ));

        // staging 存在但位于 base 之外 → 拒绝
        let outside = tempfile::tempdir().unwrap();
        assert!(!is_pending_path_valid(
            outside.path(),
            &current_exe,
            base.path(),
            &current_exe
        ));
        // staging 不存在 → 拒绝
        assert!(!is_pending_path_valid(
            &base.path().join("missing-staging"),
            &current_exe,
            base.path(),
            &current_exe
        ));
        // target 非当前进程（被篡改指向同目录内其他文件）→ 拒绝
        let other_exe = base.path().join("other.exe");
        std::fs::write(&other_exe, b"x").unwrap();
        assert!(!is_pending_path_valid(
            &staging,
            &other_exe,
            base.path(),
            &current_exe
        ));
    }
}
