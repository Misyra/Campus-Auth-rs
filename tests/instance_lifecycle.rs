//! 实例生命周期集成测试（A-8）：锁互斥、端口回退、状态查询、优雅关闭、
//! lightweight / login-once 启动模式（P2-9）
//!
//! 覆盖历史上多次出问题的链路：锁获取（F4/A4 时代缺陷）→ 容器初始化 →
//! 端口文件记录（G15 哨兵端口）→ `--stop` 经 Web API 的优雅退出。
//! 测试以真实二进制跑完整启动流程，注意用 `--no-tray --no-browser` 隔离桌面副作用。
//!
//! 真起二进制的用例（含新增两例）共享 `SERIAL` 互斥：实例启动是重 I/O + CPU
//! 操作，并发互相拖慢会放大时序敏感断言的抖动（口径同 `login_chain.rs`）。

mod common;

use assert_cmd::Command;
use common::{
    InstanceGuard, free_port, spawn_instance, spawn_instance_mode, wait_exit_or_kill,
    wait_listening, wait_until,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 用例级串行互斥：新增的 lightweight / login-once 用例各自独占一个真实二进制
/// 实例，串行执行避免资源竞争放大启动耗时抖动
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    // 2. 二次启动被实例锁拒绝（显式 --mode full：断言不隐式依赖默认模式）
    Command::cargo_bin("campus-auth")
        .unwrap()
        .args([
            "--base-path",
            &base,
            "--mode",
            "full",
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
        if let Some(info) = campus_auth::utils::lock::query_instance(dir.path())
            && info.running
            && info.port > 0
        {
            break info.port;
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

// ============================================================
// 启动模式（P2-9）：lightweight / login-once
// ============================================================

/// 进程内 mock 直连门户：与 `http_login_chain.rs` 的 DirectPortal 同构（该文件
/// 是独立测试 crate，无法跨目标复用），只保留直连渠道需要的两个端点——
/// - `POST /login`：凭据正确则置位 authenticated 并回「登录成功」，否则回错误文案；
/// - `GET /generate_204`：认证后回 204（登录后网络验证的在线判据），否则 200 文本。
///
/// 不复用 `tests/mock-servers/` 的 Python 门户：那些门户面向浏览器链路（验证码/
/// 页面选择器），直连渠道用不到，且本用例的 PATH 为空——进程内 mock 让用例
/// 完全不依赖外部 Python。
struct LoginPortal {
    addr: std::net::SocketAddr,
    authenticated: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LoginPortal {
    fn spawn() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定 mock 门户端口失败");
        listener.set_nonblocking(true).expect("设置非阻塞失败");
        let addr = listener.local_addr().expect("读取 mock 门户地址失败");
        let authenticated = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let auth_for_thread = authenticated.clone();
        let stop_for_thread = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => handle_portal_request(stream, &auth_for_thread),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            authenticated,
            stop,
            thread: Some(thread),
        }
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for LoginPortal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // 唤醒阻塞在 accept 上的服务线程再 join，避免 Drop 卡死
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_portal_request(mut stream: TcpStream, authenticated: &AtomicBool) {
    // Windows 继承监听器的非阻塞属性，显式复位才能完整读取请求
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let Ok(n) = stream.read(&mut buf) else {
            return;
        };
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&request);
    let first = text.lines().next().unwrap_or_default();
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");

    if path == "/generate_204" {
        if authenticated.load(Ordering::Acquire) {
            write_response(&mut stream, 204, "");
        } else {
            write_response(&mut stream, 200, "captive");
        }
        return;
    }
    if path == "/login" {
        if body.contains("username=testuser") && body.contains("password=testpass") {
            authenticated.store(true, Ordering::Release);
            write_response(&mut stream, 200, "登录成功");
        } else {
            write_response(&mut stream, 200, "账号或密码错误");
        }
        return;
    }
    write_response(&mut stream, 404, "not found");
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        _ => "Not Found",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// 预置直连渠道的最小配置集（login-once 无 Web API，只能文件预置）：
/// 直连任务 `tasks/http/portal-http.json`（v1 扁平字段）、方案
/// `config/profiles/default.json`（明文密码向后兼容）、`config/settings.json`
/// 只覆写监测目标——登录后网络验证探到 mock 门户的 204，保证判据确定性
/// （不依赖机器真实外网）。
fn seed_direct_channel(base: &Path, portal_base: &str) {
    let http_dir = base.join("tasks").join("http");
    std::fs::create_dir_all(&http_dir).expect("创建 tasks/http 目录失败");
    std::fs::write(
        http_dir.join("portal-http.json"),
        serde_json::json!({
            "type": "http",
            "task_id": "portal-http",
            "name": "直连门户",
            "method": "POST",
            "url": format!("{portal_base}/login"),
            "body": "username={username}&password={password}",
            "success_pattern": "登录成功",
            "failure_pattern": "账号或密码错误"
        })
        .to_string(),
    )
    .expect("写入直连任务失败");

    let profiles_dir = base.join("config").join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("创建 profiles 目录失败");
    std::fs::write(
        profiles_dir.join("default.json"),
        serde_json::json!({
            "id": "default",
            "name": "默认网络",
            "username": "testuser",
            "password": "testpass",
            "login_channel": "http",
            "active_http_task": "portal-http"
        })
        .to_string(),
    )
    .expect("写入方案失败");

    std::fs::write(
        base.join("config").join("settings.json"),
        serde_json::json!({
            "config_version": 10,
            "active_profile_id": "default",
            "global": {
                "monitor": {
                    "tcp_targets": [],
                    "http_targets": [format!("{portal_base}/generate_204")],
                    "url_targets": [],
                    "tcp_enabled": false,
                    "http_enabled": true,
                    "url_enabled": false,
                    "post_login_delay": 0
                }
            }
        })
        .to_string(),
    )
    .expect("写入 settings.json 失败");
}

/// 读取 base 下日志文件全集（合并为一段文本，供断言；仅真轻量分支使用）
#[cfg(not(target_os = "macos"))]
fn collect_logs(base: &Path) -> String {
    std::fs::read_dir(base.join("logs"))
        .expect("读取日志目录")
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 轻量模式启动形态（P2-9）。
///
/// 平台差异是产品文档承诺的既定行为，按 cfg 分派断言两侧：
/// - macOS：托盘因用户决策禁用（tray-icon 要求主线程 NSApplication 事件循环），
///   轻量模式在该平台自动降级为完整模式（`load_and_merge_config` 统一保证
///   「既无托盘也无 Web 入口」的组合不出现）——观测面即完整模式：Web 监听
///   真实端口、/api/health 可用、鉴权 token 落盘、引擎未被启动（startup_action
///   默认 None，快照 engine_state 应为 stopped）。
/// - 其余桌面平台（Windows/Linux）：轻量保持真语义——Axum 不常驻，`.instance`
///   记录哨兵端口 0（G15），`.runtime_port` 不落盘，Web 控制台不监听，
///   日志留痕「轻量模式运行中」且未触发任何启动动作，进程驻留等待托盘。
///
/// 退出：macOS 降级后端口真实，走 `--stop` 优雅退出；真轻量模式下 `--stop`
/// 设计上拒绝远程停止（Web 未启动，`stop_instance` 对哨兵端口 0 直接报错），
/// 优雅退出仅有托盘路径而测试禁用托盘——退出断言回归 [`InstanceGuard`] 的
/// 既有强杀机制（Drop kill + wait）。
#[tokio::test]
async fn lightweight_mode_surface_matches_platform_promise() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();
    // `mut` 仅 macOS 分支需要（wait_exit_or_kill 传 &mut）；分开声明避免
    // 非 macOS 编译时的 unused_mut 告警
    #[cfg(target_os = "macos")]
    let mut instance = spawn_instance_mode(&base, port, "lightweight");
    #[cfg(not(target_os = "macos"))]
    let instance = spawn_instance_mode(&base, port, "lightweight");
    let err_log = instance.stderr_path().to_path_buf();

    #[cfg(target_os = "macos")]
    {
        // 降级为完整模式：Web 监听。并行用例存在 free_port 的 TOCTOU 端口竞争，
        // 实例可能回退到内核分配端口——以 `.instance` 记录的真实端口为准
        // （record_port 落盘的是 Axum 实际绑定端口，见 launch_full）
        let actual_port;
        let port_deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(info) = campus_auth::utils::lock::query_instance(dir.path())
                && info.running
                && info.port > 0
            {
                actual_port = info.port;
                break;
            }
            assert!(
                Instant::now() < port_deadline,
                "实例未在期限内记录实际端口；stderr: {}",
                std::fs::read_to_string(&err_log).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            wait_listening(actual_port),
            "lightweight 在 macOS 应降级为完整模式并监听端口 {actual_port}；stderr: {}",
            std::fs::read_to_string(&err_log).unwrap_or_default()
        );
        let api_base = format!("http://127.0.0.1:{actual_port}");

        // health 无需鉴权（auth 中间件豁免），token 文件齐备
        let health = api_get(&api_base, "/api/health", "").await;
        assert_eq!(
            health["data"]["status"], "ok",
            "/api/health 应报告存活: {health}"
        );
        let token = wait_auth_token(dir.path()).await;

        // 引擎观测面：startup_action 默认 None，快照必须停留在未运行
        let status = api_get(&api_base, "/api/monitor/status", &token).await;
        assert_eq!(
            status["data"]["engine_state"], "stopped",
            "启动动作默认 None，引擎不得自动启动: {status}"
        );
        assert_eq!(
            status["data"]["login_status"], "idle",
            "轻量模式不得自动发起登录: {status}"
        );

        // 优雅退出：端口真实，--stop 可用（重试口径对齐既有用例的冷启动窗口）
        stop_instance_gracefully(&base, &err_log, "轻量模式（macOS 降级）实例");
        wait_exit_or_kill(
            &mut instance.0,
            "轻量模式（macOS 降级）实例",
            Some(&err_log),
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        // 真轻量：进程驻留且 `.instance` 记录哨兵端口 0
        let recorded = wait_until(Duration::from_secs(15), || {
            campus_auth::utils::lock::query_instance(dir.path())
                .map(|info| info.running && info.port == 0)
                .unwrap_or(false)
        })
        .await;
        assert!(
            recorded,
            "轻量模式应记录哨兵端口 0 且进程驻留；stderr: {}",
            std::fs::read_to_string(&err_log).unwrap_or_default()
        );

        // Web 控制台确未监听（给启动流程留足窗口后再确认）
        let mut ever_listening = false;
        for _ in 0..15 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ever_listening = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(
            !ever_listening,
            "轻量模式 Axum 应按需启动而非常驻，端口 {port} 不应监听"
        );
        assert!(
            !dir.path().join("config").join(".runtime_port").exists(),
            "轻量模式不应写入 .runtime_port（该文件由完整模式 Axum 准备路径落盘）"
        );

        // 日志观测面：确以轻量路径运行，且引擎/登录未被自动触发
        let logs = collect_logs(dir.path());
        assert!(
            logs.contains("轻量模式运行中"),
            "日志应留痕轻量模式运行: {logs}"
        );
        assert!(
            !logs.contains("完整模式运行中"),
            "非 macOS 平台轻量模式不得降级为完整模式: {logs}"
        );
        assert!(
            !logs.contains("已启动检测") && !logs.contains("单次登录模式"),
            "启动动作默认 None，日志不应出现引擎/登录自动触发: {logs}"
        );

        // 进程驻留到观测结束（未自行退出/崩溃），退出走 InstanceGuard 强杀机制
        let info =
            campus_auth::utils::lock::query_instance(dir.path()).expect("轻量模式实例信息应存在");
        assert!(info.running, "轻量模式进程应保持驻留");
    }
}

/// 单次登录模式 + 直连渠道（P2-9）：登录成功后进程自行退出（login-once 语义）。
///
/// 直连渠道全程进程内完成（HTTP 请求 + 登录后网络验证），不拉起 Python Worker
/// 与浏览器——本机 venv 无 PIL/ddddocr，浏览器链会走 skip 分支，必须避开；
/// `PATH` 置空进一步钉死「直连登录不得依赖 PATH 中的 Python/uv」。
/// 观测面：mock 门户收到正确凭据、退出码 0（`launch_login_once` 成功路径）、
/// 登录历史落盘（source=login_once / channel=http / result=success）。
#[tokio::test]
async fn login_once_direct_channel_exits_after_success() {
    let _serial = SERIAL.lock().await;
    let portal = LoginPortal::spawn();
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    seed_direct_channel(dir.path(), &portal.base());
    let port = free_port();

    let log = tempfile::NamedTempFile::new().expect("创建 stderr 临时文件失败");
    let stderr = log.reopen().expect("复用 stderr 临时文件失败");
    let mut instance = InstanceGuard::with_stderr_log(
        StdCommand::new(env!("CARGO_BIN_EXE_campus-auth"))
            .args([
                "--base-path",
                &base,
                "--port",
                &port.to_string(),
                "--no-tray",
                "--no-browser",
                "--mode",
                "login-once",
            ])
            // 直连登录不得依赖 PATH 中的 Python/uv；空 PATH 会让误入环境引导立即暴露
            .env("PATH", "")
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("启动 login-once 实例失败"),
        log,
    );

    // login-once 语义：登录成功后进程自行退出（≤120s 覆盖冷启动 + 登录 + 优雅关闭）
    let deadline = Instant::now() + Duration::from_secs(120);
    let exit_status = loop {
        match instance.0.try_wait().expect("try_wait 失败") {
            Some(status) => break status,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "login-once 实例未在期限内自行退出；stderr: {}",
                    std::fs::read_to_string(instance.stderr_path()).unwrap_or_default()
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let stderr_text = std::fs::read_to_string(instance.stderr_path()).unwrap_or_default();
    assert_eq!(
        exit_status.code(),
        Some(0),
        "登录成功后应以 0 退出；stderr: {stderr_text}"
    );

    // mock 门户确认凭据已被真实消费
    assert!(
        portal.authenticated.load(Ordering::Acquire),
        "mock 门户未收到正确凭据，登录并未真正发生；stderr: {stderr_text}"
    );

    // 直连渠道不应创建 Python Worker
    assert!(
        !dir.path().join("python_worker").exists(),
        "直连登录不应创建或复制 Python Worker"
    );

    // 登录历史落盘观测：logs/login_history/<日期>.jsonl 恰好一条成功记录
    let history_dir = dir.path().join("logs").join("login_history");
    let mut entries = std::fs::read_dir(&history_dir)
        .expect("读取登录历史目录失败")
        .collect::<Result<Vec<_>, _>>()
        .expect("遍历登录历史目录失败");
    assert_eq!(
        entries.len(),
        1,
        "应恰好落盘一个历史文件；stderr: {stderr_text}"
    );
    let history_text =
        std::fs::read_to_string(entries.pop().expect("非空").path()).expect("读取登录历史失败");
    let record: serde_json::Value = history_text
        .lines()
        .last()
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("登录历史应为 JSONL: {history_text}"));
    assert_eq!(record["source"], "login_once", "{record}");
    assert_eq!(record["channel"], "http", "{record}");
    assert_eq!(record["result"], "success", "{record}");
    assert_eq!(record["profile_id"], "default", "{record}");
}

/// 携带 token 请求 JSON API（仅 macOS 轻量降级分支使用）。
///
/// 传输层错误（连接重置/未就绪）按 500ms 步进重试至多 10s：监听端口就绪与
/// serve task 开始读请求之间存在启动窗口，首次请求可能被重置。
#[cfg(target_os = "macos")]
async fn api_get(api_base: &str, path: &str, token: &str) -> serde_json::Value {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("构建 HTTP 客户端失败");
    let url = format!("{api_base}{path}");
    let mut last_err = None;
    let mut resp = None;
    for _ in 0..20 {
        match client.get(&url).header("X-Auth-Token", token).send().await {
            Ok(r) => {
                resp = Some(r);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let resp = match resp {
        Some(resp) => resp,
        None => panic!("API {path} 持续请求失败: {:?}", last_err),
    };
    let status = resp.status();
    let text = resp.text().await.expect("读取 API 响应失败");
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("API {path} 响应非 JSON（{status}）: {e}; {text:?}"));
    assert!(status.is_success(), "GET {path} 返回 {status}: {value}");
    value
}

/// 等待实例写入鉴权 token 文件并返回内容（仅 macOS 轻量降级分支使用）
#[cfg(target_os = "macos")]
async fn wait_auth_token(base: &Path) -> String {
    let path = base.join("config").join(".auth_token");
    let appeared = wait_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&path)
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false)
    })
    .await;
    assert!(appeared, "实例未在期限内写入鉴权 token 文件");
    std::fs::read_to_string(&path)
        .expect("读取 token 文件失败")
        .trim()
        .to_string()
}

/// `--stop` 优雅关闭（重试三次对齐既有用例的冷启动口径；仅 macOS 分支使用）
#[cfg(target_os = "macos")]
fn stop_instance_gracefully(base: &str, err_log: &Path, label: &str) {
    let mut last = None;
    for _ in 0..3 {
        let output = Command::cargo_bin("campus-auth")
            .unwrap()
            .args(["--stop", "--base-path", base])
            .timeout(Duration::from_secs(30))
            .output()
            .expect("--stop 执行失败");
        let text = format!(
            "code={} stdout={} stderr={}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        last = Some(text.clone());
        if text.contains("实例已停止") {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!(
        "{label} --stop 应报告成功: {:?}；实例 stderr: {}",
        last,
        std::fs::read_to_string(err_log).unwrap_or_default()
    );
}
