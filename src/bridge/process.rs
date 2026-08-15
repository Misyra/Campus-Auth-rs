//! Python 子进程管理：spawn / wait / kill
//!
//! 通过 NDJSON 协议与 Worker 通信：stdin 写入命令/取消通知，stdout 逐行读取响应与事件，
//! stderr 转发到 tracing，health task 监听子进程退出并回传退出码。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, ChildStderr, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::bridge::ipc::{CancelNotification, IpcEvent, IpcRequest, IpcResponse};
use crate::bridge::{BridgeError, IPC_DELIMITER, IPC_MAX_LINE_LEN, IPC_WRITE_CHANNEL_CAP};

/// 发送给 stdin writer task 的消息
pub enum IpcMessage {
    /// 常规命令请求
    Request(IpcRequest),
    /// 取消通知
    Cancel(CancelNotification),
}

/// Worker 子进程回传给 Supervisor 主循环的解析消息
#[derive(Debug)]
pub enum ParsedMessage {
    /// 正常的请求响应
    Response(IpcResponse),
    /// 带有效 id 但反序列化为 [`IpcResponse`] 失败的响应
    ///
    /// 携带原始 id，供 Supervisor 以错误回收对应的在途请求，避免请求永久泄漏、
    /// 调试会话槽位卡死（历史遗留 F1）。
    ResponseError { id: u64, error: String },
    /// 事件推送
    Event(IpcEvent),
    /// 无法解析的 IPC 行
    InvalidLine(String),
    /// 子进程退出（exit code；-1 表示未知）
    WorkerExited(i32),
}

/// 活跃的 Python 子进程封装
pub struct WorkerProcess {
    /// 写入 stdin 的通道
    pub stdin_tx: mpsc::Sender<IpcMessage>,
    /// 四个后台 task 句柄
    pub handles: ProcessHandles,
    /// 强制退出信号（消耗式）
    kill_tx: Option<oneshot::Sender<()>>,
}

impl WorkerProcess {
    /// 优雅关闭：先等待 health task 正常退出（Worker 收到 shutdown 后自行退出），
    /// 超时则通过 kill_tx 强杀；最后 abort 其余后台 task。
    pub async fn shutdown(mut self, timeout: Duration) {
        let health = &mut self.handles.health_task;
        match tokio::time::timeout(timeout, health).await {
            // 已优雅退出
            Ok(_) => {}
            // 超时：强制杀死子进程
            Err(_) => {
                if let Some(tx) = self.kill_tx.take() {
                    let _ = tx.send(());
                }
                let _ = self.handles.health_task.await;
            }
        }
        self.handles.stdin_task.abort();
        self.handles.stdout_task.abort();
        self.handles.stderr_task.abort();
    }
}

/// 子进程相关的后台 task 句柄
pub struct ProcessHandles {
    /// stdin writer task
    pub stdin_task: JoinHandle<()>,
    /// stdout reader task
    pub stdout_task: JoinHandle<()>,
    /// stderr 转发 task
    pub stderr_task: JoinHandle<()>,
    /// 健康监控（Child::wait）task
    pub health_task: JoinHandle<()>,
}

