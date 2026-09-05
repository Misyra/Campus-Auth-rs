//! 更新器错误类型
//!
//! 使用 `thiserror` 定义强类型错误枚举，覆盖清单拉取、版本比较、
//! 流式下载校验、解压、pending 标记读写、助手进程启动与替换等全部失败路径。

use thiserror::Error;

/// 更新器统一错误类型
#[derive(Debug, Error)]
pub enum UpdaterError {
    /// 拉取发布清单（latest.json）失败（网络/超时/非 2xx）
    #[error("拉取发布清单失败: {0}")]
    ManifestFetchFailed(#[source] reqwest::Error),

    /// 发布清单 JSON 解析失败
    #[error("发布清单解析失败: {0}")]
    ManifestParseFailed(#[source] serde_json::Error),

    /// 当前平台无可用下载包
    #[error("当前平台无可用更新包: {0}")]
    PlatformNotAvailable(String),

    /// 远程版本号解析失败
    #[error("版本号解析失败: {0}")]
    VersionParseFailed(#[source] semver::Error),

    /// 更新包下载失败（网络中断/超时/非 2xx）
    #[error("更新包下载失败: {0}")]
    DownloadFailed(#[source] reqwest::Error),

    /// 下载停滞：等待响应头或相邻数据块超过上限时间仍未收到数据
    #[error("下载停滞：{idle_secs} 秒内未收到数据（已接收 {received_bytes} 字节）")]
    DownloadStalled { idle_secs: u64, received_bytes: u64 },

    /// SHA256 校验不匹配
    #[error("校验和不匹配（预期 {expected}，实际 {actual}）")]
    ChecksumMismatch { expected: String, actual: String },

    /// 更新包缺失 SHA256（已拒绝安装，不再降级信任 HTTPS）
    #[error("更新包缺失 SHA256 校验值，已拒绝安装")]
    MissingChecksum,
    /// 下载包超过允许大小
    #[error("更新包超过大小上限 {limit} 字节")]
    DownloadTooLarge { limit: u64 },

    /// zip 解压失败（损坏/格式错误/路径穿越）
    #[error("解压失败: {0}")]
    ExtractFailed(String),

    /// staging 目录创建失败
    #[error("创建 staging 目录失败: {0}")]
    StagingDirCreateFailed(#[source] std::io::Error),

    /// pending.json 写入失败
    #[error("写入 pending.json 失败: {0}")]
    PendingWriteFailed(#[source] std::io::Error),

    /// pending.json 读取失败
    #[error("读取 pending.json 失败: {0}")]
    PendingReadFailed(#[source] std::io::Error),

    /// 助手进程启动失败
    #[error("启动更新助手失败: {0}")]
    HelperSpawnFailed(#[source] std::io::Error),

    /// 已有更新正在进行（AtomicBool 互斥）
    #[error("更新正在进行中，请稍后再试")]
    UpdateInProgress,

    /// 有登录任务进行中，拒绝更新
    #[error("登录任务进行中，无法更新")]
    LoginInProgress,

    /// 替换可执行文件失败（self-replace 路径）
    #[error("替换可执行文件失败: {0}")]
    SelfReplaceFailed(String),

    /// 回滚操作失败（仅记录，不中断流程）
    #[error("回滚失败: {0}")]
    RollbackFailed(String),

    /// URL 非 HTTPS，安全策略拒绝
    #[error("仅允许 HTTPS 下载源: {0}")]
    HttpsRequired(String),

    /// 无法确定当前可执行文件路径
    #[error("无法确定当前可执行文件路径: {0}")]
    CurrentExeResolveFailed(#[source] std::io::Error),

    /// GitHub API 速率限制（429），需等待后重试
    #[error("请求过于频繁，请在 {retry_after} 秒后重试")]
    RateLimited { retry_after: u64 },
}
