//! Bridge Supervisor 状态机集成测试
//!
//! 在工作区内 `target/` 下搭建临时 base_path，挂载真实 `.venv`（目录 junction/symlink）+
//! 一个仅依赖标准库的假 `worker_main.py`，驱动真实的 `BridgeSupervisor`，覆盖：
//! - `execute` 正常往返（含懒加载 spawn + browser_health_check）；
//! - 历史遗留 F1：Worker 回传带 id 但无法解析的响应 → 在途请求以错误回收（不再永久阻塞）；
//! - Worker 崩溃 → 在途请求收到 `WorkerCrashed`，状态机恢复；
//! - 历史遗留 F2：`cancel` 可靠触发 → `execute` 返回 `Cancelled`。
//!
//! 说明：临时目录建在工作区 `target/` 内（沙箱仅允许工作区内写入）；清理时**仅移除 junction
//! 链接本身**（`remove_dir`，不跟随），绝不删除真实 `.venv`。找不到本地 Python 或无法创建
//! 目录链接时跳过。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use campus_auth::bridge::{BridgeError, BridgeSupervisor};
use campus_auth::config::ConfigService;
use campus_auth::status::StatusManager;
use serde_json::{Value, json};

/// 假 worker_main.py：仅依赖标准库，按 method 返回不同响应。
const FAKE_WORKER_MAIN: &str = r#"
import sys, json, time
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
        emit({"id": mid, "garbage": True})
    elif method == "sleep":
        # 挂起指定秒数（默认 10s）：模拟不响应 cancel 的挂起命令
        secs = (msg.get("params") or {}).get("secs", 10)
        time.sleep(float(secs))
        emit({"id": mid, "result": {"success": True, "data": {}, "error": None}})
    else:
        emit({"id": mid, "result": {"success": True, "data": {"echo": method}, "error": None}})
"#;

/// 临时 worker 目录树的 RAII 守卫：Drop 时安全清理（先移除 junction 链接，再删目录树）。
struct WorkerTree {
    base: PathBuf,
}

impl Drop for WorkerTree {
    fn drop(&mut self) {
        // 关键安全点：先以 remove_dir 移除 .venv junction 链接本身（不跟随、不触碰 target），
        // 再删除整个临时 base。绝不能让 remove_dir_all 跟随 junction 删到真实 .venv。
        let venv_link = self.base.join("python_worker").join(".venv");
        let _ = std::fs::remove_dir(&venv_link);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// 定位本地 Python venv 目录（其下须含 `Scripts/python.exe` 或 `bin/python3`）。
fn locate_venv() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("python_worker/.venv"),
        manifest.join("environment/.venv"),
    ];
    candidates.into_iter().find(|v| {
        let python = if cfg!(windows) {
            v.join("Scripts/python.exe")
        } else {
            v.join("bin/python3")
        };
        python.exists()
            && std::process::Command::new(python)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
    })
}

/// 创建目录链接（Windows 用 junction，Unix 用 symlink），均无需管理员权限。
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        // 使用 PowerShell 创建 junction（与 orphan 清理一致，部分环境下 cmd 不可用）
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
                    link.display(),
                    target.display()
                ),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// 在工作区 `target/` 下搭建 `python_worker/{.venv(链接), worker_main.py}`。
///
/// 成功返回持有清理逻辑的 `WorkerTree`（其 `base` 即 supervisor 的 base_path）。
fn setup_worker_tree(venv: &Path) -> Option<WorkerTree> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("bridge_it_{}", uuid::Uuid::new_v4()));
    let worker_dir = base.join("python_worker");
    std::fs::create_dir_all(&worker_dir).ok()?;
    let tree = WorkerTree { base: base.clone() };
    let venv_link = worker_dir.join(".venv");
    if !link_dir(venv, &venv_link) {
        return None; // tree 在此 drop，清理已创建的目录
    }
    // 校验链接后 Python 可达，否则视为环境不支持而跳过
    if !venv_link.join("Scripts/python.exe").exists() && !venv_link.join("bin/python3").exists() {
        return None;
    }
    std::fs::write(worker_dir.join("worker_main.py"), FAKE_WORKER_MAIN).ok()?;
    Some(tree)
}

/// 构造一个已 spawn 的 BridgeSupervisor（附带停止句柄与需保活的 ConfigService）。
async fn make_supervisor(
    base: &Path,
) -> (
    Arc<BridgeSupervisor>,
    campus_auth::ServiceHandle,
    Arc<ConfigService>,
) {
    let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
    let config = ConfigService::new(base.to_path_buf(), reload_tx)
        .await
        .expect("ConfigService 构造失败");
    let status = Arc::new(StatusManager::new());
    let bridge = BridgeSupervisor::new(base.to_path_buf(), config.clone(), status, None);
    let handle = bridge.spawn();
    (bridge, handle, config)
}