/// spawn Python Worker 子进程，并启动 stdin/stdout/stderr/health 四个后台 task。
///
/// `ipc_tx` 由 Supervisor 主循环持有其对应的 Receiver，用于回收响应/事件/退出通知。
/// `base_path` 注入给 Worker，供其锚定浏览器持久化数据目录（`<base_path>/config/browser-data`），
/// 避免依赖 Worker 脚本目录（便携包更新/重建时会被清空）。
pub async fn spawn_worker(
    python_exe: &Path,
    worker_main: &Path,
    base_path: &Path,
    ipc_tx: mpsc::Sender<ParsedMessage>,
) -> Result<WorkerProcess, BridgeError> {
    let mut cmd = Command::new(python_exe);
    cmd.arg(worker_main)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // 强制 Python 全层级 UTF-8（stdio/文件系统默认编码）：
        // 管道重定向下 stdio 默认走 ANSI 代码页（简中为 cp936），
        // 与本模块严格按 UTF-8 解码 IPC 行的约定冲突（Worker 侧另做 reconfigure 双保险）
        .env("PYTHONUTF8", "1")
        // 注入应用数据目录，Worker 据此锚定浏览器持久化数据（browser-data）
        .env("CAMPUS_AUTH_BASE_PATH", base_path);
    // Windows：设置 CREATE_NO_WINDOW，避免 Worker 子进程弹出黑色控制台窗口
    // （与 orphan.rs 的进程枚举保持一致的处理，历史遗留 F4-Win）
    #[cfg(windows)]
    {
        // tokio::process::Command 在 Windows 上提供同名 inherent 方法，无需引入 trait
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(BridgeError::SpawnFailed)?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| BridgeError::Internal("stdin 未 piped".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BridgeError::Internal("stdout 未 piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BridgeError::Internal("stderr 未 piped".into()))?;

    // stdin writer：IpcMessage channel → Worker stdin
    let (stdin_tx, stdin_rx) = mpsc::channel::<IpcMessage>(IPC_WRITE_CHANNEL_CAP);
    let stdin_task = tokio::spawn(stdin_writer_task(stdin_rx, stdin));

    // stdout reader：逐行解析 → 回传 Supervisor
    let stdout_task = tokio::spawn(stdout_reader_task(stdout, ipc_tx.clone()));

    // stderr 转发：行 → tracing（target=python_worker）
    let stderr_task = tokio::spawn(stderr_forwarder_task(stderr));

    // health monitor：Child::wait() → 退出通知
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let health_task = tokio::spawn(health_monitor_task(child, kill_rx, ipc_tx));

    Ok(WorkerProcess {
        stdin_tx,
        handles: ProcessHandles {
            stdin_task,
            stdout_task,
            stderr_task,
            health_task,
        },
        kill_tx: Some(kill_tx),
    })
}

/// stdin writer task：从 channel 读取 IpcMessage 序列化为 NDJSON 写入 Worker stdin
async fn stdin_writer_task(mut rx: mpsc::Receiver<IpcMessage>, mut stdin: ChildStdin) {
    while let Some(msg) = rx.recv().await {
        let value = match &msg {
            IpcMessage::Request(r) => serde_json::to_value(r),
            IpcMessage::Cancel(c) => serde_json::to_value(c),
        };
        let value = match value {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("IPC 消息序列化失败: {e}");
                continue;
            }
        };
        let mut bytes = match serde_json::to_string(&value) {
            Ok(s) => s.into_bytes(),
            Err(e) => {
                tracing::error!("IPC 消息序列化失败: {e}");
                continue;
            }
        };
        bytes.push(IPC_DELIMITER);
        if let Err(e) = stdin.write_all(&bytes).await {
            tracing::error!("IPC 写入 stdin 失败: {e}（Worker 可能已退出）");
            break;
        }
        if let Err(e) = stdin.flush().await {
            tracing::error!("IPC flush stdin 失败: {e}");
            break;
        }
    }
}

/// stdout reader task：逐行读取 Worker stdout，解析为响应/事件/非法行并回传
///
/// 使用 `fill_buf`/`consume` 模式逐块读取，在读取过程中检查累计长度，
/// 超长行直接丢弃后续字节，避免将整行加载到内存后才检查（防止 OOM）。
async fn stdout_reader_task(stdout: ChildStdout, ipc_tx: mpsc::Sender<ParsedMessage>) {
    let mut reader = BufReader::new(stdout);
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let mut exceeded = false;
        let mut found_newline = false;

        // 使用 fill_buf/consume 逐块读取，限制单行最大长度防止 OOM
        loop {
            let available = match reader.fill_buf().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("IPC 读取 stdout 失败: {e}");
                    return;
                }
            };

            if available.is_empty() {
                // EOF
                break;
            }

            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    // 找到换行符
                    if !exceeded && line_buf.len() + pos > IPC_MAX_LINE_LEN {
                        exceeded = true;
                        tracing::warn!(
                            "IPC 行超长，已丢弃: {} 字节",
                            line_buf.len() + pos
                        );
                    }
                    if !exceeded {
                        line_buf.extend_from_slice(&available[..pos]);
                    }
                    reader.consume(pos + 1);
                    found_newline = true;
                    break;
                }
                None => {
                    // 当前缓冲区中无换行符
                    if !exceeded && line_buf.len() + available.len() > IPC_MAX_LINE_LEN {
                        exceeded = true;
                        tracing::warn!("IPC 行超长，已丢弃");
                    }
                    if !exceeded {
                        line_buf.extend_from_slice(available);
                    }
                    let len = available.len();
                    reader.consume(len);
                }
            }
        }

        if !found_newline {
            // EOF 且无更多完整行
            // 处理缓冲区中剩余的数据（最后一行可能没有换行符）
            if !exceeded && !line_buf.is_empty() {
                if let Ok(line_str) = std::str::from_utf8(&line_buf) {
                    let trimmed = line_str.trim_end_matches(['\n', '\r']);
                    if !trimmed.is_empty() {
                        parse_and_send_line(trimmed, &ipc_tx).await;
                    }
                }
            }
            break;
        }

        if exceeded {
            continue;
        }

        if line_buf.is_empty() {
            continue;
        }

        let line_str = match std::str::from_utf8(&line_buf) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("IPC 行非 UTF-8: {e}");
                continue;
            }
        };

        let trimmed = line_str.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }

        parse_and_send_line(trimmed, &ipc_tx).await;
    }
}

