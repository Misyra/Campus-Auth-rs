//! 实例生命周期集成测试（A-8）：锁互斥、状态查询、优雅关闭
//!
//! 覆盖历史上多次出问题的链路：锁获取（F4/A4 时代缺陷）→ 容器初始化 →
//! 端口文件记录（G15 哨兵端口）→ `--stop` 经 Web API 的优雅退出。
//! 测试以真实二进制跑完整启动流程，注意用 `--no-tray --no-browser` 隔离桌面副作用。

mod common;

use assert_cmd::Command;
use common::{free_port, spawn_instance, wait_exit_or_kill, wait_listening};
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
