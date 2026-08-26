//! WebSocket /ws/logs 处理器：将广播通道日志与状态快照推送到前端
//!
//! 所有推送消息均包装为信封格式 `{ "type": "log" | "status" | "pong", "data": ... }`。
//! 前端发送 `{ "type": "ping" }` 文本消息时回复 `{ "type": "pong" }`。
//! 新连接建立时立即发送当前状态快照（首帧同步）。

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde::Serialize;

use super::state::{AppState, LogEntry};
use crate::status::StatusSnapshot;

/// WebSocket 消息信封：所有推送消息均包装为 `{ "type": "...", "data": ... }`
///
/// 注：调试截图 / 步骤进度由 Bridge 以原始 JSON 形式经 `ws_tx` 通道推送，
/// 不直接经由此枚举构造。
#[derive(Serialize)]
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
}

/// 前端发来的 WebSocket 文本消息信封
#[derive(serde::Deserialize)]
struct WsIncoming {
    /// 消息类型
    #[serde(rename = "type")]
    msg_type: String,
    /// 消息数据（frontend_log 携带 {level, scope, message, meta}）
    #[serde(default)]
    data: Option<serde_json::Value>,
}

/// 截断前端回流日志中的可变文本，避免异常对象或错误堆栈撑大日志文件。
fn truncate_log_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

/// 记录前端回流日志：用 `target="frontend"` 写入 tracing，进入 app.log 与 WebSocket 广播
fn record_frontend_log(data: &serde_json::Value) {
    let level = data.get("level").and_then(|v| v.as_str()).unwrap_or("INFO");
    let scope = truncate_log_text(
        data.get("scope").and_then(|v| v.as_str()).unwrap_or(""),
        128,
    );
    let message = truncate_log_text(
        data.get("message").and_then(|v| v.as_str()).unwrap_or(""),
        4096,
    );
    let meta = data.get("meta").cloned().unwrap_or(serde_json::Value::Null);
    // 附带 meta（非 null 时）与 scope 字段，保持与后端日志结构一致
    let meta_str = if meta.is_null() {
        String::new()
    } else {
        format!(" meta={}", truncate_log_text(&meta.to_string(), 2048))
    };
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => tracing::error!(target: "frontend", scope = %scope, "{message}{meta_str}"),
        "WARNING" | "WARN" => {
            tracing::warn!(target: "frontend", scope = %scope, "{message}{meta_str}")
        }
        "DEBUG" | "TRACE" => {
            tracing::debug!(target: "frontend", scope = %scope, "{message}{meta_str}")
        }
        _ => tracing::info!(target: "frontend", scope = %scope, "{message}{meta_str}"),
    }
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

/// WebSocket 单连接世代号：进程级原子递增计数器
///
/// 每个新连接接入时 `fetch_add(1)` 原子取号，号码严格按接入顺序分配且全局唯一。
/// AppState 中的 `ws_epoch_tx`（watch 通道）仅作「世代变化」的唤醒信号，
/// 新旧比较一律以此原子计数为准。
static WS_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    // 单连接限制（并发安全）：接入时原子取号（号码即接入顺序），再经 watch 通道
    // 广播唤醒旧连接；旧连接醒来后比较「最新世代号是否大于自己的号」，大于则被顶替。
    //
    // 旧实现 borrow→send(+1)→subscribe 三步非原子：两个连接并发接入时都 borrow 到
    // 同一世代号 N 并各自 send(N+1)，两方订阅时通道值同为 N+1，谁都看不到变化，
    // 顶替失效、多连接并存。原子 fetch_add 保证并发接入取号唯一且有序，
    // 订阅前后只需分别核对一次最新世代号即可覆盖所有交错：
    // - 更早接入的连接（号码更小）要么在订阅后核对时发现更大号码，要么被
    //   后续连接的唤醒信号触发核对后断开；
    // - 更晚接入的连接（号码更大）收到旧连接迟到的唤醒信号时，核对发现最新
    //   世代号不大于自己的号，继续存活。
    let my_epoch = WS_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let _ = state.ws_epoch_tx.send(my_epoch);
    let mut epoch_rx = state.ws_epoch_tx.subscribe();
    // 订阅后核对：取号与订阅之间若有更新连接接入，其唤醒信号在本次订阅之前
    // 发送、不会再触发 changed()，必须主动比较一次，否则新旧并存
    if WS_EPOCH.load(std::sync::atomic::Ordering::SeqCst) > my_epoch {
        tracing::debug!("WebSocket 连接接入即被更新页面顶替，直接断开");
        return;
    }

    let mut log_rx = state.log_tx.subscribe();
    let mut status_rx = state.status.subscribe();
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
                if !send_msg(&mut socket, Message::Ping(Default::default())).await {
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
            // 世代号变化唤醒：有新连接接入（也可能是旧连接迟到的唤醒信号）。
            // 仅当最新世代号大于本连接号码时才视为被顶替——原子计数只增不减，
            // 大于本连接号码必然意味着有更晚接入的连接存活。
            // （避免多个前端页面同时接收日志/事件导致重复与状态冲突）
            result = epoch_rx.changed() => {
                if result.is_err() {
                    break;
                }
                if WS_EPOCH.load(std::sync::atomic::Ordering::SeqCst) > my_epoch {
                    tracing::debug!("WebSocket 连接被新页面顶替，断开旧连接");
                    // 通知前端"被顶替"，前端据此停止自动重连，避免多标签页互相踢导致死循环
                    let _ = send_msg(&mut socket, Message::Text("{\"type\":\"ws_kicked\"}".into())).await;
                    break;
                }
                // 最新世代号不大于本连接：是更旧连接迟到的唤醒信号，继续存活
                continue;
            }
            // 监听客户端消息（Close / Text ping / Ping / Pong）
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        last_client_msg = tokio::time::Instant::now();
                        // 处理前端发来的应用层消息
                        if let Ok(incoming) = serde_json::from_str::<WsIncoming>(&text) {
                            match incoming.msg_type.as_str() {
                                // 心跳: { "type": "ping" } → 回复 pong
                                "ping" => {
                                    if !send_ws(&mut socket, &WsMessage::Pong).await {
                                        break;
                                    }
                                }
                                // 前端日志回流: { "type": "frontend_log", "data": {...} }
                                "frontend_log" => {
                                    if let Some(data) = &incoming.data {
                                        record_frontend_log(data);
                                    }
                                }
                                _ => {}
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
