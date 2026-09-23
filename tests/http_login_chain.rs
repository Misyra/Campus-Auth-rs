//! 直连 HTTP 登录全链路：无 Python/Worker 环境下由真实二进制完成登录。

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{InstanceGuard, free_port, wait_listening};
use serde_json::{Value, json};

struct DirectPortal {
    addr: std::net::SocketAddr,
    authenticated: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DirectPortal {
    fn spawn() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
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

impl Drop for DirectPortal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_portal_request(mut stream: TcpStream, authenticated: &AtomicBool) {
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
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("");

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

struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl Api {
    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("X-Auth-Token", &self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("API 请求失败");
        let status = response.status();
        let text = response.text().await.expect("读取 API 响应失败");
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("API {method} {path} 响应非 JSON（{status}）: {e}; {text:?}")
        });
        assert!(
            status.is_success(),
            "{method} {path} 返回 {status}: {value}"
        );
        value["data"].clone()
    }
}

async fn wait_token(base: &std::path::Path) -> String {
    let path = base.join("config").join(".auth_token");
    for _ in 0..40 {
        if let Ok(token) = std::fs::read_to_string(&path) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return token;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("实例未写入鉴权 token");
}

#[tokio::test]
async fn http_login_succeeds_without_python_or_worker_setup() {
    let portal = DirectPortal::spawn();
    let dir = tempfile::TempDir::new().unwrap();
    let port = free_port();
    let log = tempfile::NamedTempFile::new().unwrap();
    let stderr = log.reopen().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_campus-auth"))
        .args([
            "--base-path",
            dir.path().to_str().unwrap(),
            "--port",
            &port.to_string(),
            "--no-tray",
            "--no-browser",
            "--mode",
            "full",
        ])
        // 直连登录不得依赖 PATH 中的 Python/uv；空 PATH 会让误入环境引导立即暴露。
        .env("PATH", "")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let _instance = InstanceGuard::with_stderr_log(child, log);
    assert!(wait_listening(port), "实例未监听端口 {port}");

    let api = Api {
        client: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
        token: wait_token(dir.path()).await,
    };
    let portal_base = portal.base();
    // 直连配置先落成一个直连任务（`tasks/http/<id>.json`）：v10 起方案里不再有
    // 请求地址/方法/判定关键字等字段，方案只持有 `active_http_task` 这条绑定。
    // 顺序不能反——保存方案时会校验绑定指向的任务存在且类型为 http。
    api.request(
        "PUT",
        "/api/tasks/portal-http",
        Some(json!({
            "type": "http",
            "task_id": "portal-http",
            "name": "直连门户",
            "method": "POST",
            "url": format!("{portal_base}/login"),
            "body": "username={username}&password={password}",
            "success_pattern": "登录成功",
            "failure_pattern": "账号或密码错误"
        })),
    )
    .await;
    api.request(
        "PUT",
        "/api/profiles/default",
        Some(json!({
            "username": "testuser",
            "password": "testpass",
            "login_channel": "http",
            "active_http_task": "portal-http"
        })),
    )
    .await;
    api.request(
        "PATCH",
        "/api/config",
        Some(json!({
            "monitor": {
                "test_urls": [format!("{portal_base}/generate_204")],
                "enable_http_check": true,
                "enable_tcp_check": false,
                "enable_url_check": false,
                "enable_local_check": false,
                "post_login_delay": 0
            }
        })),
    )
    .await;

    // 任务测试端点（方案编辑器入口）：只给 task_id + profile_id，密码留空用方案已存密码。
    // 这条同时验证「新端点 → 已保存任务 → 方案凭据」在真实二进制里真的连通，
    // 而不只是 mock 单测通过。
    let probe = api
        .request(
            "POST",
            "/api/http-tasks/test",
            Some(json!({
                "task_id": "portal-http",
                "profile_id": "default",
                "username": "testuser",
                "password": "",
                "fetch_page": false
            })),
        )
        .await;
    assert_eq!(probe["outcome"], "success", "直连测试端点应成功: {probe}");
    assert!(
        probe["rendered_url"]
            .as_str()
            .unwrap_or_default()
            .contains("/login"),
        "测试端点渲染地址应来自直连任务的 url: {probe}"
    );
    assert!(
        !probe.to_string().contains("testpass"),
        "测试响应不得回显方案密码: {probe}"
    );

    let result = api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(result["success"], true, "直连登录应成功: {result}");
    assert!(portal.authenticated.load(Ordering::Acquire));
    assert!(
        !dir.path().join("python_worker").exists(),
        "直连登录不应创建或复制 Python Worker"
    );
}
