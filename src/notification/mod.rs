//! 推送通知服务：订阅 EventBus，按配置把事件经系统通知 / webhook / Bark / Server酱 / Telegram 推送
//!
//! # 服务形态
//!
//! [`spawn_notification_service`] 为**接线就绪**的服务构造点：订阅在 spawn 返回
//! 前同步建立（broadcast 只投递订阅之后的消息，先订阅再 spawn 保证调用方拿到
//! 句柄前的 publish 不丢），返回 [`crate::ServiceHandle`] 统一句柄。容器接线时
//! 以 `spawn_notification_service(container.events.clone(), container.config.clone(), base_path)`
//! 挂入（`Arc<dyn ConfigApi>` 已实现 [`NotificationConfigSource`]）。
//!
//! # 可停止语义（P3-10）
//!
//! 本服务**不监听应用级关闭令牌**（只消费 EventBus 广播），停用只能经
//! [`crate::ServiceHandle`] 的 stop 通道：`stop` / `stop_with_timeout` 发出
//! `true` 后主循环即刻退出并执行停机排空（`run_notification_loop` 尾部的
//! `try_recv` 段，在途积压事件照常派发为独立任务）；`stop_tx` 被 drop 仅
//! 停用该信号源、生命周期随应用退出（与事件落盘同口径）。优雅关闭流程
//! （`launcher::shutdown` 第 2 步）据此对通知句柄显式 `stop_with_timeout`，
//! 保证停机排空在 runtime teardown 前执行完毕。
//!
//! # 失败静默口径（与登录主链路的边界）
//!
//! 发送失败——包括重试退避（[`sender::send_with_retry`]，指数退避；os 通道
//! 例外：失败多为环境性，不重试）耗尽——只
//! `tracing::warn` 留痕并继续服务：**绝不 panic、绝不影响登录主链路**。事件
//! 发布方对通知零感知（broadcast 发布不等待）；单事件投递也在独立任务执行，
//! 慢 HTTP 不会阻塞事件消费循环。
//!
//! # 配置读取
//!
//! 服务内缓存 [`NotificationSettings`]，每 `CONFIG_REFRESH_INTERVAL` 从配置源
//! 重读一次：修改推送配置最多一个周期后生效，避免为低频事件逐条读盘。
//!
//! # Lagged 处理口径
//!
//! 与 `events` 模块同一约定：warn 留痕后继续收（不崩、不重启）；推送是尽力
//! 而为的旁路，丢几条旧通知远比停止订阅严重。

pub mod filter;
pub mod sender;
pub mod template;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};

use crate::ServiceHandle;
use crate::config::{ConfigApi, NotificationSettings};
use crate::events::EventEnvelope;

/// 通知配置源抽象（服务不做磁盘/锁细节，接线时由具体配置服务实现）
///
/// 实现方应保证多次调用幂等且开销可控；生产实现走 `ConfigApi`（settings 快照）。
#[async_trait::async_trait]
pub trait NotificationConfigSource: Send + Sync + 'static {
    /// 读取当前推送通知设置
    async fn load_notification_settings(&self) -> NotificationSettings;
}

#[async_trait::async_trait]
impl NotificationConfigSource for Arc<dyn ConfigApi> {
    async fn load_notification_settings(&self) -> NotificationSettings {
        self.load_settings_async().await.global.notification
    }
}

/// 配置刷新间隔：修改推送配置到生效的最大延迟
const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// 启动推送通知服务（订阅 [`crate::events::EventBus`]，按配置过滤并发送）
///
/// `base_path` 为预留参数：当前推送无落盘需求，保留它与事件落盘
/// （`spawn_event_sink`）同参形态，便于接线批次对齐传参；后续若需要投递
/// 审计或本地缓存，直接在该目录下派生子目录即可。
pub fn spawn_notification_service(
    bus: Arc<crate::events::EventBus>,
    config_source: Arc<dyn NotificationConfigSource>,
    _base_path: PathBuf,
) -> ServiceHandle {
    // 订阅先于 spawn：句柄返回前的 publish 也必须投递到（与事件落盘同口径）
    let rx = bus.subscribe();
    let (stop_tx, stop_rx) = watch::channel(false);
    let join_handle = tokio::spawn(async move {
        run_notification_loop(rx, config_source, stop_rx).await;
    });
    ServiceHandle {
        stop_tx,
        join_handle,
        name: "notification",
    }
}

