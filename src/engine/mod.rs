//! 调度引擎：命令循环 + tokio::select! 驱动网络监测和登录调度

pub mod commands;
pub mod run_loop;
pub mod slot;

pub use commands::{EngineCommand, ProbeDetails, ProfileSwitchSource, TestNetworkResult};
pub use slot::EngineSlot;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigService, ProfileService};
use crate::login::LoginOrchestrator;
use crate::monitor::MonitorService;
use crate::network::detect::NetworkDetect;
use crate::status::StatusManager;

/// mpsc channel 容量
pub const CMD_CHANNEL_CAPACITY: usize = 64;
/// 网络检查默认间隔（秒）
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 300;
/// Profile 切换检测默认间隔（秒）
pub const DEFAULT_PROFILE_CHECK_INTERVAL_SECS: u64 = 180;
/// 无事件时最大休眠时间（秒）
pub const MAX_IDLE_SLEEP_SECS: u64 = 5;
/// Engine panic 后最大重启次数
pub const MAX_RESTART_ATTEMPTS: u32 = 3;
/// 重启间隔（秒）
pub const RESTART_DELAY_SECS: u64 = 5;
/// profile_check_interval 配置下限（秒）
pub const PROFILE_CHECK_INTERVAL_MIN: u64 = 60;
/// profile_check_interval 配置上限（秒）
pub const PROFILE_CHECK_INTERVAL_MAX: u64 = 600;

/// 引擎相关错误
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// channel 已满（try_send 失败）
    #[error("引擎命令通道已满，请稍后重试")]
    ChannelFull,

    /// channel 已关闭（Engine task 已退出）
    #[error("引擎已关闭")]
    ChannelClosed,

    /// 网络探测失败（内部错误）
    #[error("网络探测执行失败: {0}")]
    ProbeError(String),

    /// 网络测试等待回复超时（EngineSlot::test_network 30s 预算）
    #[error("网络测试超时")]
    TestNetworkTimeout,
}

/// Engine 构造依赖包
pub struct EngineDeps {
    /// 配置服务
    pub config_service: Arc<ConfigService>,
    /// Profile 服务
    pub profile_service: Arc<ProfileService>,
    /// 登录编排器
    pub orchestrator: Arc<LoginOrchestrator>,
    /// 状态管理器
    pub status_manager: Arc<StatusManager>,
    /// 网络监测服务
    pub monitor_service: Arc<MonitorService>,
    /// 网络检测器
    pub network_detect: Arc<dyn NetworkDetect>,
}

/// 调度引擎公共接口
pub struct Engine {
    cmd_tx: mpsc::Sender<EngineCommand>,
}

pub use crate::ServiceHandle;

/// Engine 句柄（ServiceHandle 模式）
pub struct EngineHandle {
    /// Engine 公共接口
    pub engine: Arc<Engine>,
    #[allow(dead_code)] // 持有 JoinHandle 保证 task 生命周期与句柄绑定；drop 即 detach
    join_handle: JoinHandle<()>,
    /// Engine task 完成信号（正常退出与 panic 均触发，任意数量等待者）
    ///
    /// `CancellationToken` 而非 `Notify`：Notify 单 permit 在多等待者竞争时会
    /// 丢失唤醒（watch_engine 与 graceful_shutdown 并发等待）；token 取消对
    /// 任意数量 `cancelled().await` 全体可见。
    pub completed: Arc<CancellationToken>,
}

/// 完成信号守卫：spawn 块退出时（含 panic 展开）触发取消
///
/// panic 会跳过 `.await` 之后的普通语句，但 Drop 在 unwind 中仍执行——
/// 原 `notify_one()` 写法在 Engine panic 时从不触发，初始 Engine 的崩溃
/// 因此从未被 watch_engine 检测到（本批修复）。
struct CompletionGuard {
    token: Arc<CancellationToken>,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl Engine {
    /// 创建 channel + tokio::spawn + 返回 handle
    pub fn spawn(deps: EngineDeps) -> EngineHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let engine = Arc::new(Engine { cmd_tx });

        // 完成信号：Engine task 退出（含 panic）时立即唤醒等待者
        let completed = Arc::new(CancellationToken::new());

        let engine_for_task = Arc::clone(&engine);
        let guard = CompletionGuard {
            token: completed.clone(),
        };
        let join_handle = tokio::spawn(async move {
            let _guard = guard;
            run_loop::run_loop(engine_for_task, deps, cmd_rx).await;
        });

        EngineHandle {
            engine,
            join_handle,
            completed,
        }
    }

    /// 发送命令（Launcher/Tray 使用，可阻塞）
    pub async fn dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| EngineError::ChannelClosed)
    }

    /// 尝试发送命令（Web API 使用，不阻塞）
    pub fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        self.cmd_tx.try_send(cmd).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => EngineError::ChannelFull,
            mpsc::error::TrySendError::Closed(_) => EngineError::ChannelClosed,
        })
    }

    /// 由既有发送端构造（仅供 slot 单测使用，不 spawn 任务）
    #[cfg(test)]
    pub(crate) fn from_sender(cmd_tx: mpsc::Sender<EngineCommand>) -> Self {
        Self { cmd_tx }
    }
}

impl EngineHandle {
    /// 由既有 Engine 构造空壳句柄（仅供 slot 单测使用）
    #[cfg(test)]
    pub(crate) fn for_test(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            join_handle: tokio::spawn(async {}),
            completed: Arc::new(CancellationToken::new()),
        }
    }
}

