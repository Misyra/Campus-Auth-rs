//! 登录全链路集成测试：mock 门户 → 真实二进制 → Worker(Playwright+OCR) → success
//!
//! 覆盖自动化此前从未触及的链路：`POST /api/login` → orchestrator →
//! `execute_login_attempt` → 浏览器填表 + OCR 验证码 → mock `/login` →
//! 登录后网络验证（mock `/generate_204`）→ 历史落盘。
//! 第二轮覆盖验证码失败重试（mock `/failonce`）。
//!
//! 环境门槛（任一缺失即跳过，非失败）：
//! - 本地 Python 且可 `import PIL, ddddocr`（mock 验证码生成 + Worker OCR）
//! - Playwright chromium 已安装（`ms-playwright/chromium*`）
//! - 本地回环未被代理劫持（测试内自带 `no_proxy` 回环）
//!
//! 注意：为隔离并行/本地开发实例，mock 用随机端口（`server.py --port`），
//! profile 与任务经 API 写入临时 base，不污染 `tests/fixtures` 模板。

mod common;

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use common::{InstanceGuard, free_port, locate_python, spawn_instance, wait_listening};
use serde_json::{Value, json};

/// 环境预检：返回可用的 Python 解释器，缺失任一条件则打印原因后跳过
fn preflight() -> Option<PathBuf> {
    let python = locate_python()?;
    let check = |module: &str| {
        std::process::Command::new(&python)
            .args(["-c", &format!("import {module}")])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    for module in ["PIL", "ddddocr"] {
        if !check(module) {
            eprintln!(
                "跳过 login_chain：Python 缺少 {module}（mock 需 PIL，Worker OCR 需 ddddocr）"
            );
            return None;
        }
    }
    // Playwright chromium 是否已安装（CI 由 e2e job 预装，本地按需 `playwright install`）
    let cache = dirs::cache_dir().map(|c| c.join("ms-playwright"));
    let has_chromium = cache.is_some_and(|dir| {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                e.file_name().to_str().is_some_and(|n| {
                    n.starts_with("chromium") || n.starts_with("chromium_headless_shell")
                })
            })
        })
    });
    if !has_chromium {
        eprintln!("跳过 login_chain：未找到 Playwright chromium（ms-playwright）");
        return None;
    }
    Some(python)
}

/// 确保回环直连（测试机代理如 127.0.0.1:7890 会劫持 reqwest/urllib 回环导致 502）
fn ensure_loopback_bypass() {
    for var in ["NO_PROXY", "no_proxy"] {
        let cur = std::env::var(var).unwrap_or_default();
        let mut parts: Vec<&str> = cur
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        for need in ["127.0.0.1", "localhost"] {
            if !parts.contains(&need) {
                parts.push(need);
            }
        }
        // Edition 2024 下 set_var 为 unsafe：单测进程内串行设置，无并发写
        unsafe {
            std::env::set_var(var, parts.join(","));
        }
    }
}

struct MockPortal {
    _guard: InstanceGuard,
    port: u16,
}

