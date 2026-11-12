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
use crate::utils::io_human::io_kind_label;

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
    ResponseError {
        /// 关联命令的请求 ID。
        id: u64,
        /// Worker 返回的错误信息。
        error: String,
    },
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
                    tracing::warn!("kill 后等待 Worker 退出超时（2s），中止健康监控任务");
                    self.handles.health_task.abort();
                    let _ = self.handles.health_task.await;
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
/// `keep_alive` 注入为环境变量，Worker 据此决定浏览器常驻策略（登录成功保留
/// 页面与登录状态、非成功终态会话级释放、不装浏览器空闲回收计时器）。
pub async fn spawn_worker(
    python_exe: &Path,
    worker_main: &Path,
    base_path: &Path,
    keep_alive: bool,
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
    if keep_alive {
        cmd.env("CAMPUS_AUTH_WORKER_KEEP_ALIVE", "1");
    }
    // Windows：设置 CREATE_NO_WINDOW，避免 Worker 子进程弹出黑色控制台窗口
    // （与 orphan.rs 的进程枚举保持一致的处理，历史遗留 F4-Win）
    #[cfg(windows)]
    {
        // tokio::process::Command 在 Windows 上提供同名 inherent 方法，无需引入 trait
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // spawn 失败载荷用「失败类别 + 原始错误」人话化（io_error 的「XX失败：」句式
    // 会与变体 Display 的「Worker 进程启动失败：」重复，故只拼类别短语）
    let mut child = cmd.spawn().map_err(|e| {
        BridgeError::SpawnFailed(std::io::Error::new(
            e.kind(),
            format!("{}（{e}）", io_kind_label(e.kind())),
        ))
    })?;
    tracing::info!(pid = ?child.id(), "Worker 子进程已启动");

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
        // 序列化（to_value + to_string）任一失败统一记为一条 warn（内部错误，
        // 不值得 error 级别），kind 字段区分 Request/Cancel 阶段
        let kind = match &msg {
            IpcMessage::Request(_) => "request",
            IpcMessage::Cancel(_) => "cancel",
        };
        let serialized = match &msg {
            IpcMessage::Request(r) => serde_json::to_value(r),
            IpcMessage::Cancel(c) => serde_json::to_value(c),
        }
        .and_then(|v| serde_json::to_string(&v));
        let bytes = match serialized {
            Ok(s) => {
                let mut b = s.into_bytes();
                b.push(IPC_DELIMITER);
                b
            }
            Err(e) => {
                tracing::warn!(kind, error = %e, "IPC 消息序列化失败，跳过该消息");
                continue;
            }
        };
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
                        tracing::warn!(len = line_buf.len() + pos, "IPC stdout 行超长，已丢弃");
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
                        tracing::warn!(
                            len = line_buf.len() + available.len(),
                            "IPC stdout 行超长，已丢弃"
                        );
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
            if !exceeded
                && !line_buf.is_empty()
                && let Ok(line_str) = std::str::from_utf8(&line_buf)
            {
                let trimmed = line_str.trim_end_matches(['\n', '\r']);
                if !trimmed.is_empty() {
                    parse_and_send_line(trimmed, &ipc_tx).await;
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

/// 原始 IPC 行写入日志的最大预览长度：单行可达 1MiB 且可能携带凭据，
/// 整行入日志存在泄露与刷屏风险，超长部分截断
const IPC_LOG_PREVIEW_LEN: usize = 200;

/// 截取原始 IPC 行的日志预览（按 UTF-8 字符边界安全截断）
fn log_line_preview(line: &str) -> &str {
    if line.len() <= IPC_LOG_PREVIEW_LEN {
        return line;
    }
    let mut end = IPC_LOG_PREVIEW_LEN;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
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
                        None => tracing::warn!(
                            line_len = trimmed.len(),
                            "IPC 响应解析失败且 id 非法: {e} | {}",
                            log_line_preview(trimmed)
                        ),
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
                tracing::warn!(
                    line_len = trimmed.len(),
                    "未知 IPC 消息格式: {}",
                    log_line_preview(trimmed)
                );
            }
        }
        Err(e) => {
            // 非 JSON 行意味着 stdout 被第三方库的意外 print 污染：
            // 若该行本是响应会超时才被发现，累计计数供 Worker 退出时汇总告警（M6）
            INVALID_IPC_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(
                line_len = trimmed.len(),
                "非 JSON IPC 行: {e} | {}",
                log_line_preview(trimmed)
            );
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
/// 无级别前缀的续行（如 traceback 堆栈）继承前一行级别（仅在 ERROR/CRITICAL
/// 之后继承），其余非日志行回退 WARN。
///
/// 使用 `fill_buf`/`consume` 逐块读取并施加行长度限制（`IPC_MAX_LINE_LEN`），
/// 超长行直接丢弃，与 stdout reader 一致，防止异常输出导致 OOM。
async fn stderr_forwarder_task(stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut line_buf = Vec::new();
    // 上一有效行是否为 ERROR/CRITICAL：供无格式续行继承级别
    let mut last_was_error = false;
    loop {
        line_buf.clear();
        let mut exceeded = false;
        let mut found_newline = false;
        // 逐块读取直到换行符，限制单行最大长度防止 OOM
        loop {
            let available = match reader.fill_buf().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("读取 Worker stderr 失败，stderr 转发终止: {e}");
                    return;
                }
            };
            if available.is_empty() {
                break;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    if !exceeded && line_buf.len() + pos > IPC_MAX_LINE_LEN {
                        exceeded = true;
                        tracing::warn!(target: "python_worker", len = line_buf.len() + pos, "IPC stderr 行超长，已丢弃");
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
                        tracing::warn!(target: "python_worker", len = line_buf.len() + available.len(), "IPC stderr 行超长，已丢弃");
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
            // EOF：处理缓冲区中剩余的最后一行后退出（无需回传继承状态）
            if !exceeded && !line_buf.is_empty() {
                log_stderr_line(&line_buf, last_was_error);
            }
            return;
        }
        if !exceeded && !line_buf.is_empty() {
            last_was_error = log_stderr_line(&line_buf, last_was_error);
        }
    }
}

/// 解析 Python Worker 的 stdlib logging 行（`时间 级别 [名称] 消息`），
/// 去掉已经由 Rust 日志层提供的时间和级别前缀。
/// 返回 `(级别, 消息)`；级别词必须命中已知档位才算解析成功，
/// 否则整行视为无格式续行（如 traceback 堆栈行）原样返回，由上层继承前一行级别
///（旧的宽松解析会把消息文本误当级别切掉，如 `  File "x.py" ...` 变成级别 FILE）。
fn parse_worker_stderr_line(line: &str) -> (String, String) {
    const KNOWN_LEVELS: [&str; 8] = [
        "TRACE", "DEBUG", "INFO", "WARNING", "WARN", "ERROR", "CRITICAL", "FATAL",
    ];
    let trimmed = line.trim_end();
    let mut fields = trimmed.splitn(4, ' ');
    let date = fields.next();
    let time = fields.next();
    let level = fields.next();
    let message = fields.next().map(str::trim).filter(|s| !s.is_empty());
    if let (Some(_date), Some(_time), Some(level), Some(message)) = (date, time, level, message) {
        let upper = level.to_ascii_uppercase();
        if KNOWN_LEVELS.contains(&upper.as_str()) {
            return (upper, message.to_string());
        }
    }
    (String::new(), trimmed.to_string())
}

/// 按日志级别转发单行 stderr 到 tracing
///
/// 返回本行是否为 ERROR/CRITICAL（供后续无格式续行继承级别）；
/// `prev_was_error` 为继承输入，空行与 UTF-8 无效行不改变继承状态。
fn log_stderr_line(line: &[u8], prev_was_error: bool) -> bool {
    let s = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => return prev_was_error,
    };
    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        return prev_was_error;
    }
    let (level, message) = parse_worker_stderr_line(trimmed);
    match level.as_str() {
        "TRACE" | "DEBUG" => {
            tracing::debug!(target: "python_worker", "{message}");
            false
        }
        "INFO" => {
            tracing::info!(target: "python_worker", "{message}");
            false
        }
        "WARNING" | "WARN" => {
            tracing::warn!(target: "python_worker", "{message}");
            false
        }
        "ERROR" => {
            tracing::error!(target: "python_worker", "{message}");
            true
        }
        "CRITICAL" | "FATAL" => {
            tracing::error!(target: "python_worker", "{message}");
            true
        }
        // 无格式续行：仅 ERROR/CRITICAL 之后继承（traceback 堆栈不断尾），其余维持 WARN 兜底
        "" if prev_was_error => {
            tracing::error!(target: "python_worker", "{message}");
            true
        }
        _ => {
            tracing::warn!(target: "python_worker", "{message}");
            false
        }
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
    // Supervisor 可能正在同一任务内等待 `shutdown()`；若 bounded IPC channel 已满，
    // 直接 await send 会让 shutdown 与 health task 互相等待。通知交给独立短任务，
    // health task 先结束；receiver 关闭时发送任务会自然收敛。
    tokio::spawn(async move {
        let _ = ipc_tx.send(ParsedMessage::WorkerExited(code)).await;
    });
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

    #[test]
    fn test_parse_worker_stderr_line_strict_level() {
        // 级别词未命中已知档位时整行按无格式续行原样保留，
        // 防止 traceback 行（`  File "x.py", line 10`）被误切出 bogus 级别
        let line = "  File \"worker.py\", line 10, in run";
        let (level, message) = parse_worker_stderr_line(line);
        assert!(level.is_empty());
        assert_eq!(message, line);

        // 大小写不敏感命中
        let (level, _) = parse_worker_stderr_line("2026-07-09 21:39:57,938 error [t] boom");
        assert_eq!(level, "ERROR");
    }

    #[test]
    fn test_log_stderr_line_error_continuation_inheritance() {
        // ERROR 首行 → 续行继承 ERROR（traceback 堆栈不断尾）
        assert!(log_stderr_line(
            b"2026-09-08 10:00:00,000 ERROR [task] worker crashed",
            false
        ));
        assert!(log_stderr_line(
            b"  File \"worker.py\", line 10, in run",
            true
        ));
        // 空行不改变继承状态（堆栈块中常见）
        assert!(log_stderr_line(b"   ", true));
        // 其他级别的格式行结束继承
        assert!(!log_stderr_line(
            b"2026-09-08 10:00:01,000 WARNING [task] recovered",
            true
        ));
        // 非错误上下文的无前缀行维持 WARN 兜底语义
        assert!(!log_stderr_line(b"random noise without prefix", false));
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

    // ============ 属性测试（proptest）：随机输入下解析器永不 panic、产出有界 ============
    //
    // 生成器策略覆盖面：
    // - 任意字符串（乱码 / 半截 JSON / 非 JSON 文本）；字节级任意序列单独覆盖 settle 路径
    // - 合法 JSON 值（嵌套对象 / 数组 / 非 JSON-RPC 形状 / 超大字段 / 任意 Unicode）
    // - 形似响应信封（合法与畸形 result、杂散外字段、id 与 event 并存）
    // - 形似事件信封（event 名为任意字符串）
    // - 空行 / 纯空白 / CRLF 残留（读侧已 trim 的边界形态）
    //
    // 注：`arb_json_value` 与 src/login/session.rs 测试模块中的同名策略互为镜像，
    // 各自独立维护（跨模块共用会引入生产文件上的 cfg(test) 桥）。

    use proptest::prelude::*;

    /// 属性测试统一口径：512 cases（CI 亲和，无秒级超时；随机失败必可 shrink 复现）
    fn prop_config() -> proptest::test_runner::Config {
        proptest::test_runner::Config::with_cases(512)
    }

    /// 递归 JSON 值策略核心：对象 / 数组 / 字符串（任意 Unicode）/ 数值 /
    /// 布尔 / null，嵌套至多 4 层；键名混入协议关键字以提高「形似信封」密度。
    /// `allow_floats=false` 时数值叶子仅限整数（供 roundtrip 类属性使用，
    /// 见 [`arb_json_value_exact`] 文档）。
    fn arb_json_value_impl(
        allow_floats: bool,
    ) -> proptest::strategy::BoxedStrategy<serde_json::Value> {
        // 数值叶子：allow_floats 时混入任意有限浮点（非有限浮点进不了 serde_json）
        let numeric: proptest::strategy::BoxedStrategy<serde_json::Value> = if allow_floats {
            any::<f64>()
                .prop_filter("非有限浮点不可序列化", |f| f.is_finite())
                .prop_map(|f| serde_json::json!(f))
                .boxed()
        } else {
            any::<i64>().prop_map(|n| serde_json::json!(n)).boxed()
        };
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            numeric,
            any::<String>().prop_map(serde_json::Value::String),
            // 协议关键字键名（对象字段撞名时提高信封相似度）
            prop_oneof![
                Just("id".to_string()),
                Just("result".to_string()),
                Just("event".to_string()),
                Just("data".to_string()),
                Just("success".to_string()),
                Just("error".to_string()),
            ]
            .prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(
            4,  // 嵌套深度
            64, // 期望规模
            8,  // 分支大小
            |inner| {
                prop_oneof![
                    // 对象（键可重复，Map 收敛为 last-wins，属合法 JSON 语义）
                    proptest::collection::vec((any::<String>(), inner.clone()), 0..8).prop_map(
                        |entries| serde_json::Value::Object(entries.into_iter().collect())
                    ),
                    // 数组
                    proptest::collection::vec(inner, 0..8).prop_map(serde_json::Value::Array),
                ]
            },
        )
        .boxed()
    }

    /// 递归 JSON 值策略（含任意有限浮点）：用于不要求数值位保真的
    /// never-panic / 分派类属性
    fn arb_json_value() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        arb_json_value_impl(true)
    }

    /// roundtrip 安全变体（不含任意浮点）。
    ///
    /// 属性测试实测：serde_json 对部分 f64 的序列化→解析往返存在 1 ulp 保真
    /// 损失（其解析器非全区间正确舍入，如 -1.0443002892504437e-284 往返后
    /// 末位差 1）。数值位保真不属 IPC 解析器契约（数据透传以 serde_json 为界），
    /// 故逐字段相等类属性用整数域版本钉住其余结构的无损性；浮点路径的
    /// never-panic 覆盖由 [`arb_json_value`] 承担。
    fn arb_json_value_exact() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        arb_json_value_impl(false)
    }

    /// 任意 IPC 行策略（parse_and_send_line 的输入域）
    fn arb_ipc_line() -> impl proptest::strategy::Strategy<Value = String> {
        prop_oneof![
            // 完全乱码 / 非 JSON 文本
            3 => any::<String>(),
            // 合法 JSON（含数组 / 字符串 / 数字等非对象信封形状）
            3 => arb_json_value().prop_map(|v| v.to_string()),
            // 形似响应信封：{"id": u64, "result": {...}} + 杂散外字段（可与 event 并存）
            3 => (any::<u64>(), arb_json_value(), any::<Option<String>>(), any::<bool>())
                .prop_map(|(id, data, err, with_event)| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("id".into(), serde_json::json!(id));
                    obj.insert(
                        "result".into(),
                        serde_json::json!({ "success": true, "data": data, "error": err }),
                    );
                    if with_event {
                        obj.insert("event".into(), serde_json::json!("screenshot"));
                    }
                    serde_json::Value::Object(obj).to_string()
                }),
            // 形似事件信封
            2 => (any::<String>(), arb_json_value()).prop_map(|(event, data)| {
                serde_json::json!({ "event": event, "data": data }).to_string()
            }),
            // 空行 / 纯空白 / CRLF 残留（读侧已 trim \r\n 后可能残留的形态）
            1 => prop_oneof![
                Just(String::new()),
                Just("   ".to_string()),
                Just("\t\r\n".to_string()),
                Just(" ".to_string()),
            ],
        ]
    }

    /// 同步驱动单行解析并收走 channel 中的全部消息。
    ///
    /// 解析 future 内部仅含容量充足的 mpsc send（立即就绪），单次 poll 必完成，
    /// 无需 tokio runtime——属性测试保持纯同步，prop_assert 的 early-return
    /// 语义才能直接作用于测试函数本体。
    fn parse_line_collect(line: &str) -> Vec<ParsedMessage> {
        let (tx, mut rx) = mpsc::channel(8);
        let fut = parse_and_send_line(line, &tx);
        tokio::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(fut.as_mut(), &mut cx) {
            std::task::Poll::Ready(()) => {}
            std::task::Poll::Pending => panic!("单行解析 future 不应挂起"),
        }
        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    /// 同步驱动超限行结算并收走 channel 中的全部消息（同上：单次 poll 必完成）
    fn settle_line_collect(prefix: &[u8]) -> Vec<ParsedMessage> {
        let (tx, mut rx) = mpsc::channel(8);
        let fut = settle_oversized_line(prefix, &tx);
        tokio::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(fut.as_mut(), &mut cx) {
            std::task::Poll::Ready(()) => {}
            std::task::Poll::Pending => panic!("超限结算 future 不应挂起"),
        }
        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【不变量 P1】任意行输入下解析器永不 panic，且单行至多产出一条消息，
        /// 消息只能是 Response / ResponseError / Event 之一（绝不 WorkerExited /
        /// InvalidLine 等越界变体）；产出的信封必然可再序列化（结构有界合法）
        #[test]
        fn prop_parse_line_never_panics_and_yields_bounded_envelope(line in arb_ipc_line()) {
            let msgs = parse_line_collect(&line);
            prop_assert!(msgs.len() <= 1, "单行至多产出一条消息");
            if let Some(msg) = msgs.into_iter().next() {
                match &msg {
                    ParsedMessage::Response(resp) => {
                        prop_assert!(serde_json::to_string(resp).is_ok());
                    }
                    ParsedMessage::ResponseError { error, .. } => {
                        prop_assert!(!error.is_empty());
                    }
                    ParsedMessage::Event(ev) => {
                        prop_assert!(serde_json::to_string(ev).is_ok());
                    }
                    // WorkerExited / InvalidLine 只能由读循环与健康监控产生，
                    // 单行解析路径绝不发出
                    other => {
                        prop_assert!(
                            !matches!(other, ParsedMessage::WorkerExited(_) | ParsedMessage::InvalidLine(_)),
                            "行解析不得产出越界变体: {other:?}"
                        );
                    }
                }
            }
        }

        /// 【不变量 P2】任意合法 IpcResponse 序列化为 NDJSON 行后必须无损还原：
        /// 响应信封解析对协议自身产出的所有信封是完备的（id / success / data /
        /// error 逐字段一致），这是在途请求结算语义的根基
        /// （data 用整数域策略：serde_json 浮点往返保真限制见 arb_json_value_exact）
        #[test]
        fn prop_response_envelope_roundtrip(
            id in any::<u64>(),
            success in any::<bool>(),
            data in arb_json_value_exact(),
            error in any::<Option<String>>(),
        ) {
            let resp = IpcResponse {
                id,
                result: IpcResult { success, data, error },
            };
            let line = serde_json::to_string(&resp).unwrap();
            let msgs = parse_line_collect(&line);
            let [ParsedMessage::Response(parsed)] = msgs.as_slice() else {
                panic!("合法响应信封必须还原为 Response: {msgs:?}");
            };
            prop_assert_eq!(parsed.id, resp.id);
            prop_assert_eq!(parsed.result.success, resp.result.success);
            prop_assert_eq!(&parsed.result.data, &resp.result.data);
            prop_assert_eq!(&parsed.result.error, &resp.result.error);
        }

        /// 【不变量 P3】事件信封同样无损还原（event 名与 data 逐字段一致）；
        /// data 缺省与任意负载（含对象内协议关键字撞名）均不得改变分派
        /// （data 用整数域策略：serde_json 浮点往返保真限制见 arb_json_value_exact）
        #[test]
        fn prop_event_envelope_roundtrip(event in any::<String>(), data in arb_json_value_exact()) {
            let ev = IpcEvent { event, data };
            let line = serde_json::to_string(&ev).unwrap();
            let msgs = parse_line_collect(&line);
            let [ParsedMessage::Event(parsed)] = msgs.as_slice() else {
                panic!("合法事件信封必须还原为 Event: {msgs:?}");
            };
            prop_assert_eq!(&parsed.event, &ev.event);
            prop_assert_eq!(&parsed.data, &ev.data);
        }

        /// 【不变量 P4】顶层 id 为 u64 的任意 JSON 行必以同一 id 结算在途请求：
        /// 要么还原为合法 Response，要么产出 ResponseError{同 id}——绝不误派发
        /// 为 Event、也绝不静默丢弃（历史遗留 F1 的防回归口径）；信封分派中
        /// id 优先于 event（两者并存时按响应处理）
        #[test]
        fn prop_id_bearing_line_always_settles_with_same_id(id in any::<u64>(), rest in arb_json_value()) {
            // rest 以对象形式铺进信封顶层（可含畸形 result / event / 杂散字段），
            // id 最后写入，保证顶层 id 恒为 u64
            let mut obj = serde_json::Map::new();
            if let serde_json::Value::Object(map) = rest {
                obj.extend(map);
            } else {
                obj.insert("payload".into(), rest);
            }
            obj.insert("id".into(), serde_json::json!(id));
            let line = serde_json::Value::Object(obj).to_string();
            // 期望口径直接以 serde 为准：能否反序列化为 IpcResponse 决定分支
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            let expect_response = serde_json::from_value::<IpcResponse>(v).is_ok();
            let msgs = parse_line_collect(&line);
            prop_assert!(msgs.len() == 1, "带 u64 id 的行必须恰好结算一条: {msgs:?}");
            match &msgs[0] {
                ParsedMessage::Response(resp) => {
                    prop_assert!(expect_response, "反序列化失败的信封不得伪装为 Response");
                    prop_assert_eq!(resp.id, id);
                }
                ParsedMessage::ResponseError { id: got, .. } => {
                    prop_assert!(!expect_response, "合法响应信封不得降级为 ResponseError");
                    prop_assert_eq!(*got, id, "ResponseError 必须携带原始 id");
                }
                other => panic!("带 u64 id 的行不得派发为 {other:?}"),
            }
        }

        /// 【不变量 P5】顶层 id 非 u64（字符串 / 浮点 / 负数 / 对象等）的响应形行
        /// 不得派发任何消息——无有效 id 无法定位在途请求，宁可留痕也不误结算
        #[test]
        fn prop_non_u64_id_line_yields_no_message(id in arb_non_u64_json()) {
            let line = serde_json::json!({
                "id": id,
                "result": { "success": true, "data": null, "error": null },
            })
            .to_string();
            let msgs = parse_line_collect(&line);
            prop_assert!(msgs.is_empty(), "非 u64 id 的行不得派发消息: {msgs:?}");
        }

        /// 【不变量 P6】既无 id 也无 event 键的 JSON 行不派发消息（未知格式仅留痕）
        #[test]
        fn prop_id_and_event_less_line_yields_no_message(v in arb_json_value()) {
            // 剥掉顶层 id / event 键后喂入（非对象形状原样保留）
            let line = match v {
                serde_json::Value::Object(map) => serde_json::Value::Object(
                    map.into_iter()
                        .filter(|(k, _)| k != "id" && k != "event")
                        .collect(),
                ),
                other => other,
            }
            .to_string();
            let msgs = parse_line_collect(&line);
            prop_assert!(msgs.is_empty(), "无 id/event 的行不得派发消息: {msgs:?}");
        }

        /// 【不变量 P7】extract_leading_id 对规范形态（任意 ASCII 空白穿插 +
        /// 任意非数字起始后缀）精确接受：Some(id) 当且仅当数字段等于 id（G18）
        #[test]
        fn prop_extract_leading_id_accepts_canonical_prefix(
            id in any::<u64>(),
            ws in proptest::collection::vec(arb_ascii_ws(), 0..4),
            suffix in arb_non_digit_starting_string(),
        ) {
            // ws[0] 行首 / ws[1] { 与 "id" 之间 / ws[2] "id" 与 : 之间 /
            // ws[3] : 与数字之间；后缀必须非数字起始（数字扫描停在 id 之后）
            let w = |i: usize| ws.get(i).copied().unwrap_or("");
            let head = format!(
                "{}{{{}\"id\"{}:{}{}{}",
                w(0), w(1), w(2), w(3), id, suffix
            );
            prop_assert_eq!(extract_leading_id(&head), Some(id), "head: {:?}", head);
        }

        /// 【不变量 P8】非规范前缀形态一律拒绝（近失语料逐形钉住）：
        /// 缺冒号 / 字符串 id / 缺数字 / 键名大小写 / id 非行首 / 缺括号 /
        /// 行首杂散字符 / 数字溢出 u64 / 空串
        #[test]
        fn prop_extract_leading_id_rejects_near_misses(id in any::<u64>(), mutation in 0usize..9) {
            let heads = [
                format!("{{\"id\" {id}}}"),
                format!("{{\"id\":\"{id}\"}}"),
                "{\"id\":}".to_string(),
                format!("{{ \"ID\": {id} }}"),
                format!("{{\"result\":{{\"id\":{id}}}}}"),
                format!("\"id\":{id}"),
                format!("x{{\"id\":{id}}}"),
                // 数字段溢出 u64：提取循环吃满数字但 parse 失败 → None
                format!("{{\"id\":{id}999999999999999999999999}}"),
                String::new(),
            ];
            prop_assert_eq!(
                extract_leading_id(&heads[mutation]),
                None,
                "head: {:?}",
                heads[mutation]
            );
        }

        /// 【不变量 P9】任意字符串下 extract_leading_id 永不 panic（多字节 UTF-8
        /// 边界、截断的 "id" 键、超长数字等边界全部落入 None 或合法 u64）
        #[test]
        fn prop_extract_leading_id_never_panics(s in any::<String>()) {
            // 结果域由 P7/P8 钉住，此处只钉全域无 panic
            let _ = extract_leading_id(&s);
        }

        /// 【不变量 P10】settle_oversized_line 对任意字节序列不 panic，且：
        /// 产消息 ⟺ 前 512 字节为合法 UTF-8 且前缀可提取 id；产出的必为
        /// success=false 的 Response，id 与提取结果一致（G18 结算口径）
        #[test]
        fn prop_settle_oversized_line_bounded_settlement(prefix in arb_oversized_prefix()) {
            let head_len = prefix.len().min(512);
            let expect = std::str::from_utf8(&prefix[..head_len])
                .ok()
                .and_then(extract_leading_id);
            let msgs = settle_line_collect(&prefix);
            match (expect, msgs.as_slice()) {
                (Some(id), [ParsedMessage::Response(resp)]) => {
                    prop_assert_eq!(resp.id, id);
                    prop_assert!(!resp.result.success, "超限结算必为失败信封");
                    let err = resp.result.error.clone().unwrap_or_default();
                    prop_assert!(err.contains("MiB"), "错误消息应说明单行上限: {err}");
                }
                (Some(id), other) => {
                    panic!("可提取 id={id} 的超长前缀必须结算: {other:?}");
                }
                (None, []) => {}
                (None, other) => panic!("无法提取 id 的前缀不得产消息: {other:?}"),
            }
        }

        /// 【不变量 P11】日志预览按 UTF-8 字符边界安全截断：任意字符串不 panic、
        /// 结果必为原行前缀且不超过 200 字节、短行原样返回
        #[test]
        fn prop_log_line_preview_is_char_boundary_safe_prefix(line in any::<String>()) {
            let preview = log_line_preview(&line);
            prop_assert!(line.starts_with(preview), "预览必为原行前缀");
            prop_assert!(preview.len() <= 200);
            if line.len() <= 200 {
                prop_assert_eq!(preview, line.as_str());
            }
        }

        /// 【不变量 P12】Worker stderr 行解析对任意输入不 panic，且级别词
        /// 要么为空（无格式续行）要么命中已知档位（大小写归一后）
        #[test]
        fn prop_parse_worker_stderr_line_level_domain(line in any::<String>()) {
            let (level, _message) = parse_worker_stderr_line(&line);
            prop_assert!(
                level.is_empty()
                    || [
                        "TRACE", "DEBUG", "INFO", "WARNING", "WARN", "ERROR", "CRITICAL", "FATAL",
                    ]
                    .contains(&level.as_str()),
                "未知级别词 {level}"
            );
        }

        /// 【不变量 P13】stderr 转发对任意字节（含非 UTF-8 与空行）不 panic，
        /// 且非 UTF-8 行与空白行不改继承状态（traceback 块语义的边界）
        #[test]
        fn prop_log_stderr_line_preserves_inheritance_on_undecodable(
            bytes in proptest::collection::vec(any::<u8>(), 0..=256),
            prev in any::<bool>(),
        ) {
            let got = log_stderr_line(&bytes, prev);
            let inherits_neutral = std::str::from_utf8(&bytes)
                .map(|s| s.trim_end().is_empty())
                .unwrap_or(true);
            if inherits_neutral {
                prop_assert_eq!(got, prev, "非 UTF-8 / 空白行不改继承状态");
            }
        }
    }

    /// 任意非负 ASCII 空白串（空格 / \t / \r / 换行 / 换页），至多 3 字符
    fn arb_ascii_ws() -> impl proptest::strategy::Strategy<Value = &'static str> {
        prop_oneof![
            Just(""),
            Just(" "),
            Just("\t"),
            Just("\r"),
            Just("\n"),
            Just("\u{0c}"),
            Just(" \t"),
            Just("\r\n "),
        ]
    }

    /// 不以 ASCII 数字起始的任意字符串（G18 后缀域：数字扫描必须停在 id 之后）
    fn arb_non_digit_starting_string() -> impl proptest::strategy::Strategy<Value = String> {
        any::<String>().prop_filter("后缀不得以数字起始", |s| {
            !s.bytes().next().is_some_and(|b| b.is_ascii_digit())
        })
    }

    /// 非 u64 可表示的 JSON 值策略（id 字段的畸形域：字符串 / 浮点 / 负数 /
    /// 对象 / 数组 / null / 布尔）
    fn arb_non_u64_json() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        arb_json_value().prop_filter("排除 as_u64 可命中的值", |v| v.as_u64().is_none())
    }

    /// 超限行前缀策略：任意字节序列 + 嵌入 `{"id":N` 规范前缀的变体
    /// （后者保证 id 提取成功路径有足够密度）
    fn arb_oversized_prefix() -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
        prop_oneof![
            1 => proptest::collection::vec(any::<u8>(), 0..=1200),
            2 => (any::<u64>(), proptest::collection::vec(any::<u8>(), 0..=1100))
                .prop_map(|(id, tail)| {
                    let mut b = format!("{{\"id\":{id},\"result\":{{").into_bytes();
                    b.extend(tail);
                    b
                }),
        ]
    }
}