/// Web 层消费的引擎抽象（M1 细粒度 state：engine 域）
///
/// handler 经 `State<Arc<dyn EngineApi>>` 提取（AppState 直字段委派），
/// 不再触达 `state.container.engine`；实现为 [`EngineSlot`]，测试可注入
/// 内存实现（见 web/routes/monitor.rs 模块测试）。
#[async_trait::async_trait]
pub trait EngineApi: Send + Sync {
    /// 尝试发送命令到当前活跃 Engine（不阻塞）。
    ///
    /// 无活跃 Engine（重启次数耗尽 / 已清理）返回
    /// [`EngineError::ChannelClosed`]，通道饱和返回 [`EngineError::ChannelFull`]。
    fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError>;

    /// 执行一次网络探测并等待回复（30s 超时封装在实现内）。
    async fn test_network(&self) -> Result<TestNetworkResult, EngineError>;
}

#[async_trait::async_trait]
impl EngineApi for EngineSlot {
    fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        EngineSlot::try_dispatch(self, cmd)
    }

    async fn test_network(&self) -> Result<TestNetworkResult, EngineError> {
        EngineSlot::test_network(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ Engine::try_dispatch / dispatch 通道逻辑测试 ============

    #[test]
    fn test_try_dispatch_success_on_empty_channel() {
        // 容量 1 的空通道：发送成功
        let (cmd_tx, _rx) = mpsc::channel::<EngineCommand>(1);
        let engine = Engine { cmd_tx };
        assert!(engine.try_dispatch(EngineCommand::Start).is_ok());
    }

    #[test]
    fn test_try_dispatch_returns_channel_full_when_saturated() {
        // 容量 1：先填满，再次 try_dispatch 应返回 ChannelFull
        let (cmd_tx, _rx) = mpsc::channel::<EngineCommand>(1);
        let engine = Engine { cmd_tx };
        engine.try_dispatch(EngineCommand::Start).unwrap();
        let err = engine.try_dispatch(EngineCommand::Stop).unwrap_err();
        assert!(matches!(err, EngineError::ChannelFull));
        // ChannelFull Display 应包含可读文案
        assert!(err.to_string().contains("通道已满"));
    }

    #[test]
    fn test_try_dispatch_returns_channel_closed_when_rx_dropped() {
        // 接收端全部 drop → 发送返回 ChannelClosed
        let (cmd_tx, rx) = mpsc::channel::<EngineCommand>(1);
        let engine = Engine { cmd_tx };
        drop(rx);
        let err = engine.try_dispatch(EngineCommand::Start).unwrap_err();
        assert!(matches!(err, EngineError::ChannelClosed));
        assert!(err.to_string().contains("已关闭"));
    }

    #[tokio::test]
    async fn test_dispatch_returns_channel_closed_when_rx_dropped() {
        // 异步 dispatch：接收端 drop 后应返回 ChannelClosed
        let (cmd_tx, rx) = mpsc::channel::<EngineCommand>(1);
        let engine = Engine { cmd_tx };
        drop(rx);
        let err = engine.dispatch(EngineCommand::Start).await.unwrap_err();
        assert!(matches!(err, EngineError::ChannelClosed));
    }

    #[tokio::test]
    async fn test_dispatch_success_and_recv() {
        // 异步 dispatch 成功后接收端能收到命令
        let (cmd_tx, mut rx) = mpsc::channel::<EngineCommand>(2);
        let engine = Engine { cmd_tx };
        engine.dispatch(EngineCommand::Pause).await.unwrap();
        let cmd = rx.recv().await.unwrap();
        assert!(matches!(cmd, EngineCommand::Pause));
    }

    #[tokio::test]
    async fn test_completion_token_fires_on_normal_exit_and_panic() {
        // CompletionGuard：spawn 块正常退出与 panic 展开都触发 token 取消

        // 正常退出路径
        let token = Arc::new(CancellationToken::new());
        let t2 = token.clone();
        tokio::spawn(async move {
            let _guard = CompletionGuard { token: t2 };
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            token.cancelled(),
        )
        .await
        .expect("正常退出应触发完成信号");

        // panic 展开路径（catch_unwind 防止测试进程崩溃）
        let token = Arc::new(CancellationToken::new());
        let t2 = token.clone();
        tokio::spawn(async move {
            let _guard = CompletionGuard { token: t2 };
            panic!("模拟 Engine panic");
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), token.cancelled())
            .await
            .expect("panic 展开也应触发完成信号（Drop 在 unwind 中执行）");
    }

    // ============ EngineError Display 测试 ============

    #[test]
    fn test_engine_error_display_messages() {
        // 验证各变体 Display 文案（thiserror 模板渲染）
        assert!(EngineError::ChannelFull.to_string().contains("通道已满"));
        assert!(EngineError::ChannelClosed.to_string().contains("已关闭"));
        assert!(EngineError::ProbeError("boom".into())
            .to_string()
            .contains("boom"));
    }

    // ============ 常量合理性测试 ============

    #[test]
    fn test_constants_within_sane_bounds() {
        // 通道容量非零、休眠上限大于 0、profile 检测区间 min < max
        const { assert!(CMD_CHANNEL_CAPACITY > 0) };
        const { assert!(MAX_IDLE_SLEEP_SECS > 0) };
        const { assert!(PROFILE_CHECK_INTERVAL_MIN < PROFILE_CHECK_INTERVAL_MAX) };
        const { assert!(MAX_RESTART_ATTEMPTS > 0) };
    }
}
