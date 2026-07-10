//! WebSocket /ws/logs 处理器：将广播通道日志与状态快照推送到前端
//!
//! 所有推送消息均包装为信封格式 `{ "type": "log" | "status" | "pong", "data": ... }`。
//! 前端发送 `{ "type": "ping" }` 文本消息时回复 `{ "type": "pong" }`。
//! 新连接建立时立即发送当前状态快照（首帧同步）。

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;

use super::state::{AppState, LogEntry};
use crate::status::StatusSnapshot;

/// 调试截图消息
#[derive(Serialize)]
pub struct ScreenshotMsg {
    /// 截图 URL（后端可访问路径）
    pub url: String,
    /// 关联步骤索引（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
    /// 说明（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 调试步骤进度消息
#[derive(Serialize)]
pub struct StepProgressMsg {
    /// 当前步骤索引
    pub step_index: usize,
    /// 步骤总数
    pub total_steps: usize,
    /// 步骤说明（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 步骤类型（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_type: Option<String>,
}

/// WebSocket 消息信封：所有推送消息均包装为 `{ "type": "...", "data": ... }`
///
/// 注：`Screenshot` / `StepProgress` 变体仅由 Bridge 以原始 JSON 形式经 `ws_tx` 通道推送，
/// 不直接经由此枚举构造，故标记 `allow(dead_code)` 以保留其作为协议契约的文档意义。
#[derive(Serialize)]
#[allow(dead_code)]
#[serde(tag = "type", content = "data")]
enum WsMessage {
    /// 日志条目
    #[serde(rename = "log")]
    Log(LogEntry),
    /// 状态快照
    #[serde(rename = "status")]
    Status(StatusSnapshot),
    /// 心跳响应
    #[serde(rename = "pong")]
    Pong,
    /// 调试截图
    #[serde(rename = "screenshot")]
    Screenshot(ScreenshotMsg),
    /// 调试步骤进度
    #[serde(rename = "step_progress")]
    StepProgress(StepProgressMsg),
}

/// 前端发来的 WebSocket 文本消息信封
#[derive(serde::Deserialize)]
struct WsIncoming {
    /// 消息类型
    #[serde(rename = "type")]
    msg_type: String,
}

/// Ping 定时器间隔（30 秒）
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Pong 超时（60 秒内未收到客户端任何消息则视为断线）
const PONG_TIMEOUT: Duration = Duration::from_secs(60);

/// GET /ws/logs → 升级为 WebSocket，持续推送日志与状态
pub async fn logs_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_logs(socket, state))
}

/// 发送超时（200ms），防止慢消费者阻塞事件循环
const SEND_TIMEOUT: Duration = Duration::from_millis(200);

/// 通过 socket 发送一条消息，超时则断开连接返回 false
async fn send_msg(socket: &mut WebSocket, msg: Message) -> bool {
    match tokio::time::timeout(SEND_TIMEOUT, socket.send(msg)).await {
        Ok(result) => result.is_ok(),
        Err(_) => {
            tracing::warn!("WebSocket 发送超时，断开慢消费者");
            false
        }
    }
}

/// 序列化 WsMessage 并通过 socket 发送，失败返回 false 表示客户端已断开
async fn send_ws(socket: &mut WebSocket, msg: &WsMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(json) => send_msg(socket, Message::Text(json.into())).await,
        Err(e) => {
            tracing::warn!("WebSocket 序列化失败: {e}");
            true // 序列化失败不中断连接
        }
    }
}

async fn handle_logs(mut socket: WebSocket, state: AppState) {
    let mut log_rx = state.log_tx.subscribe();
    let mut status_rx = state.container.status.subscribe();
    // 通用事件通道（screenshot / step_progress 等），由 Bridge 推送
    let mut ws_rx = state.ws_tx.subscribe();

    // 首帧同步：立即发送当前状态快照
    {
        let snapshot = status_rx.borrow().clone();
        if !send_ws(&mut socket, &WsMessage::Status(snapshot)).await {
            return;
        }
    }

    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    // 第一次 tick 立即触发，跳过
    ping_interval.tick().await;

    // 记录最后一次收到客户端消息的时间，用于 Pong 超时检测
    let mut last_client_msg = tokio::time::Instant::now();

    loop {
        tokio::select! {
            // 从广播通道读取日志条目
            msg = log_rx.recv() => {
                match msg {
                    Ok(entry) => {
                        if !send_ws(&mut socket, &WsMessage::Log(entry)).await {
                            break;
                        }
                    }
                    // 广播发送端丢弃 → 通道关闭
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket 客户端丢弃 {n} 条历史日志");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 状态快照变更推送
            result = status_rx.changed() => {
                if result.is_err() {
                    break;
                }
                let snapshot = status_rx.borrow().clone();
                if !send_ws(&mut socket, &WsMessage::Status(snapshot)).await {
                    break;
                }
            }
            // Ping 定时器：向客户端发送 WebSocket 协议级 Ping 帧保活
            _ = ping_interval.tick() => {
                // 检查 Pong 超时：客户端超过 PONG_TIMEOUT 未发送任何消息
                if last_client_msg.elapsed() > PONG_TIMEOUT {
                    tracing::debug!("WebSocket 客户端 Pong 超时，断开连接");
                    break;
                }
                // 发送协议级 Ping 帧（客户端 WebSocket 库自动回复 Pong）
                if !send_msg(&mut socket, Message::Ping(bytes::Bytes::new())).await {
                    break;
                }
            }
            // 通用事件通道：Bridge 推来的 screenshot / step_progress 等
            msg = ws_rx.recv() => {
                match msg {
                    Ok(text) => {
                        if !send_msg(&mut socket, Message::Text(text.into())).await {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket 客户端丢弃 {n} 条历史日志");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 监听客户端消息（Close / Text ping / Ping / Pong）
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        last_client_msg = tokio::time::Instant::now();
                        // 处理前端发来的应用层 ping: { "type": "ping" }
                        if let Ok(incoming) = serde_json::from_str::<WsIncoming>(&text) {
                            if incoming.msg_type == "ping"
                                && !send_ws(&mut socket, &WsMessage::Pong).await
                            {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        last_client_msg = tokio::time::Instant::now();
                        let _ = send_msg(&mut socket, Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_client_msg = tokio::time::Instant::now();
                    }
                    Some(Err(_)) => break,
                    _ => {
                        last_client_msg = tokio::time::Instant::now();
                    }
                }
            }
        }
    }
}