#[tokio::test]
async fn supervisor_执行_f1恢复_崩溃恢复() {
    let Some(venv) = locate_venv() else {
        eprintln!("跳过 bridge_supervisor：未找到本地 Python venv");
        return;
    };
    let Some(tree) = setup_worker_tree(&venv) else {
        eprintln!("跳过 bridge_supervisor：无法创建 .venv 目录链接（可能缺少权限）");
        return;
    };

    let (bridge, handle, _config) = make_supervisor(&tree.base).await;
    let short = Duration::from_secs(40);

    // 1. 正常往返：懒加载 spawn + 健康检查 + 请求回显
    match bridge
        .execute_with_timeout("browser_task", Value::Null, short)
        .await
    {
        Ok(resp) => {
            assert!(resp.result.success, "browser_task 应成功");
            assert_eq!(
                resp.result.data.get("echo").and_then(Value::as_str),
                Some("browser_task")
            );
        }
        other => panic!("期望 Ok，实际 {other:?}"),
    }

    // 2. F1：带 id 但无法解析的响应 → 在途请求以 Internal 错误回收（不再永久阻塞至超时）
    let r = bridge
        .execute_with_timeout("emit_malformed", Value::Null, short)
        .await;
    assert!(
        matches!(r, Err(BridgeError::Internal(_))),
        "malformed 响应应回收为 Internal 错误，实际 {r:?}"
    );

    // 3. 崩溃恢复：Worker 退出 → 在途请求收到 WorkerCrashed
    let r = bridge
        .execute_with_timeout("crash", Value::Null, short)
        .await;
    assert!(
        matches!(r, Err(BridgeError::WorkerCrashed { .. })),
        "崩溃应回收为 WorkerCrashed，实际 {r:?}"
    );

    handle.stop().await;
}

#[tokio::test]
async fn supervisor_cancel_返回_cancelled() {
    let Some(venv) = locate_venv() else {
        eprintln!("跳过 bridge_supervisor cancel：未找到本地 Python venv");
        return;
    };
    let Some(tree) = setup_worker_tree(&venv) else {
        eprintln!("跳过 bridge_supervisor cancel：无法创建 .venv 目录链接");
        return;
    };

    let (bridge, handle, _config) = make_supervisor(&tree.base).await;

    // 先预热：确保 Worker 已 spawn 并通过健康检查，避免后续 sleep 请求的 token 注册
    // 被 spawn/健康检查耗时阻塞导致 cancel 先于 token 注册而落空。
    let _ = bridge
        .execute_with_timeout("browser_task", Value::Null, Duration::from_secs(40))
        .await;

    // 在独立 task 中发起一个耗时请求（Worker sleep 10s），随后触发 cancel。
    let bridge2 = bridge.clone();
    let jh = tokio::spawn(async move {
        bridge2
            .execute_with_timeout(
                "sleep",
                json!({ "cancel_id": "c1" }),
                Duration::from_secs(40),
            )
            .await
    });

    // Worker 已预热，token 注册与请求发送均在毫秒级完成；稍等后触发取消。
    tokio::time::sleep(Duration::from_secs(1)).await;
    bridge.cancel("c1");

    let res = tokio::time::timeout(Duration::from_secs(8), jh)
        .await
        .expect("cancel 后 execute 应尽快返回")
        .expect("execute task 不应 panic");
    assert!(
        matches!(res, Err(BridgeError::Cancelled)),
        "cancel 后应返回 Cancelled，实际 {res:?}"
    );

    handle.stop().await;
}

/// 5.2：调用方超时后应释放会话槽位，后续调试类请求不再 WorkerBusy。
///
/// `sleep` 请求在 Worker 侧挂起 10s，用 1s 超时触发其超时。修复前超时后槽位滞留
/// （current_session 仍为 Login），后续 debug_step 会因会话不兼容而 WorkerBusy；
/// 修复后超时发送 Cancel 触发本地 token → 守卫 drop → 槽位复位，debug_step 应成功。
#[tokio::test]
async fn supervisor_超时_释放会话槽位() {
    let Some(venv) = locate_venv() else {
        eprintln!("跳过 bridge_supervisor timeout：未找到本地 Python venv");
        return;
    };
    let Some(tree) = setup_worker_tree(&venv) else {
        eprintln!("跳过 bridge_supervisor timeout：无法创建 .venv 目录链接");
        return;
    };

    let (bridge, handle, _config) = make_supervisor(&tree.base).await;

    // 预热：确保 Worker 已 spawn 并通过健康检查
    let _ = bridge
        .execute_with_timeout("browser_task", Value::Null, Duration::from_secs(40))
        .await;

    // 发起挂起请求（Worker sleep 10s），用 1s 超时触发超时清理
    let r = bridge
        .execute_with_timeout("sleep", json!({}), Duration::from_secs(1))
        .await;
    assert!(
        matches!(r, Err(BridgeError::Timeout)),
        "挂起请求应超时，实际 {r:?}"
    );

    // 等待超时后的 Cancel → 本地 token 唤醒 → guard drop → 槽位释放
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 调试类请求应通过（槽位已释放；若滞留则会 WorkerBusy）
    let r2 = bridge
        .execute_with_timeout("debug_step", Value::Null, Duration::from_secs(40))
        .await;
    assert!(
        r2.is_ok(),
        "超时后会话槽位应释放，debug_step 应成功，实际 {r2:?}"
    );

    handle.stop().await;
}

