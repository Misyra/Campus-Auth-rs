//! EventBus：tokio broadcast 有界通道（默认 1024，与既有 broadcast 口径一致）
//!
//! 发布端零等待、无订阅者零成本（broadcast 语义：无人接收时 `send` 仅返回
//! Err，事件即弃）；订阅端独占消费各自的环形队列。本模块只管内存分发，
//! 落盘订阅者见 [`super::sink`]。
//!
//! # Lagged 处理口径（订阅端约定）
//!
//! 慢消费者跟不上发布速率时，`recv()` 返回 `RecvError::Lagged(n)`——那是
//! broadcast 在告知"你丢了 n 条最旧的消息"。**约定：warn 留痕后继续收**
//! （不崩、不重启、不丢后续消息）；对通知类事件丢几条旧的远比停止订阅严重，
//! 消费方（sink / 后续通知与诊断）照此处理即可。

use tokio::sync::broadcast;

use super::{AppEvent, EventEnvelope};

/// 默认通道容量：突发事件（如断网重试风暴）可缓冲 1024 条，
/// 订阅端短暂阻塞不至于触发 Lagged
const DEFAULT_CAPACITY: usize = 1024;

/// 应用事件总线（类型化发布/订阅，无全局状态，容器构造注入）
pub struct EventBus {
    tx: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    /// 以默认容量构造
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// 以指定容量构造（测试注入小容量以验证 Lagged 行为）
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// 发布事件：补全 id 与 UTC 时间戳后广播
    ///
    /// 返回封装好的事件（含 id / timestamp），供发布方记日志或做关联；
    /// 无订阅者时 `send` 的 Err 被忽略——"没人听"是常态而非错误，
    /// 事件构建成本（uuid + 时间戳）即为零订阅者的全部开销。
    pub fn publish(&self, event: AppEvent) -> EventEnvelope {
        let envelope = EventEnvelope::new(event);
        let _ = self.tx.send(envelope.clone());
        envelope
    }

    /// 订阅事件流；Lagged 处理口径见模块 doc（warn 留痕继续收）
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// 当前订阅端数量（诊断用）
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{LoginSummary, NetworkSummary};
    use super::*;

    fn login_event() -> AppEvent {
        AppEvent::LoginSucceeded(LoginSummary {
            profile_id: "p-1".into(),
            channel: "browser".into(),
            reason: None,
            attempt: Some(1),
        })
    }

    /// publish / subscribe 往返：订阅者收到的是发布返回的同一封装
    #[tokio::test]
    async fn test_publish_subscribe_roundtrip() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let published = bus.publish(login_event());
        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, published.id);
        assert_eq!(received.event, published.event);
        assert_eq!(received.timestamp, published.timestamp);
        assert_eq!(received.event.kind(), "login_succeeded");
    }

    /// 多订阅者各自独立收到完整副本
    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);
        let published = bus.publish(AppEvent::NetworkDown(NetworkSummary {
            profile_id: Some("p-1".into()),
            reason: Some("tcp_timeout".into()),
        }));
        let got1 = rx1.recv().await.unwrap();
        let got2 = rx2.recv().await.unwrap();
        assert_eq!(got1.id, published.id);
        assert_eq!(got2.id, published.id);
        assert_eq!(got1.event, got2.event);
    }

    /// 无订阅者发布零成本：不 panic、不阻塞，正常返回封装
    #[tokio::test]
    async fn test_publish_without_subscribers() {
        let bus = EventBus::new();
        let published = bus.publish(login_event());
        assert_eq!(published.event.kind(), "login_succeeded");
        assert_eq!(bus.receiver_count(), 0);
    }

    /// Lagged 容忍：慢订阅者落后于容量后，先报 Lagged 再续传最新消息
    #[tokio::test]
    async fn test_lagged_then_continue() {
        // 容量 2：落后 3 条即触发 Lagged
        let bus = EventBus::with_capacity(2);
        let mut rx = bus.subscribe();
        for _ in 0..3 {
            bus.publish(login_event());
        }
        // 第一次 recv 丢的是最旧的 n 条，返回 Lagged
        let err = rx.recv().await.unwrap_err();
        let lost = match err {
            broadcast::error::RecvError::Lagged(n) => n,
            other => panic!("应为 Lagged，实际: {other:?}"),
        };
        assert_eq!(lost, 1, "容量 2 内保留了 2 条，丢弃 1 条最旧的");
        // warn 留痕继续收：后续消息照常到达（最新一条仍在环形队列里）
        let next = rx.recv().await.unwrap();
        assert_eq!(next.event.kind(), "login_succeeded");
    }
}
