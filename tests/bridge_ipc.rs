//! Bridge 进程层（NDJSON IPC）集成测试
//!
//! 用本地 Python 运行一个仅依赖标准库的假 Worker，对 `spawn_worker` +
//! stdin/stdout reader/writer + health monitor 做真实子进程验证，重点覆盖：
//! - 正常请求/响应 roundtrip；
//! - 历史遗留 F1：带有效 id 但无法反序列化为 IpcResponse 的行 → 回传 `ResponseError`；
//! - 事件（无 id）转发；
//! - 子进程退出 → `WorkerExited(code)`。
//!
//! 该测试是 Bridge 后续演进（如 supervisor actor 化）的安全网。找不到 Python 时跳过。

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use campus_auth::bridge::{spawn_worker, IpcMessage, IpcRequest, ParsedMessage};
use serde_json::Value;
use tokio::sync::mpsc;

/// 仅依赖标准库的假 Worker：读取 NDJSON 命令，按 method 返回不同响应。
///
/// - `browser_health_check` → 正常响应 `{healthy:true}`；
/// - `emit_malformed` → 带 id 但缺 `result` 字段（触发 F1 的 ResponseError 分支）；
/// - `emit_event` → 无 id 的事件行；
/// - `crash` → 以退出码 3 退出；
/// - `shutdown` → 正常退出（码 0）；
/// - 其他 → 回显 method 的成功响应。
const FAKE_WORKER: &str = r#"
import sys, json
# 关闭换行翻译，保证按 \n 分隔（避免 Windows \r\n）
try:
    sys.stdout.reconfigure(newline="")
except Exception:
    pass

def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    # 取消通知（无 id）
    if "cancel" in msg and "id" not in msg:
        continue
    mid = msg.get("id")
    method = msg.get("method")
    if method == "shutdown":
        sys.exit(0)
    elif method == "crash":
        sys.exit(3)
    elif method == "browser_health_check":
        emit({"id": mid, "result": {"success": True, "data": {"healthy": True}, "error": None}})
    elif method == "emit_malformed":
        # 带 id 但缺 result 字段 → 反序列化为 IpcResponse 失败
        emit({"id": mid, "garbage": True})
    elif method == "emit_event":
        emit({"event": "step_progress", "data": {"step_index": 1}})
        emit({"id": mid, "result": {"success": True, "data": {}, "error": None}})
    else:
        emit({"id": mid, "result": {"success": True, "data": {"echo": method}, "error": None}})
"#;

/// 定位本地 Python 解释器（优先项目内 venv，其次 PATH）。
fn locate_python() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("python_worker/.venv/Scripts/python.exe"),
        manifest.join("environment/.venv/Scripts/python.exe"),
        manifest.join("python_worker/.venv/bin/python3"),
        manifest.join("environment/.venv/bin/python3"),
    ];
    for c in candidates {
        // 仅判断文件存在不够：uv 重建/删除后，venv 的 python.exe 可能仍是
        // 指向已不存在解释器的启动器，启动会以 101 退出，导致测试误报。
        if c.exists()
            && Command::new(&c)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            return Some(c);
        }
    }
    ["python3", "python"].into_iter().find_map(|name| {
        let path = which::which(name).ok()?;
        let output = Command::new(&path).arg("--version").output().ok()?;
        output.status.success().then_some(path)
    })
}

/// 带超时地接收一条解析消息。
async fn recv_timeout(rx: &mut mpsc::Receiver<ParsedMessage>, secs: u64) -> Option<ParsedMessage> {
    tokio::time::timeout(Duration::from_secs(secs), rx.recv())
        .await
        .ok()
        .flatten()
}

/// 发送一条请求。
async fn send_request(proc: &campus_auth::bridge::WorkerProcess, id: u64, method: &str) {
    proc.stdin_tx
        .send(IpcMessage::Request(IpcRequest {
            id,
            method: method.to_string(),
            params: Value::Null,
        }))
        .await
        .expect("stdin 发送失败");
}

#[tokio::test]
async fn 进程层_ipc_roundtrip_事件_f1_崩溃() {
    let Some(python) = locate_python() else {
        eprintln!("跳过 bridge_ipc：未找到本地 Python 解释器");
        return;
    };

    // 写入假 worker 脚本到临时 .py 文件（保持句柄存活至测试结束）
    let mut script = tempfile::Builder::new()
        .suffix(".py")
        .tempfile()
        .expect("创建临时脚本失败");
    script
        .write_all(FAKE_WORKER.as_bytes())
        .expect("写入脚本失败");
    let worker_path = script.path().to_path_buf();

    let (tx, mut rx) = mpsc::channel::<ParsedMessage>(64);
    // spawn_worker 需要 base_path 锚定浏览器数据目录，测试用临时目录即可
    let base_dir = tempfile::tempdir().expect("创建临时 base_path 失败");
    let proc = spawn_worker(&python, &worker_path, base_dir.path(), tx)
        .await
        .expect("spawn_worker 失败");

    // 1. 正常响应 roundtrip
    send_request(&proc, 2, "browser_health_check").await;
    match recv_timeout(&mut rx, 15).await {
        Some(ParsedMessage::Response(r)) => {
            assert_eq!(r.id, 2, "响应 id 应回显");
            assert!(r.result.success, "健康检查应成功");
            assert_eq!(
                r.result.data.get("healthy").and_then(Value::as_bool),
                Some(true)
            );
        }
        other => panic!("期望 Response，实际 {other:?}"),
    }

    // 2. F1：带 id 但无法解析为 IpcResponse → ResponseError（回收在途请求的关键路径）
    send_request(&proc, 5, "emit_malformed").await;
    match recv_timeout(&mut rx, 15).await {
        Some(ParsedMessage::ResponseError { id, error }) => {
            assert_eq!(id, 5, "ResponseError 应携带原始 id");
            assert!(!error.is_empty(), "应携带解析错误信息");
        }
        other => panic!("期望 ResponseError，实际 {other:?}"),
    }

    // 3. 事件（无 id）转发，随后是对应请求的成功响应
    send_request(&proc, 7, "emit_event").await;
    match recv_timeout(&mut rx, 15).await {
        Some(ParsedMessage::Event(ev)) => assert_eq!(ev.event, "step_progress"),
        other => panic!("期望 Event，实际 {other:?}"),
    }
    match recv_timeout(&mut rx, 15).await {
        Some(ParsedMessage::Response(r)) => assert_eq!(r.id, 7),
        other => panic!("期望 Response(7)，实际 {other:?}"),
    }

    // 4. 崩溃 → WorkerExited(3)
    send_request(&proc, 9, "crash").await;
    match recv_timeout(&mut rx, 15).await {
        Some(ParsedMessage::WorkerExited(code)) => {
            assert_eq!(code, 3, "退出码应透传");
        }
        other => panic!("期望 WorkerExited(3)，实际 {other:?}"),
    }

    // 清理后台 task（子进程已退出）
    proc.shutdown(Duration::from_secs(2)).await;
}
