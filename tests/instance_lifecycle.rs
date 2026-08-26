//! 实例生命周期集成测试（A-8）：锁互斥、状态查询、优雅关闭
//!
//! 覆盖历史上多次出问题的链路：锁获取（F4/A4 时代缺陷）→ 容器初始化 →
//! 端口文件记录（G15 哨兵端口）→ `--stop` 经 Web API 的优雅退出。
//! 测试以真实二进制跑完整启动流程，注意用 `--no-tray --no-browser` 隔离桌面副作用。

use assert_cmd::Command;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

/// 子进程守卫：无论测试从哪条路径失败（panic/断言），Drop 都会强杀实例，
/// 防止残留进程占住测试管道或锁文件
struct InstanceGuard(Child);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 取一个当前空闲的高位端口（存在 TOCTOU 窗口，但测试环境可接受）
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("绑定临时端口失败")
        .local_addr()
        .unwrap()
        .port()
}

/// 启动一个完整模式实例（托盘与浏览器均禁用）
fn err_log_path(port: u16) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("campus-auth-itl-{port}.log"))
}

fn spawn_instance(base: &str, port: u16, err_log: &std::path::Path) -> InstanceGuard {
    let err_file = std::fs::File::create(err_log).expect("创建 stderr 日志失败");
    let child = StdCommand::new(env!("CARGO_BIN_EXE_campus-auth"))
        .args([
            "--base-path",
            base,
            "--port",
            &port.to_string(),
            "--no-tray",
            "--no-browser",
            "--mode",
            "full",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file))
        .spawn()
        .expect("启动实例进程失败");
    InstanceGuard(child)
}

/// 等待 Axum 端口开始接受 TCP 连接（最多 15s）
fn wait_listening(port: u16) -> bool {
    for _ in 0..60 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// 在限期内等待子进程退出；超时则强杀并失败
fn wait_exit_or_kill(child: &mut Child, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    panic!("{label} 未在期限内退出");
}

#[test]
fn instance_lock_status_and_graceful_stop() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();

    // 1. 首个实例正常启动并监听
    let err_log = err_log_path(port);
    let mut first = spawn_instance(&base, port, &err_log);
    assert!(
        wait_listening(port),
        "首个实例未在期限内开始监听端口 {port}；stderr: {}",
        std::fs::read_to_string(&err_log).unwrap_or_default()
    );

    // 2. 二次启动被实例锁拒绝
    Command::cargo_bin("campus-auth")
        .unwrap()
        .args([
            "--base-path",
            &base,
            "--no-tray",
            "--no-browser",
        ])
        .timeout(Duration::from_secs(40))
        .assert()
        .failure()
        .stderr(predicates::str::contains("实例锁"));

    // 3. --status 报告运行中且端口为实际监听值
    let status = Command::cargo_bin("campus-auth")
        .unwrap()
        .args(["--status", "--base-path", &base])
        .timeout(Duration::from_secs(10))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_text = String::from_utf8_lossy(&status).to_string();
    assert!(status_text.contains("实例运行中"), "输出: {status_text}");
    assert!(
        status_text.contains(&port.to_string()),
        "状态应含真实端口 {port}，输出: {status_text}"
    );

    // 4. --stop 优雅关闭（经 /api/system/shutdown + 进程退出轮询）。
    // 冷启动调试构建下首次可能逼近轮询上限，重试一次：若进程已在两次调用
    // 之间退出，stop_instance 会清理残留并同样返回成功。
    let mut last = None;
    for _ in 0..3 {
        let output = Command::cargo_bin("campus-auth")
            .unwrap()
            .args(["--stop", "--base-path", &base])
            .timeout(Duration::from_secs(30))
            .output()
            .expect("--stop 执行失败");
        let text = format!(
            "code={} stdout={} stderr={}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if text.contains("实例已停止") {
            last = Some(text);
            break;
        }
        last = Some(text);
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        last.as_deref().unwrap_or("").contains("实例已停止"),
        "--stop 应报告成功: {:?}；实例 stderr: {}",
        last,
        std::fs::read_to_string(&err_log).unwrap_or_default()
    );
    wait_exit_or_kill(&mut first.0, "首个实例");
}

/// `--stop` 对不存在的实例应快速失败并给出明确错误（而非空等超时）
#[test]
fn stop_without_instance_fails_fast() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let started = Instant::now();
    Command::cargo_bin("campus-auth")
        .unwrap()
        .args(["--stop", "--base-path", &base])
        .timeout(Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(predicates::str::contains("未找到运行中的实例信息"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "无实例时 --stop 应快速失败"
    );
}