/// 解析单行 IPC JSON 并发送到 Supervisor
async fn parse_and_send_line(trimmed: &str, ipc_tx: &mpsc::Sender<ParsedMessage>) {
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            if let Some(id_val) = v.get("id") {
                // 先提取 id，确保后续反序列化失败时仍能回收在途请求（历史遗留 F1）
                let id = id_val.as_u64();
                match serde_json::from_value::<IpcResponse>(v) {
                    Ok(resp) => {
                        let _ = ipc_tx.send(ParsedMessage::Response(resp)).await;
                    }
                    Err(e) => match id {
                        // 带有效 id：回传 ResponseError，由 Supervisor 以错误结束在途请求，
                        // 避免调用方永久阻塞、调试会话槽位卡死。
                        Some(id) => {
                            let _ = ipc_tx
                                .send(ParsedMessage::ResponseError {
                                    id,
                                    error: e.to_string(),
                                })
                                .await;
                        }
                        // id 非法（非 u64）：无法定位在途请求，仅记录
                        None => tracing::warn!("IPC 响应解析失败且 id 非法: {e} | {trimmed}"),
                    },
                }
            } else if v.get("event").is_some() {
                match serde_json::from_value::<IpcEvent>(v) {
                    Ok(ev) => {
                        let _ = ipc_tx.send(ParsedMessage::Event(ev)).await;
                    }
                    Err(e) => tracing::warn!("IPC 事件解析失败: {e}"),
                }
            } else {
                tracing::warn!("未知 IPC 消息格式: {trimmed}");
            }
        }
        Err(e) => tracing::warn!("非 JSON IPC 行: {e} | {trimmed}"),
    }
}

/// stderr forwarder task：逐行转发到 tracing（最后的日志防线）
///
/// 解析 Python loguru 日志行中的级别字段（如 `INFO` / `WARNING` / `ERROR`），
/// 按实际级别调用对应的 tracing 宏，避免所有 stderr 输出都被误记为 WARN。
/// 非日志行（无级别字段）回退为 WARN。
///
/// 使用 `fill_buf`/`consume` 逐块读取并施加行长度限制（`IPC_MAX_LINE_LEN`），
/// 超长行直接丢弃，与 stdout reader 一致，防止异常输出导致 OOM。
async fn stderr_forwarder_task(stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut line_buf = Vec::new();
    loop {
        line_buf.clear();
        let mut exceeded = false;
        let mut found_newline = false;
        // 逐块读取直到换行符，限制单行最大长度防止 OOM
        loop {
            let available = match reader.fill_buf().await {
                Ok(b) => b,
                Err(_) => return,
            };
            if available.is_empty() {
                break;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    if !exceeded && line_buf.len() + pos > IPC_MAX_LINE_LEN {
                        exceeded = true;
                        tracing::warn!(target: "python_worker", "stderr 行超长，已丢弃");
                    }
                    if !exceeded {
                        line_buf.extend_from_slice(&available[..pos]);
                    }
                    reader.consume(pos + 1);
                    found_newline = true;
                    break;
                }
                None => {
                    if !exceeded && line_buf.len() + available.len() > IPC_MAX_LINE_LEN {
                        exceeded = true;
                        tracing::warn!(target: "python_worker", "stderr 行超长，已丢弃");
                    }
                    if !exceeded {
                        line_buf.extend_from_slice(available);
                    }
                    let len = available.len();
                    reader.consume(len);
                }
            }
        }
        if !found_newline {
            // EOF：处理缓冲区中剩余的最后一行后退出
            if !exceeded && !line_buf.is_empty() {
                log_stderr_line(&line_buf);
            }
            return;
        }
        if !exceeded && !line_buf.is_empty() {
            log_stderr_line(&line_buf);
        }
    }
}

