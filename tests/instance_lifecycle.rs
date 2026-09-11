//! 实例生命周期集成测试（A-8）：锁互斥、端口回退、状态查询、优雅关闭
//!
//! 覆盖历史上多次出问题的链路：锁获取（F4/A4 时代缺陷）→ 容器初始化 →
//! 端口文件记录（G15 哨兵端口）→ `--stop` 经 Web API 的优雅退出。
//! 测试以真实二进制跑完整启动流程，注意用 `--no-tray --no-browser` 隔离桌面副作用。

mod common;

use assert_cmd::Command;
use common::{free_port, spawn_instance, wait_exit_or_kill, wait_listening};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[test]
fn instance_lock_status_and_graceful_stop() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();

    // 1. 首个实例正常启动并监听
    let mut first = spawn_instance(&base, port);
    let err_log = first.stderr_path().to_path_buf();
    assert!(
        wait_listening(port),
        "首个实例未在期限内开始监听端口 {port}；stderr: {}",
        std::fs::read_to_string(&err_log).unwrap_or_default()
    );

    // 2. 二次启动被实例锁拒绝
    Command::cargo_bin("campus-auth")
        .unwrap()
        .args(["--base-path", &base, "--no-tray", "--no-browser"])
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
    wait_exit_or_kill(&mut first.0, "首个实例", Some(&err_log));
}

/// 首选端口已被占用时，完整模式应使用系统分配端口，并将真实值同步给
/// `.runtime_port` / `.instance`，保证浏览器、--status 与 --stop 使用同一端口。
#[test]
fn occupied_port_falls_back_and_records_actual_port() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let occupied = TcpListener::bind(("127.0.0.1", 0)).expect("占用测试端口");
    let requested = occupied.local_addr().expect("读取测试端口").port();

    let mut instance = spawn_instance(&base, requested);
    let err_log = instance.stderr_path().to_path_buf();
    let deadline = Instant::now() + Duration::from_secs(15);
    let actual = loop {
        if let Some(info) = campus_auth::utils::lock::query_instance(dir.path()) {
            if info.running && info.port > 0 {
                break info.port;
            }
        }
        assert!(
            Instant::now() < deadline,
            "实例未在期限内记录实际端口；stderr: {}",
            std::fs::read_to_string(&err_log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    assert_ne!(actual, requested, "不得继续使用已占用端口");
    assert!(
        wait_listening(actual),
        "回退端口 {actual} 未开始监听；stderr: {}",
        std::fs::read_to_string(&err_log).unwrap_or_default()
    );
    let runtime_port = std::fs::read_to_string(dir.path().join("config").join(".runtime_port"))
        .expect("读取运行端口文件");
    assert_eq!(runtime_port.trim(), actual.to_string());

    Command::cargo_bin("campus-auth")
        .unwrap()
        .args(["--stop", "--base-path", &base])
        .timeout(Duration::from_secs(30))
        .assert()
        .success();
    wait_exit_or_kill(&mut instance.0, "端口回退实例", Some(&err_log));
}

/// Docker/LAN 非回环监听保持固定端口：冲突应在服务容器初始化前失败，且最终
/// 错误必须在 WorkerGuard 释放前写入文件日志，不能只出现在控制台。
#[test]
fn fixed_non_loopback_port_failure_is_flushed_to_log() {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let occupied = TcpListener::bind(("0.0.0.0", 0)).expect("占用非回环测试端口");
    let requested = occupied.local_addr().expect("读取测试端口").port();

    Command::cargo_bin("campus-auth")
        .unwrap()
        .args([
            "--base-path",
            &base,
            "--host",
            "0.0.0.0",
            "--port",
            &requested.to_string(),
            "--no-tray",
            "--no-browser",
            "--mode",
            "full",
        ])
        .timeout(Duration::from_secs(15))
        .assert()
        .failure();

    let log_text = std::fs::read_dir(dir.path().join("logs"))
        .expect("读取日志目录")
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        log_text.contains("启动失败"),
        "最终错误未写入日志: {log_text}"
    );
    assert!(
        log_text.contains("Web 控制台端口准备失败"),
        "日志缺少端口失败上下文: {log_text}"
    );
    assert!(
        !log_text.contains("正在初始化服务"),
        "端口失败后不应再启动后台服务: {log_text}"
    );
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
