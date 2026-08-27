//! Python 子进程管理：spawn / wait / kill
//!
//! 通过 NDJSON 协议与 Worker 通信：stdin 写入命令/取消通知，stdout 逐行读取响应与事件，
//! stderr 转发到 tracing，health task 监听子进程退出并回传退出码。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::bridge::ipc::{CancelNotification, IpcEvent, IpcRequest, IpcResponse, IpcResult};
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
    /// Job Object 句柄（Windows）：Drop 时关闭句柄触发内核回收 Worker 进程树
    /// （含 chromium），主进程被强杀时同样生效（M3 进程树治理第一层防线）
    #[cfg(windows)]
    job: Option<crate::bridge::job::JobHandle>,
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
                // Windows：先关 Job 句柄让内核立即回收整棵进程树（Worker 内
                // Playwright 拉起的 chromium 与 Worker 本身），与 kill_tx 直杀
                // 双保险；无 Job 时仅靠 kill_tx + orphan 兜底
                #[cfg(windows)]
                if let Some(job) = self.job.as_mut() {
                    job.terminate_tree();
                }
                if let Some(tx) = self.kill_tx.take() {
                    let _ = tx.send(());
                }
                // kill 后等待 health task 退出同样加超时：Windows 上目标进程
                // 被 AV/驱动句柄卡住时 child.wait() 可能长期不返回，
                // 无限等待会卡死 Supervisor 主循环的 shutdown/kill 分支
                if tokio::time::timeout(Duration::from_secs(2), &mut self.handles.health_task)
                    .await
                    .is_err()
                {
                    tracing::warn!("kill 后等待 Worker 退出超时（2s），放弃等待");
                }
            }
        }
        self.handles.stdin_task.abort();
        self.handles.stdout_task.abort();
        self.handles.stderr_task.abort();
        // self drop 时关闭 Job 句柄（若仍持有）：正常退出路径下 Worker 已自行
        // 关闭浏览器，此处仅回收漏网进程
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

    // Windows：spawn 后立即加入 KILL_ON_JOB_CLOSE 的 Job Object（内核级进程树
    // 回收，M3）。失败仅告警退回应用层清理（kill_on_drop + orphan.rs 兜底）。
    #[cfg(windows)]
    let job = crate::bridge::job::try_assign_job(&child);

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
        #[cfg(windows)]
        job,
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
                        tracing::warn!("IPC 行超长，已丢弃: {} 字节", line_buf.len() + pos);
                        // G18：保留有界前缀（至多补齐到上限）供请求 id 提取，
                        // 结算对应在途请求，避免其挂满 execute 超时
                        let keep = IPC_MAX_LINE_LEN.saturating_sub(line_buf.len()).min(pos);
                        line_buf.extend_from_slice(&available[..keep]);
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
                        // G18：同上，保留有界前缀供请求 id 提取
                        let keep = IPC_MAX_LINE_LEN
                            .saturating_sub(line_buf.len())
                            .min(available.len());
                        line_buf.extend_from_slice(&available[..keep]);
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
            // G18：超限行无法作为响应整体解析，但保留的有界前缀仍可能携带
            // 请求 id——构造错误 IpcResponse 结算对应在途请求（仿 worker_main
            // 的 _oversized_request_id），否则该请求要挂满 execute 超时才返回。
            settle_oversized_line(&line_buf, &ipc_tx).await;
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
        Err(e) => {
            // 非 JSON 行意味着 stdout 被第三方库的意外 print 污染：
            // 若该行本是响应会超时才被发现，累计计数供 Worker 退出时汇总告警（M6）
            INVALID_IPC_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("非 JSON IPC 行: {e} | {trimmed}");
        }
    }
}

/// 从超限响应行的有界前缀提取请求 id（G18）
///
/// 仿 worker_main `_oversized_request_id`：`IpcResponse` 序列化时 `id` 位于
/// 行首附近，仅扫描前 512 字符即足够；用手写扫描替代引入 regex 依赖。
/// 仅接受 `{"id": N` 起始形态（忽略空白），避免误提取正文中的其他 id 字段。
fn extract_leading_id(head: &str) -> Option<u64> {
    let b = head.as_bytes();
    let mut i = 0usize;
    let skip_ws = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    i = skip_ws(i);
    if i >= b.len() || b[i] != b'{' {
        return None;
    }
    i = skip_ws(i + 1);
    if !head[i..].starts_with("\"id\"") {
        return None;
    }
    i = skip_ws(i + 4);
    if i >= b.len() || b[i] != b':' {
        return None;
    }
    i = skip_ws(i + 1);
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    head[start..i].parse().ok()
}

/// 超长 IPC 响应行的结算（G18）
///
/// 从有界前缀提取请求 id，构造错误 [`IpcResponse`]（消息说明超出单行上限）
/// 回传 Supervisor 结算对应在途请求；提取不到 id 时无法定位请求，仅保留
/// 上方读取路径的 warn 日志。
async fn settle_oversized_line(prefix: &[u8], ipc_tx: &mpsc::Sender<ParsedMessage>) {
    let head = &prefix[..prefix.len().min(512)];
    let head = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(target: "python_worker", "超长 IPC 行前缀非 UTF-8，无法提取请求 id");
            return;
        }
    };
    let Some(id) = extract_leading_id(head) else {
        tracing::warn!(target: "python_worker", "超长 IPC 行前缀无法提取请求 id，在途请求可能需等待超时");
        return;
    };
    tracing::warn!(target: "python_worker", "IPC 响应超长（>1MiB），以错误结算请求 id={id}");
    let resp = IpcResponse {
        id,
        result: IpcResult {
            success: false,
            data: serde_json::Value::Null,
            error: Some(format!(
                "IPC 响应超过 {} MiB 单行上限，已丢弃",
                IPC_MAX_LINE_LEN / (1024 * 1024)
            )),
        },
    };
    let _ = ipc_tx.send(ParsedMessage::Response(resp)).await;
}