/// 服务主循环：select 配置刷新 / 停止信号 / 事件接收（ServiceHandle 统一模式）
async fn run_notification_loop(
    mut rx: broadcast::Receiver<EventEnvelope>,
    config_source: Arc<dyn NotificationConfigSource>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut cached = config_source.load_notification_settings().await;
    let mut refresh = tokio::time::interval(CONFIG_REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // stop_tx 是否仍存活（句柄未经 stop 被 drop 后停用该信号源，与事件落盘同口径）
    let mut watch_alive = true;
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                cached = config_source.load_notification_settings().await;
            }
            changed = stop_rx.changed(), if watch_alive => {
                match changed {
                    Ok(()) => {
                        if *stop_rx.borrow_and_update() {
                            break;
                        }
                        // Ok 但值仍为 false：假唤醒，回到循环继续等
                    }
                    Err(_) => {
                        // stop_tx 已 drop：仅停用该信号源，生命周期随应用退出
                        watch_alive = false;
                    }
                }
            }
            recv = rx.recv() => match recv {
                Ok(envelope) => dispatch_event_detached(envelope, cached.clone()),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Lagged 处理口径（见 bus 模块 doc）：warn 留痕继续收
                    tracing::warn!("推送通知落后 {n} 条，最旧事件已丢弃（继续收后续）");
                }
                // 总线关闭：所有发送端均已 drop（应用退出路径）
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    // 停止后排空积压并照常派发（派发为独立任务、不等待完成；应用退出时随
    // runtime 结束，发送中断即静默失败，符合本模块失败口径）。本段即
    // 「停机排空」：只有经 ServiceHandle 显式 stop 才会执行——这是优雅关闭
    // 流程对通知句柄显式 stop_with_timeout 的直接动机（P3-10，语义见模块 doc）
    while let Ok(envelope) = rx.try_recv() {
        dispatch_event_detached(envelope, cached.clone());
    }
}

/// 单事件投递入口（独立任务派发）：慢 HTTP 不阻塞事件消费循环
fn dispatch_event_detached(envelope: EventEnvelope, settings: NotificationSettings) {
    tokio::spawn(async move {
        dispatch_event(&envelope, &settings).await;
    });
}