/// F2 回归：请求 A 超时后若槽位已被新请求 B 接管（FIFO 语义），超时宽限循环
/// 不得把 B 误判为 A 卡死而强杀——B 应正常完成。
///
/// 时序：A（sleep 12s，1s 超时）占位 → 0.5s 后 B（echo 快命令）经 FIFO 接管
/// 槽位（Python 侧串行队列使其排队在 A 的 sleep 之后）→ A 超时时刻
/// current_cancel_id 已属 B，归属校验判定本请求与槽位滞留无关，直接返回
/// Timeout；B 在 A 的 sleep 结束后正常回包。旧实现只看 current_request_id
/// .is_some()，会在宽限期结束时（槽位仍被 B 占用）强杀 Worker，B 被 drain
/// 为 WorkerCrashed。
#[tokio::test]
async fn supervisor_超时宽限_不误杀接管槽位的新会话() {
    let Some(venv) = locate_venv() else {
        eprintln!("跳过 bridge_supervisor F2：未找到本地 Python venv");
        return;
    };
    let Some(tree) = setup_worker_tree(&venv) else {
        eprintln!("跳过 bridge_supervisor F2：无法创建 .venv 目录链接");
        return;
    };

    let (bridge, handle, _config) = make_supervisor(&tree.base).await;

    // 预热：确保 Worker 已 spawn 并通过健康检查
    let _ = bridge
        .execute_with_timeout("browser_task", Value::Null, Duration::from_secs(40))
        .await;

    // A：挂起 12s（不响应 cancel），1s 超时触发超时路径
    let bridge_a = bridge.clone();
    let task_a = tokio::spawn(async move {
        let start = std::time::Instant::now();
        let r = bridge_a
            .execute_with_timeout("sleep", json!({ "secs": 12 }), Duration::from_secs(1))
            .await;
        (r, start.elapsed())
    });

    // 等 A 占位后发送 B：FIFO 接管槽位（Login + browser_task 兼容），
    // Python 串行队列让 B 排在 A 的 sleep 之后
    tokio::time::sleep(Duration::from_millis(500)).await;
    let task_b = {
        let bridge_b = bridge.clone();
        tokio::spawn(async move {
            bridge_b
                .execute_with_timeout("browser_task", Value::Null, Duration::from_secs(40))
                .await
        })
    };

    // A 应快速返回 Timeout（归属不匹配跳过宽限等待，无需耗满 10s）
    let (r_a, elapsed_a) = join_with_timeout(task_a, 8).await;
    assert!(
        matches!(r_a, Err(BridgeError::Timeout)),
        "A 应返回 Timeout，实际 {r_a:?}"
    );
    assert!(
        elapsed_a < Duration::from_secs(5),
        "A 的超时返回应跳过宽限等待（<5s），实际 {elapsed_a:?}"
    );

    // B 不被误杀：A 的 sleep 结束（~12s）后正常回包
    let r_b = join_with_timeout(task_b, 35).await;
    match r_b {
        Ok(resp) => {
            assert!(resp.result.success, "B 不应被误杀，实际 {resp:?}");
            assert_eq!(
                resp.result.data.get("echo").and_then(Value::as_str),
                Some("browser_task")
            );
        }
        other => panic!("期望 B 成功，实际 {other:?}（误杀会表现为 WorkerCrashed）"),
    }

    handle.stop().await;
}

/// 带超时地等待一个返回 (T, Duration) 的 JoinHandle（测试辅助）
async fn join_with_timeout<T>(handle: tokio::task::JoinHandle<T>, secs: u64) -> T {
    tokio::time::timeout(Duration::from_secs(secs), handle)
        .await
        .expect("任务应在超时窗口内完成")
        .expect("任务不应 panic")
}