/// 非 JSON IPC 行计数（进程级：同一时刻仅一个 Worker，语义足够）
static INVALID_IPC_LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 取走累计的非 JSON IPC 行计数（读后复位）
pub fn take_invalid_ipc_line_count() -> u64 {
    INVALID_IPC_LINES.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// stderr forwarder task：逐行转发到 tracing（最后的日志防线）
///
/// 解析 Python Worker（stdlib `logging`，格式 `时间 级别 [名称] 消息`）
/// 日志行中的级别字段（如 `INFO` / `WARNING` / `ERROR`），按实际级别调用
/// 对应的 tracing 宏，避免所有 stderr 输出都被误记为 WARN。
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

/// 解析 Python Worker 的 stdlib logging 行（`时间 级别 [名称] 消息`），
/// 去掉已经由 Rust 日志层提供的时间和级别前缀。
/// 返回 `(级别, 消息)`；格式异常时保留原文并让上层按 WARN 处理。
fn parse_worker_stderr_line(line: &str) -> (String, String) {
    let trimmed = line.trim_end();
    let mut fields = trimmed.splitn(4, ' ');
    let date = fields.next();
    let time = fields.next();
    let level = fields.next();
    let message = fields.next().map(str::trim).filter(|s| !s.is_empty());
    if date.is_some() && time.is_some() && level.is_some() && message.is_some() {
        return (
            level.unwrap_or_default().to_ascii_uppercase(),
            message.unwrap_or_default().to_string(),
        );
    }
    (String::new(), trimmed.to_string())
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
    let (level, message) = parse_worker_stderr_line(trimmed);
    match level.as_str() {
        "TRACE" | "DEBUG" => tracing::debug!(target: "python_worker", "{message}"),
        "INFO" => tracing::info!(target: "python_worker", "{message}"),
        "WARNING" | "WARN" => tracing::warn!(target: "python_worker", "{message}"),
        "ERROR" => tracing::error!(target: "python_worker", "{message}"),
        "CRITICAL" | "FATAL" => tracing::error!(target: "python_worker", "{message}"),
        _ => tracing::warn!(target: "python_worker", "{message}"),
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

    #[test]
    fn test_parse_worker_stderr_line_removes_duplicate_prefix() {
        let (level, message) =
            parse_worker_stderr_line("2026-07-09 21:39:57,938 INFO [login] 页面加载完成");
        assert_eq!(level, "INFO");
        assert_eq!(message, "[login] 页面加载完成");

        let (level, message) = parse_worker_stderr_line("worker crashed");
        assert!(level.is_empty());
        assert_eq!(message, "worker crashed");
    }

    /// 解析一行并取回 channel 中的下一条消息
    async fn parse_line(line: &str) -> Option<ParsedMessage> {
        let (tx, mut rx) = mpsc::channel(8);
        parse_and_send_line(line, &tx).await;
        rx.recv().await
    }

    #[tokio::test]
    async fn test_parse_valid_response() {
        let msg =
            parse_line(r#"{"id":1,"result":{"success":true,"data":{"ok":true},"error":null}}"#)
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
        let msg = parse_line(r#"{"event":"step_progress","data":{"step":2}}"#)
            .await
            .unwrap();
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
        let msg = parse_line(r#"{"id":42,"result":{"success":"not-bool"}}"#)
            .await
            .unwrap();
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

    // ============ G18：超长响应行结算 ============

    #[test]
    fn test_extract_leading_id_valid_forms() {
        // 标准形态（忽略空白）
        assert_eq!(extract_leading_id(r#"{"id":42,"result":{}}"#), Some(42));
        assert_eq!(extract_leading_id(r#"  {  "id" :  7  }"#), Some(7));
        assert_eq!(extract_leading_id(r#"{"id":0}"#), Some(0));
    }

    #[test]
    fn test_extract_leading_id_rejects_non_leading_or_invalid() {
        // id 不在行首附近（前面有其他字段）→ 拒绝，避免误提取正文中的 id
        assert_eq!(extract_leading_id(r#"{"result":{"id":42}}"#), None);
        // 非 JSON / 缺冒号 / 缺数字
        assert_eq!(extract_leading_id("not json"), None);
        assert_eq!(extract_leading_id(r#"{"id" 42}"#), None);
        assert_eq!(extract_leading_id(r#"{"id":"abc"}"#), None);
        // 空串
        assert_eq!(extract_leading_id(""), None);
    }

    /// G18：超限行前缀携带 id → 构造错误 IpcResponse 结算该请求
    #[tokio::test]
    async fn test_settle_oversized_line_sends_error_response() {
        let (tx, mut rx) = mpsc::channel(8);
        let prefix = br#"{"id":99,"result":{"success":true,"data":"AAAA"#;
        settle_oversized_line(prefix, &tx).await;
        match rx.recv().await {
            Some(ParsedMessage::Response(resp)) => {
                assert_eq!(resp.id, 99);
                assert!(!resp.result.success);
                let err = resp.result.error.expect("应携带错误消息");
                assert!(err.contains("1 MiB"), "错误消息应说明单行上限: {err}");
            }
            other => panic!("期望 Response，得到 {other:?}"),
        }
    }

    /// G18：前缀提取不到 id → 不产生任何消息（仅 warn）
    #[tokio::test]
    async fn test_settle_oversized_line_without_id_is_silent() {
        let (tx, mut rx) = mpsc::channel(8);
        settle_oversized_line(b"garbage without id field", &tx).await;
        assert!(rx.try_recv().is_err());
    }
}