/// 按日志级别转发单行 stderr 到 tracing
fn log_stderr_line(line: &[u8]) {
    let s = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => return,
    };
    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        return;
    }
    // loguru 格式: "2026-07-09 21:39:57,938 INFO [module] message"
    // 取第 3 个空白分隔字段作为级别
    let level = trimmed
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .to_ascii_uppercase();
    match level.as_str() {
        "TRACE" | "DEBUG" => tracing::debug!(target: "python_worker", "{trimmed}"),
        "INFO" => tracing::info!(target: "python_worker", "{trimmed}"),
        "WARNING" | "WARN" => tracing::warn!(target: "python_worker", "{trimmed}"),
        "ERROR" => tracing::error!(target: "python_worker", "{trimmed}"),
        "CRITICAL" | "FATAL" => tracing::error!(target: "python_worker", "{trimmed}"),
        _ => tracing::warn!(target: "python_worker", "{trimmed}"),
    }
}

/// health monitor task：阻塞等待子进程退出；收到 kill 信号则强杀后退出
async fn health_monitor_task(
    mut child: Child,
    mut kill_rx: oneshot::Receiver<()>,
    ipc_tx: mpsc::Sender<ParsedMessage>,
) {
    let code = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(e) => {
                    tracing::error!("Worker 进程等待失败: {e}");
                    -1
                }
            }
        }
        _ = &mut kill_rx => {
            let _ = child.start_kill();
            // 等待退出以释放句柄
            let _ = child.wait().await;
            -1
        }
    };
    let _ = ipc_tx.send(ParsedMessage::WorkerExited(code)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析一行并取回 channel 中的下一条消息
    async fn parse_line(line: &str) -> Option<ParsedMessage> {
        let (tx, mut rx) = mpsc::channel(8);
        parse_and_send_line(line, &tx).await;
        rx.recv().await
    }

    #[tokio::test]
    async fn test_parse_valid_response() {
        let msg = parse_line(r#"{"id":1,"result":{"success":true,"data":{"ok":true},"error":null}}"#)
            .await
            .unwrap();
        match msg {
            ParsedMessage::Response(resp) => {
                assert_eq!(resp.id, 1);
                assert!(resp.result.success);
                assert_eq!(resp.result.data["ok"], true);
            }
            other => panic!("期望 Response，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_parse_event() {
        let msg = parse_line(r#"{"event":"step_progress","data":{"step":2}}"#).await.unwrap();
        match msg {
            ParsedMessage::Event(ev) => {
                assert_eq!(ev.event, "step_progress");
                assert_eq!(ev.data["step"], 2);
            }
            other => panic!("期望 Event，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_parse_response_with_invalid_body_recovers_id() {
        // 响应体反序列化失败但 id 有效 → ResponseError，Supervisor 可回收在途请求（历史遗留 F1）
        let msg = parse_line(r#"{"id":42,"result":{"success":"not-bool"}}"#).await.unwrap();
        match msg {
            ParsedMessage::ResponseError { id, .. } => {
                assert_eq!(id, 42);
            }
            other => panic!("期望 ResponseError，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_parse_non_json_line_is_ignored() {
        let (tx, mut rx) = mpsc::channel(8);
        // 非 JSON 行只记日志，不产生消息
        parse_and_send_line("this is not json", &tx).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_parse_event_with_invalid_body_is_ignored() {
        let (tx, mut rx) = mpsc::channel(8);
        // event 字段存在但类型非法（event 应为 String，此处为 number）→ 反序列化失败，只记日志
        parse_and_send_line(r#"{"event":123}"#, &tx).await;
        assert!(rx.try_recv().is_err());
    }
}
