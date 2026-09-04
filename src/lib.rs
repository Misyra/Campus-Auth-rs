//! Campus-Auth 库入口：聚合所有模块
//!
//! 二进制入口 `main.rs` 仅做 CLI 解析与启动分发，核心逻辑均在此库 crate 中实现。
//! 各模块通过 `crate::` 互相引用，无全局可变状态。

use tokio::sync::watch;
use tokio::task::JoinHandle;

pub mod app;
pub mod bridge;
pub mod browser;
pub mod config;
pub mod container;
pub mod engine;
pub mod environment;
pub mod launcher;
pub mod logging;
pub mod login;
pub mod monitor;
pub mod network;
pub mod scheduler;
pub mod status;
pub mod tasks;
pub mod tray;
pub mod updater;
pub mod utils;
pub mod web;

/// 统一启停句柄（ServiceHandle 模式）
///
/// 跨模块共享的基础类型：发送停止信号 (`stop_tx`) 并等待后台 task 退出 (`join_handle`)。
/// 由 bridge/engine/scheduler/tray 各服务的 spawn/start 方法返回。
pub struct ServiceHandle {
    /// 停止信号
    pub stop_tx: watch::Sender<bool>,
    /// tokio task 句柄
    pub join_handle: JoinHandle<()>,
    /// 服务名（最小传入：仅用于退出/超时日志定位是哪个服务）
    pub name: &'static str,
}

impl ServiceHandle {
    /// 发送停止信号并等待 task 退出
    pub async fn stop(self) {
        let _ = self.stop_tx.send(true);
        if let Err(e) = self.join_handle.await {
            tracing::warn!(service = self.name, "服务任务退出时返回错误: {:?}", e);
        }
    }

    /// 在限定时间内停止服务，超时后中止后台 task 并完成回收。
    ///
    /// 与在调用方外层包裹 [`tokio::time::timeout`] 不同，本方法仍持有
    /// `JoinHandle`，因此超时时不会仅仅丢弃句柄、让后台 task 继续游离运行。
    pub async fn stop_with_timeout(mut self, timeout: std::time::Duration) {
        let _ = self.stop_tx.send(true);
        match tokio::time::timeout(timeout, &mut self.join_handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(service = self.name, "服务任务退出时返回错误: {:?}", e)
            }
            Err(_) => {
                tracing::warn!(service = self.name, "服务任务关闭超时，强制中止后台 task");
                self.join_handle.abort();
                let _ = self.join_handle.await;
            }
        }
    }
}