/// 单事件投递：开关 → 事件类型集合 → 免打扰时段 → 逐通道发送
///
/// 通道间串行（避免风暴式并发）；任何通道失败只 warn，不影响其他通道。
pub(crate) async fn dispatch_event(envelope: &EventEnvelope, settings: &NotificationSettings) {
    if !settings.enabled {
        return;
    }
    let kind = envelope.event.kind();
    if !filter::event_allowed(&settings.events, kind) {
        return;
    }
    // 免打扰判定用本地时区（用户感知的「夜里 22 点」）；窗口无效（越界）视为未启用
    if let Some((start, end)) = settings.quiet_window()
        && filter::in_quiet_window(chrono::Local::now().time(), start, end)
    {
        tracing::debug!(event = kind, "免打扰时段内，事件仅记录不推送");
        return;
    }
    let message = template::summarize(envelope);
    let policy = sender::RetryPolicy::from_settings(settings);
    for channel in settings.channels.iter().filter(|c| c.enabled()) {
        match sender::send_with_retry(channel, &message, &policy, &sender::RealClock).await {
            Ok(()) => {
                tracing::info!(
                    channel = channel.display_name(),
                    event = kind,
                    "推送通知已发送"
                );
            }
            Err(err) => {
                // 失败静默：重试耗尽也只留痕，绝不上抛、绝不影响主链路
                tracing::warn!(
                    channel = channel.display_name(),
                    event = kind,
                    "推送通知发送失败（已重试）: {err}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotificationChannel, NotificationSettings};
    use crate::events::{AppEvent, EventBus, LoginSummary, NetworkSummary};
    use chrono::Timelike;
    use std::sync::Mutex;
    use std::time::Duration;

    /// 固定配置源（测试注入；真实接线为 Arc<dyn ConfigApi>）
    struct FixedSource(NotificationSettings);

    #[async_trait::async_trait]
    impl NotificationConfigSource for FixedSource {
        async fn load_notification_settings(&self) -> NotificationSettings {
            self.0.clone()
        }
    }

    /// 本地 webhook 接收器：记录 (头名, 头值, 请求体)，返回监听地址
    async fn spawn_webhook_receiver() -> (
        std::net::SocketAddr,
        Arc<Mutex<Vec<(String, String, String)>>>,
    ) {
        use axum::routing::post;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let capture = captured.clone();
        let app = axum::Router::new().route(
            "/hook",
            post(move |headers: axum::http::HeaderMap, body: String| {
                let capture = capture.clone();
                async move {
                    let token = headers
                        .get("x-token")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    capture
                        .lock()
                        .unwrap()
                        .push(("x-token".into(), token, body));
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("接收器不应崩溃");
        });
        (addr, captured)
    }

    fn webhook_channel(addr: std::net::SocketAddr) -> NotificationChannel {
        NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: "本地接收器".into(),
            enabled: true,
            url: format!("http://{addr}/hook"),
            headers: vec![crate::config::NotificationHeader {
                name: "X-Token".into(),
                value: "secret-header-value".into(),
            }],
            body_template: String::new(),
            timeout_seconds: 5,
        }
    }

    fn webhook_channel_disabled(addr: std::net::SocketAddr) -> NotificationChannel {
        NotificationChannel::Webhook {
            id: "ch-1".into(),
            name: "本地接收器".into(),
            enabled: false,
            url: format!("http://{addr}/hook"),
            headers: Vec::new(),
            body_template: String::new(),
            timeout_seconds: 5,
        }
    }

    fn settings_with(
        channels: Vec<NotificationChannel>,
        events: Vec<&str>,
        mutate: impl FnOnce(&mut NotificationSettings),
    ) -> NotificationSettings {
        let mut settings = NotificationSettings {
            enabled: true,
            channels,
            events: events.into_iter().map(String::from).collect(),
            ..NotificationSettings::default()
        };
        mutate(&mut settings);
        settings
    }

    fn login_failed_event() -> AppEvent {
        AppEvent::LoginFailed(LoginSummary {
            profile_id: "p-1".into(),
            channel: "http".into(),
            reason: Some("invalid_credential".into()),
            attempt: Some(1),
        })
    }

    fn network_up_event() -> AppEvent {
        AppEvent::NetworkUp(NetworkSummary {
            profile_id: None,
            reason: None,
        })
    }

    /// 等待捕获条数达到期望（轮询 + 总超时），超时返回当前条数
    async fn wait_for(captured: &Mutex<Vec<(String, String, String)>>, expected: usize) -> usize {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let count = captured.lock().unwrap().len();
            if count >= expected {
                return count;
            }
            if tokio::time::Instant::now() >= deadline {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 端到端：订阅 → 过滤 → webhook 真实送达（自定义头与 JSON 载荷形状）
    #[tokio::test]
    async fn test_service_delivers_allowed_event_to_webhook() {
        let (addr, captured) = spawn_webhook_receiver().await;
        let bus = Arc::new(EventBus::new());
        let settings = settings_with(vec![webhook_channel(addr)], vec!["login_failed"], |_| {});
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );

        bus.publish(login_failed_event());
        // 不在集合里的事件不推送
        bus.publish(network_up_event());
        assert_eq!(
            wait_for(&captured, 1).await,
            1,
            "仅 login_failed 事件推送一次"
        );
        let (header_name, header_value, body) = captured.lock().unwrap()[0].clone();
        assert_eq!(header_name, "x-token");
        assert_eq!(header_value, "secret-header-value");
        let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["title"], "登录失败");
        assert_eq!(payload["event"], "login_failed");

        service.stop_with_timeout(Duration::from_secs(3)).await;
    }

    /// 事件不在类型集合 / 通道未启用 / 总开关关闭：零发送
    #[tokio::test]
    async fn test_service_filters_silently() {
        let (addr, captured) = spawn_webhook_receiver().await;
        let bus = Arc::new(EventBus::new());
        // 通道存在但 disabled
        let disabled = webhook_channel_disabled(addr);
        let settings = settings_with(vec![disabled], vec!["login_failed"], |_| {});
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );
        bus.publish(login_failed_event());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(captured.lock().unwrap().len(), 0, "禁用通道不发送");
        service.stop_with_timeout(Duration::from_secs(3)).await;

        // 总开关关闭：同样零发送（且不 panic）
        let settings = settings_with(vec![webhook_channel(addr)], vec!["login_failed"], |s| {
            s.enabled = false;
        });
        let bus = Arc::new(EventBus::new());
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );
        bus.publish(login_failed_event());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(captured.lock().unwrap().len(), 0, "总开关关闭不发送");
        service.stop_with_timeout(Duration::from_secs(3)).await;
    }

    /// 免打扰时段内：只记日志不发送（动态构造覆盖当前时刻的窗口）
    #[tokio::test]
    async fn test_dispatch_quiet_window_blocks_send() {
        let (addr, captured) = spawn_webhook_receiver().await;
        // 构造必然命中的窗口：[now-1h, now+1h]（跨零点由 filter 纯函数专项覆盖）
        let now = chrono::Local::now().time();
        let start = now - chrono::Duration::hours(1);
        let end = now + chrono::Duration::hours(1);
        let settings = settings_with(vec![webhook_channel(addr)], vec!["login_failed"], |s| {
            s.quiet_enabled = true;
            s.quiet_start_hour = start.hour() as u8;
            s.quiet_start_minute = start.minute() as u8;
            s.quiet_end_hour = end.hour() as u8;
            s.quiet_end_minute = end.minute() as u8;
        });
        let envelope = crate::events::EventEnvelope::new(login_failed_event());
        dispatch_event(&envelope, &settings).await;
        assert_eq!(captured.lock().unwrap().len(), 0, "免打扰时段内不发送");
    }

    /// 发送失败静默：坏通道（连接拒绝 + 重试耗尽）不影响服务与其他通道送达
    #[tokio::test]
    async fn test_send_failure_is_silent_and_non_blocking() {
        let (addr, captured) = spawn_webhook_receiver().await;
        let bus = Arc::new(EventBus::new());
        // 坏通道（端口 1 快速拒绝）在前，好通道在后；重试置 0 保持测试快
        let bad = NotificationChannel::Webhook {
            id: "ch-bad".into(),
            name: String::new(),
            enabled: true,
            url: "http://127.0.0.1:1/hook".into(),
            headers: Vec::new(),
            body_template: String::new(),
            timeout_seconds: 2,
        };
        let settings = settings_with(
            vec![bad, webhook_channel(addr)],
            vec!["login_failed"],
            |s| {
                s.retry_max_attempts = 0;
            },
        );
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );
        bus.publish(login_failed_event());
        assert_eq!(wait_for(&captured, 1).await, 1, "坏通道失败不影响好通道");
        // 服务仍在运行：再发一条仍能送达
        bus.publish(login_failed_event());
        assert_eq!(wait_for(&captured, 2).await, 2, "服务继续存活");
        service.stop_with_timeout(Duration::from_secs(3)).await;
    }

    /// Lagged 容忍：小容量总线被灌爆后，服务 warn 继续收，后续事件照常推送
    #[tokio::test]
    async fn test_service_survives_lagged() {
        let (addr, captured) = spawn_webhook_receiver().await;
        let bus = Arc::new(EventBus::with_capacity(1));
        let settings = settings_with(vec![webhook_channel(addr)], vec!["login_failed"], |_| {});
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );
        // 容量 1：连发 3 条必触发 Lagged（丢 2 条最旧）
        for _ in 0..3 {
            bus.publish(login_failed_event());
        }
        // 至少收到 1 条（环形队列保留的最新一条），且服务未崩溃
        assert!(
            wait_for(&captured, 1).await >= 1,
            "Lagged 后最新事件仍应送达"
        );
        // 服务继续收新事件
        bus.publish(login_failed_event());
        assert_eq!(wait_for(&captured, 2).await, 2, "Lagged 后服务继续工作");
        service.stop_with_timeout(Duration::from_secs(5)).await;
    }

    /// P3-10：通知服务可被 stop 信号在超时内结束——主循环退出、EventBus
    /// 订阅释放、停机排空照常执行，全程不依赖 abort 兜底
    #[tokio::test]
    async fn test_service_stops_within_timeout_on_stop_signal() {
        let (addr, captured) = spawn_webhook_receiver().await;
        let bus = Arc::new(EventBus::new());
        let settings = settings_with(vec![webhook_channel(addr)], vec!["login_failed"], |_| {});
        let service = spawn_notification_service(
            bus.clone(),
            Arc::new(FixedSource(settings)),
            PathBuf::new(),
        );
        // 停止前服务存活且正常送达（排除「通道关闭导致意外退出」的假阳性）
        bus.publish(login_failed_event());
        assert_eq!(wait_for(&captured, 1).await, 1, "停止前事件正常送达");
        assert_eq!(bus.receiver_count(), 1, "停止前应恰有一个订阅者");

        // 拆开句柄直接发 stop 信号并限时 join：超时即失败——证明服务是
        // 「被 stop 信号结束」而非靠 stop_with_timeout 的 abort 兜底过关
        let crate::ServiceHandle {
            stop_tx,
            join_handle,
            name: _,
        } = service;
        stop_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), join_handle)
            .await
            .expect("通知服务应在 stop 信号后 3s 内退出")
            .expect("通知服务 task 不应 panic");

        // 循环退出后订阅释放：EventBus 订阅者计数归零（task 已 join，
        // rx 的 drop 先于 join 完成可见，此处无竞态）
        assert_eq!(bus.receiver_count(), 0, "退出后 EventBus 订阅应释放");
    }
}