fn spawn_mock(python: &PathBuf) -> Option<MockPortal> {
    let port = free_port();
    let server = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("mock-servers")
        .join("full-portal")
        .join("server.py");
    let log = tempfile::NamedTempFile::new().ok()?;
    let err_file = log.reopen().ok()?;
    let child: Child = std::process::Command::new(python)
        .args([server.to_str()?, "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file))
        .spawn()
        .ok()?;
    let guard = InstanceGuard::with_stderr_log(child, log);
    // 等待端口可达（最多 15s），失败打印 mock stderr 便于定位（缺 PIL 等）
    let mut ok = false;
    for _ in 0..60 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !ok {
        let tail = std::fs::read_to_string(guard.stderr_path()).unwrap_or_default();
        eprintln!("跳过 login_chain：mock 门户未在期限内启动（127.0.0.1:{port}）；stderr: {tail}");
        return None;
    }
    Some(MockPortal {
        _guard: guard,
        port,
    })
}

struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl Api {
    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let mut req = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if !self.token.is_empty() {
            req = req.header("X-Auth-Token", &self.token);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("API 请求发送失败");
        let status = resp.status();
        let v: Value = resp.json().await.expect("API 响应非 JSON");
        assert!(
            status.is_success(),
            "API {method} {path} 返回 {status}：{v}"
        );
        v["data"].clone()
    }
}

async fn wait_token(base_path: &std::path::Path) -> String {
    let path = base_path.join("config").join(".auth_token");
    for _ in 0..40 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("实例未在期限内写入 .auth_token");
}

fn login_task_json(mock_base: &str) -> Value {
    json!({
        "type": "browser",
        "task_id": "mock-login",
        "name": "mock 门户登录",
        "url": format!("{mock_base}/"),
        "steps": [
            {"id": "s1", "type": "input", "selector": "#username", "value": "{{USERNAME}}", "description": "填账号"},
            {"id": "s2", "type": "input", "selector": "#password", "value": "{{PASSWORD}}", "description": "填密码"},
            {"id": "s3", "type": "ocr", "selector": "#captcha-img", "target_selector": "#captcha-input", "description": "识别验证码并填入"},
            {"id": "s4", "type": "click", "selector": "#login-btn", "description": "提交登录"}
        ]
    })
}

/// 全链路：登录成功（mock 已认证 + 历史落盘），随后 failonce 重试成功
#[tokio::test]
async fn login_chain_success_then_failonce_retry() {
    let Some(python) = preflight() else {
        return;
    };
    ensure_loopback_bypass();
    let Some(mock) = spawn_mock(&python) else {
        return;
    };
    let mock_base = format!("http://127.0.0.1:{}", mock.port);

    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();
    let _instance = spawn_instance(&base, port);
    assert!(wait_listening(port), "实例未在期限内监听端口 {port}");
    let token = wait_token(dir.path()).await;
    let api = Api {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(200))
            .no_proxy()
            .build()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
        token,
    };

    // 1) 凭证指向 mock（服务端加密落盘，明文不写模板）
    api.request(
        "PUT",
        "/api/profiles/default",
        Some(json!({
            "username": "testuser",
            "password": "testpass",
            "auth_url": format!("{mock_base}/"),
        })),
    )
    .await;
    // 2) 登录任务 + 设为活跃
    api.request(
        "PUT",
        "/api/tasks/mock-login",
        Some(login_task_json(&mock_base)),
    )
    .await;
    api.request("POST", "/api/tasks/active/mock-login", None)
        .await;
    // 3) 监测只看 mock（登录后网络验证以此判定 Online）
    api.request(
        "PATCH",
        "/api/config",
        Some(json!({
            "monitor": {
                "check_interval_seconds": 5,
                "test_urls": [format!("{mock_base}/generate_204")],
                "enable_http_check": true,
                "enable_tcp_check": false,
                "enable_local_check": false,
                "network_check_timeout": 5,
            }
        })),
    )
    .await;

    // 4) 第一轮：同步登录，期望成功（client 超时 200s 覆盖 login_timeout 120s）
    let r = api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "首轮登录应成功，message={}（mock 日志见 /status）",
        r["message"]
    );

    // 5) mock 侧已认证 + 历史落盘
    let mock_status: Value = api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert_eq!(mock_status["authenticated"], true);
    assert_eq!(mock_status["username"], "testuser");
    assert!(
        mock_status["login_count"].as_u64().unwrap_or(0) >= 1,
        "mock 应记录至少一次登录：{mock_status}"
    );
    let history = api.request("GET", "/api/history", None).await;
    let entries = history.as_array().cloned().unwrap_or_default();
    assert!(!entries.is_empty(), "登录历史应落盘首条记录");

    // 6) 第二轮：先模拟掉线（否则登录后验证沿用首轮已认证态，成功是空心的），
    // 再 arm failonce 强制验证码失败一次，重试后仍成功
    api.client
        .post(format!("{mock_base}/logout"))
        .send()
        .await
        .expect("mock /logout 不可达");
    api.client
        .post(format!("{mock_base}/failonce"))
        .send()
        .await
        .expect("mock /failonce 不可达");
    let before = mock_status["login_count"].as_u64().unwrap_or(0);
    let r = api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "failonce 后重试应成功：{}",
        r["message"]
    );
    let mock_status: Value = api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert!(
        mock_status["login_count"].as_u64().unwrap_or(0) > before,
        "重试轮应产生新的成功登录：{mock_status}"
    );
}
