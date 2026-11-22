//! 更新器错误类型
//!
//! 使用 `thiserror` 定义强类型错误枚举，覆盖清单拉取、版本比较、
//! 流式下载校验、解压、pending 标记读写、助手进程启动与替换等全部失败路径。

use thiserror::Error;

/// 更新器统一错误类型
#[derive(Debug, Error)]
pub enum UpdaterError {
    /// 路由器版交由系统服务管理升级，避免助手与 procd 同时启动后继进程。
    #[error("OpenWrt 版请先停止 campus-auth 服务，再手动替换安装包并启动服务")]
    ServiceManaged,
    /// 独立更新任务异常退出（panic / 被中止），载荷为中文描述 + 原始 JoinError。
    #[error("{0}")]
    OperationFailed(String),
    /// 拉取发布清单（latest.json）失败（网络/超时/非 2xx）
    #[error("拉取发布清单失败: {0}")]
    ManifestFetchFailed(#[source] reqwest::Error),

    /// 发布清单 JSON 解析失败
    #[error("发布清单解析失败: {0}")]
    ManifestParseFailed(#[source] serde_json::Error),

    /// 当前平台无可用下载包
    #[error("当前平台无可用更新包: {0}")]
    PlatformNotAvailable(String),

    /// 发布含当前平台包但缺失 `.sha256` 伴随文件（无法构造可信更新）
    ///
    /// 与 [`UpdaterError::PlatformNotAvailable`] 区分：前者是"没有包"，
    /// 本变体是"有包但发布流程漏带校验文件"，诊断方向不同。
    #[error("发布中当前平台包缺失 SHA256 校验文件，已拒绝更新")]
    ChecksumUnavailable,

    /// 发布清单签名缺失或校验失败（嵌公钥构建的严格校验模式）
    ///
    /// 仅编译期嵌入了签名公钥的构建会走到这里（验证策略见
    /// `updater::signature` 模块 doc）：清单没有携带 `signature` 字段，或签名
    /// 与嵌入的发布方公钥不匹配。fail-closed：拒绝接受该清单，不降级为无签名
    /// 流程——清单源被劫持时换包补假摘要的通道被签名彻底封死。
    #[error("发布清单签名校验失败，已拒绝该清单（缺失签名或签名与发布方公钥不匹配）")]
    ManifestSignatureInvalid,

    /// 嵌公钥构建下发布未附带已签名的清单文件（GitHub Release 路径）
    ///
    /// 嵌公钥构建的信任锚只有发布方签名（见 `updater::signature` 模块 doc 的
    /// 「同源同批」）：发布缺 `latest.json` 签名资产时，回退逐资产合成清单等于
    /// 退回纯 TLS 信任，两种信任模型不可共存，故 fail-closed 拒绝而非降级。
    /// 未嵌公钥构建不产生本错误（资产缺失照旧回退合成清单，与现状一致）。
    #[error("发布缺少已签名的清单文件，已拒绝回退到未签名合成清单")]
    SignedManifestMissing,

    /// 清单版本低于本机已见过的最高版本（版本回滚保护）
    ///
    /// 版本单调保护只约束**清单驱动的自动流**：同一更新通道内，清单版本低于
    /// 已记录的最高已见版本、且仍高于当前版本（可能驱动一次降级安装）时拒绝。
    /// 用户主动切换更新通道（主动降级）不受影响——记录按通道隔离，切换后按
    /// 新通道重建记录；低于当前版本的清单不构成可执行动作，不在此拒绝。
    #[error("版本回滚被拒绝：清单版本 {manifest} 低于本机已见过的最高版本 {seen}")]
    VersionRollbackRejected {
        /// 本机已见过的最高清单版本。
        seen: String,
        /// 被拒绝的清单版本。
        manifest: String,
    },

    /// Releases 列表中没有符合更新通道的发布（列表为空 / 全为草稿 / tag 无法解析）
    #[error("Releases 列表中未找到符合更新通道的发布")]
    NoMatchingRelease,

    /// 远程版本号解析失败
    #[error("版本号解析失败: {0}")]
    VersionParseFailed(#[source] semver::Error),

    /// 更新包下载失败（网络中断/超时/非 2xx）
    #[error("更新包下载失败: {0}")]
    DownloadFailed(#[source] reqwest::Error),

    /// 下载停滞：等待响应头或相邻数据块超过上限时间仍未收到数据
    #[error("下载停滞：{idle_secs} 秒内未收到数据（已接收 {received_bytes} 字节）")]
    DownloadStalled {
        /// 无数据传输的超时秒数。
        idle_secs: u64,
        /// 超时前已经接收的字节数。
        received_bytes: u64,
    },

    /// SHA256 校验不匹配
    #[error("校验和不匹配（预期 {expected}，实际 {actual}）")]
    ChecksumMismatch {
        /// 期望的摘要。
        expected: String,
        /// 实际计算得到的摘要。
        actual: String,
    },

    /// 更新包缺失 SHA256（已拒绝安装，不再降级信任 HTTPS）
    #[error("更新包缺失 SHA256 校验值，已拒绝安装")]
    MissingChecksum,
    /// 下载包超过允许大小
    #[error("更新包超过大小上限 {limit} 字节")]
    DownloadTooLarge {
        /// 允许的最大字节数。
        limit: u64,
    },

    /// 手动选择的安装包版本不高于当前版本
    ///
    /// 手动「选择安装包」路径专用：版本号来自解压产物 exe 中的真实版本（PE
    /// VERSIONINFO），不依赖远程清单。此时无更新可应用，也不能像本地包复用那样
    /// 静默忽略——要明确告知用户"这个包用不上"。
    #[error("安装包版本 {version} 不高于当前版本，无需安装")]
    PackageNotNewer {
        /// 安装包版本。
        version: String,
    },

    /// 无法从安装包中识别版本号（手动「选择安装包」路径）
    ///
    /// 版本闸门需要包内真实版本（Windows PE VERSIONINFO）；非 Windows 产物、
    /// 缺版本资源或构建流程未嵌入版本信息的包都会走到这里。版本号未知的包写进
    /// pending 只会被 helper 的版本闸门拒绝，徒留一个永远无法应用的待定更新，
    /// 故必须 fail-closed 并给出可执行的补救建议。
    #[error("无法从安装包中识别版本号，请使用本项目的打包脚本（build.ps1）构建后再试")]
    VersionUnrecognized,

    /// 待应用更新标记存在但暂存内容已失效（pending 不可读 / staging 产物缺失）
    ///
    /// 重复发起「立即更新」时若只看到 pending.json 就补唤醒 helper 并报成功，
    /// 前端会展示"更新已就绪"而实际替换必然失败。此时应清理失效现场并要求
    /// 用户重新检查、重新下载安装。
    #[error("更新暂存已失效，请重新检查并安装")]
    StalePending,

    /// zip 解压失败（损坏/格式错误/路径穿越）
    ///
    /// 载荷是「动作 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装），Display 直接透出。
    #[error("{0}")]
    ExtractFailed(String),

    /// staging 目录创建失败
    ///
    /// 载荷是「动作 + 路径 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装），`ErrorKind` 原样保留。
    #[error("{0}")]
    StagingDirCreateFailed(#[source] std::io::Error),

    /// pending.json / 暂存包落盘写失败
    ///
    /// 载荷是「动作 + 路径 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装），`ErrorKind` 原样保留。
    #[error("{0}")]
    PendingWriteFailed(#[source] std::io::Error),

    /// pending.json 读取失败
    ///
    /// 载荷是「动作 + 路径 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装），`ErrorKind` 原样保留。
    #[error("{0}")]
    PendingReadFailed(#[source] std::io::Error),

    /// 助手进程启动失败
    ///
    /// 载荷是「动作 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装，固定原因场景为自述中文文案）。
    #[error("{0}")]
    HelperSpawnFailed(#[source] std::io::Error),

    /// 已有更新正在下载（进程内并发互斥窗口）
    #[error("更新正在进行中，请稍后再试")]
    UpdateInProgress,

    /// 有登录任务进行中，拒绝更新
    #[error("登录任务进行中，无法更新")]
    LoginInProgress,

    /// 更新已被取消（程序正在卸载）
    ///
    /// `UpdaterService::cancel_pending_update` 落这个标记后：在途的下载即使跑完也不再写
    /// `pending.json`（否则用户刚卸载的程序会被更新助手在退出时装回来），此后任何更新
    /// 入口一律拒绝。属调用时序冲突，不是服务端故障。
    #[error("更新已取消（程序正在卸载）")]
    Cancelled,

    /// 替换可执行文件失败（self-replace 路径）
    #[error("替换可执行文件失败: {0}")]
    SelfReplaceFailed(String),

    /// URL 非 HTTPS，安全策略拒绝
    #[error("仅允许 HTTPS 下载源: {0}")]
    HttpsRequired(String),

    /// 无法确定当前可执行文件路径
    ///
    /// 载荷是「动作 + 失败类别 + 原始错误」的中文文案（构造点经
    /// `utils::io_human` 包装）。
    #[error("{0}")]
    CurrentExeResolveFailed(#[source] std::io::Error),

    /// 当前 Worker 来自外部或只读布局，不能由便携版更新器安全覆盖
    #[error("当前 Python Worker 不在程序数据目录内，无法使用应用内自更新: {0}")]
    UnsupportedSelfUpdateLayout(String),

    /// GitHub API 速率限制（429），需等待后重试
    #[error("请求过于频繁，请在 {retry_after} 秒后重试")]
    RateLimited {
        /// 建议等待后再试的秒数。
        retry_after: u64,
    },
}
