//! 调度引擎：命令循环 + tokio::select! 驱动网络监测和登录调度

pub mod commands;
pub mod run_loop;

pub use commands::{EngineCommand, ProbeDetails, ProfileSwitchSource, TestNetworkResult};

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

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

    /// 配置重载失败
    #[error("配置重载失败: {0}")]
    ReloadFailed(String),

    /// Profile 不存在
    #[error("Profile 不存在: {0}")]
    ProfileNotFound(String),

    /// 网络探测失败（内部错误）
    #[error("网络探测执行失败: {0}")]
    ProbeError(String),

    /// Engine task panic 后重启次数耗尽
    #[error("引擎多次重启失败，需要手动重启应用")]
    RestartExhausted,
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
    /// 基准路径
    pub base_path: PathBuf,
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
    stop_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
    /// Engine task 完成通知（用于零延迟崩溃检测，替代 1s 轮询）
    pub completed: Arc<tokio::sync::Notify>,
}

impl EngineHandle {
    /// 发送停止信号并等待 task 退出
    pub async fn stop(self) {
        let _ = self.stop_tx.send(true);
        let _ = self.join_handle.await;
    }

    /// 获取底层 tokio task 的 JoinHandle 引用（用于 is_finished / abort_handle）
    pub fn task_handle(&self) -> &JoinHandle<()> {
        &self.join_handle
    }

    /// 消费句柄，等待 task 自然完成（不发送停止信号，用于崩溃恢复监测）
    pub async fn into_completion(self) {
        let _ = self.join_handle.await;
    }

    /// 消费句柄，等待 task 完成并返回 JoinResult（用于区分 panic 与正常退出）
    pub async fn into_result(self) -> Result<(), tokio::task::JoinError> {
        self.join_handle.await
    }
}

impl Engine {
    /// 创建 channel + tokio::spawn + 返回 handle
    pub fn spawn(deps: EngineDeps) -> EngineHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let (stop_tx, stop_rx) = watch::channel(false);
        let engine = Arc::new(Engine { cmd_tx });

        // 完成通知：Engine task 退出时立即唤醒等待者（零延迟，替代 1s 轮询）
        let completed = Arc::new(tokio::sync::Notify::new());

        let engine_for_task = Arc::clone(&engine);
        let completed_for_task = completed.clone();
        let join_handle = tokio::spawn(async move {
            run_loop::run_loop(engine_for_task, deps, cmd_rx, stop_rx).await;
            completed_for_task.notify_one();
        });

        EngineHandle {
            engine,
            stop_tx,
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

    /// 获取命令发送端的克隆（用于崩溃恢复时替换持有者）
    pub fn cmd_sender(&self) -> mpsc::Sender<EngineCommand> {
        self.cmd_tx.clone()
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

    #[test]
    fn test_cmd_sender_clones_shared_channel() {
        // cmd_sender 返回的克隆与原 Engine 共享同一通道
        let (cmd_tx, _rx) = mpsc::channel::<EngineCommand>(2);
        let engine = Engine { cmd_tx };
        let sender = engine.cmd_sender();
        assert!(sender.try_send(EngineCommand::Start).is_ok());
    }

    // ============ EngineError Display 测试 ============

    #[test]
    fn test_engine_error_display_messages() {
        // 验证各变体 Display 文案（thiserror 模板渲染）
        assert!(EngineError::ChannelFull.to_string().contains("通道已满"));
        assert!(EngineError::ChannelClosed.to_string().contains("已关闭"));
        assert!(EngineError::ProfileNotFound("p1".into())
            .to_string()
            .contains("p1"));
        assert!(EngineError::ProbeError("boom".into())
            .to_string()
            .contains("boom"));
        assert!(EngineError::ReloadFailed("x".into()).to_string().contains("x"));
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
