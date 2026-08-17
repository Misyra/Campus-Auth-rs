//! Engine 可替换句柄槽（Engine 引用收口，todo 7.3 中期方案）
//!
//! Engine 崩溃后由 `watch_engine` 重启，重启产生的新 [`EngineHandle`] 通过
//! [`EngineSlot::replace`] 原子换入。Web / 托盘 / 关闭流程一律经 slot 取
//! 「当前活跃」Engine，不再持有启动时的初始引用——修复「崩溃重启后
//! Web/托盘仍向已死 Engine 的通道发命令、开关失效」的问题。
//!
//! 底层为 `Arc<ArcSwapOption<EngineHandle>>`（即 `Option<Arc<EngineHandle>>`
//! 的无锁原子交换）：读取无锁；无活跃 Engine（重启次数耗尽 / 已清理）时返回
//! `None`，命令派发映射为 [`EngineError::ChannelClosed`]。

use std::sync::Arc;

use arc_swap::ArcSwapOption;

use crate::engine::{Engine, EngineCommand, EngineError, EngineHandle};

/// 可替换的 Engine 句柄槽（Clone 共享同一底层存储）
#[derive(Clone)]
pub struct EngineSlot {
    inner: Arc<ArcSwapOption<EngineHandle>>,
}

impl EngineSlot {
    /// 以初始 Engine 句柄构造
    pub fn new(initial: EngineHandle) -> Self {
        Self {
            inner: Arc::new(ArcSwapOption::from_pointee(initial)),
        }
    }

    /// 当前活跃 Engine 公共接口（快照语义）
    pub fn current_engine(&self) -> Option<Arc<Engine>> {
        self.inner.load_full().map(|h| h.engine.clone())
    }

    /// 当前活跃句柄（供完成等待）
    pub fn current_handle(&self) -> Option<Arc<EngineHandle>> {
        self.inner.load_full()
    }

    /// 原子替换为新 Engine（watch_engine 重启成功后调用）
    pub fn replace(&self, handle: EngineHandle) {
        self.inner.store(Some(Arc::new(handle)));
    }

    /// 清空槽位（重启次数耗尽、Engine 已 Dead 时调用）
    pub fn clear(&self) {
        self.inner.store(None);
    }

    /// 异步发送命令到当前活跃 Engine（Launcher/Tray 使用）
    ///
    /// 无活跃 Engine 时返回 [`EngineError::ChannelClosed`]。
    pub async fn dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        match self.current_engine() {
            Some(engine) => engine.dispatch(cmd).await,
            None => Err(EngineError::ChannelClosed),
        }
    }

    /// 尝试发送命令（Web API 使用，不阻塞）
    pub fn try_dispatch(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        match self.current_engine() {
            Some(engine) => engine.try_dispatch(cmd),
            None => Err(EngineError::ChannelClosed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// 用真实 channel 构造最小 Engine 句柄（不 spawn 任务）
    fn bare_handle() -> (EngineHandle, mpsc::Receiver<EngineCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let engine = Arc::new(Engine::from_sender(cmd_tx));
        let handle = EngineHandle::for_test(engine);
        (handle, cmd_rx)
    }

    #[tokio::test]
    async fn test_initial_handle_is_current() {
        let (handle, _rx) = bare_handle();
        let slot = EngineSlot::new(handle);
        assert!(slot.current_handle().is_some());
        assert!(slot.try_dispatch(EngineCommand::Start).is_ok());
    }

    #[tokio::test]
    async fn test_replace_swaps_command_channel() {
        let (h1, mut rx1) = bare_handle();
        let slot = EngineSlot::new(h1);
        let (h2, mut rx2) = bare_handle();
        slot.replace(h2);
        // 新句柄接管命令派发
        slot.dispatch(EngineCommand::Start).await.unwrap();
        assert!(matches!(rx2.recv().await, Some(EngineCommand::Start)));
        // 旧通道不再接收
        assert!(rx1.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_clear_makes_dispatch_channel_closed() {
        let (handle, _rx) = bare_handle();
        let slot = EngineSlot::new(handle);
        slot.clear();
        assert!(slot.current_handle().is_none());
        assert!(matches!(
            slot.try_dispatch(EngineCommand::Start),
            Err(EngineError::ChannelClosed)
        ));
        assert!(matches!(
            slot.dispatch(EngineCommand::Start).await,
            Err(EngineError::ChannelClosed)
        ));
    }

    #[tokio::test]
    async fn test_clone_shares_underlying_slot() {
        let (handle, _rx) = bare_handle();
        let slot = EngineSlot::new(handle);
        let cloned = slot.clone();
        let (h2, _rx2) = bare_handle();
        cloned.replace(h2);
        // 经克隆替换，原 slot 也看到新句柄（共享存储）
        assert!(slot.current_handle().is_some());
        assert!(Arc::ptr_eq(
            &cloned.current_handle().unwrap(),
            &slot.current_handle().unwrap()
        ));
    }
}
