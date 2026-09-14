//! 登录全链路集成测试：mock 门户 → 真实二进制 → Worker(Playwright+OCR) → success
//!
//! 覆盖自动化此前从未触及的链路：`POST /api/login` → orchestrator →
//! `execute_login_attempt` → 浏览器填表 + OCR 验证码 → mock `/login` →
//! 登录后网络验证（mock `/generate_204`）→ 历史落盘。
//! 第二轮覆盖验证码失败重试（mock `/failonce`）。
//! 扩展轮（对应 2026-09-13 E:\Test 便携版实测场景）：
//! - 302 跳转链登录（portal-v2 三级跳转，Worker 落地登录页）
//! - 慢响应登录（`/slowlogin` 延迟注入，仍在等待窗口内成功）
//! - 限时封禁（`/ban` 首次拒绝，重试跨过封禁窗口成功）
//! - kick 掉线 → 监测发现 captive → 引擎自动重登（全自动，无手动触发）
//!
//! 环境门槛（任一缺失即跳过，非失败）：
//! - 本地 Python 且可 `import PIL, ddddocr`（mock 验证码生成 + Worker OCR）
//! - Playwright chromium 已安装（`ms-playwright/chromium*`）
//! - 本地回环未被代理劫持（测试内自带 `no_proxy` 回环）
//!
//! 注意：为隔离并行/本地开发实例，mock 用随机端口（`server.py --port`），
//! profile 与任务经 API 写入临时 base，不污染 `tests/fixtures` 模板。
//! 用例级串行（SERIAL）：每个用例都要独占启动浏览器 + OCR，并发只会互相拖慢。

mod common;

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use common::{
    InstanceGuard, free_port, locate_python, preset_ocr_preference, spawn_instance, wait_listening,
};
use serde_json::{Value, json};

/// 用例级串行锁：浏览器 + OCR 会话无法有效并发，异步感知锁避免跨 await 持锁告警
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

impl MockPortal {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn spawn_mock_on(python: &PathBuf, script_dir: &str) -> Option<MockPortal> {
    let port = free_port();
    let server = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("mock-servers")
        .join(script_dir)
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

/// 一套隔离测试环境：mock 门户 + 真实实例 + 已认证 API 客户端。
/// 字段顺序即 drop 顺序：实例先停，mock 门户后停。
struct TestEnv {
    _dir: tempfile::TempDir,
    _instance: InstanceGuard,
    api: Api,
    mock: MockPortal,
}

async fn setup_env(python: &PathBuf, mock_script: &str) -> Option<TestEnv> {
    ensure_loopback_bypass();
    let mock = spawn_mock_on(python, mock_script)?;
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    // 登录用例依赖 OCR 识别验证码：预置偏好启用，实例引导会把 ddddocr
    // `uv add` 进 base 内的 worker 副本（等价真实用户点「安装 OCR 依赖」）
    preset_ocr_preference(dir.path());
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();
    let instance = spawn_instance(&base, port);
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
    Some(TestEnv {
        _dir: dir,
        _instance: instance,
        api,
        mock,
    })
}

/// 配置凭证 + 登录任务 + 绑定任务到方案 + 监测只看 mock（登录后网络验证以
/// test_urls 判定 Online，必须指向 mock 而非真实公网）
///
/// 任务启用态已改为按方案绑定（`profile.active_task`），全局
/// `POST /api/tasks/active/{id}` 路由已移除。
async fn setup_profile_and_task(env: &TestEnv, auth_url: &str) {
    let mock_base = env.mock.base();
    env.api
        .request(
            "PUT",
            "/api/tasks/mock-login",
            Some(login_task_json(&mock_base)),
        )
        .await;
    env.api
        .request(
            "PUT",
            "/api/profiles/default",
            Some(json!({
                "username": "testuser",
                "password": "testpass",
                "auth_url": auth_url,
                "active_task": "mock-login",
            })),
        )
        .await;
    env.api
        .request(
            "PATCH",
            "/api/config",
            Some(json!({
                "monitor": {
                    "check_interval_seconds": 20,
                    "test_urls": [format!("{mock_base}/generate_204")],
                    "enable_http_check": true,
                    "enable_tcp_check": false,
                    "enable_local_check": false,
                    "network_check_timeout": 5,
                }
            })),
        )
        .await;
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

/// 轮询 mock /status 直至 login_count 达到 min（监测驱动自动登录的等待）
async fn wait_for_login_count(
    client: &reqwest::Client,
    mock_base: &str,
    min: u64,
    timeout: Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return false;
        }
        if let Ok(resp) = client.get(format!("{mock_base}/status")).send().await {
            if let Ok(v) = resp.json::<Value>().await {
                if v["login_count"].as_u64().unwrap_or(0) >= min {
                    return true;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}

/// 全链路：登录成功（mock 已认证 + 历史落盘），随后 failonce 重试成功
#[tokio::test]
async fn login_chain_success_then_failonce_retry() {
    let _serial = SERIAL.lock().await;
    let Some(python) = preflight() else {
        return;
    };
    let Some(env) = setup_env(&python, "full-portal").await else {
        return;
    };
    let mock_base = env.mock.base();
    setup_profile_and_task(&env, &format!("{mock_base}/")).await;

    // 1) 同步登录，期望成功（client 超时 200s 覆盖 login_timeout 120s）
    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "首轮登录应成功，message={}（mock 日志见 /status）",
        r["message"]
    );

    // 2) mock 侧已认证 + 历史落盘
    let mock_status: Value = env
        .api
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
    let history = env.api.request("GET", "/api/history", None).await;
    let entries = history.as_array().cloned().unwrap_or_default();
    assert!(!entries.is_empty(), "登录历史应落盘首条记录");

    // 3) 第二轮：先模拟掉线（否则登录后验证沿用首轮已认证态，成功是空心的），
    // 再 arm failonce 强制验证码失败一次，重试后仍成功
    env.api
        .client
        .post(format!("{mock_base}/logout"))
        .send()
        .await
        .expect("mock /logout 不可达");
    env.api
        .client
        .post(format!("{mock_base}/failonce"))
        .send()
        .await
        .expect("mock /failonce 不可达");
    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "failonce 后重试应成功：{}",
        r["message"]
    );
    let mock_status: Value = env
        .api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert!(
        mock_status["login_count"].as_u64().unwrap_or(0) >= 2,
        "重试轮应产生新的成功登录：{mock_status}"
    );
}

/// 302 跳转链登录：auth_url 指向三级跳转链入口（/step1 → /step2 → /portal →
/// 登录页），Worker 落地登录页并完成 OCR 登录——真实门户普遍经网关跳转
#[tokio::test]
async fn login_chain_via_redirect_chain() {
    let _serial = SERIAL.lock().await;
    let Some(python) = preflight() else {
        return;
    };
    let Some(env) = setup_env(&python, "portal-v2").await else {
        return;
    };
    let mock_base = env.mock.base();
    setup_profile_and_task(&env, &format!("{mock_base}/step1")).await;

    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "302 跳转链登录应成功，message={}",
        r["message"]
    );

    let mock_status: Value = env
        .api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert_eq!(mock_status["authenticated"], true, "{mock_status}");
}

/// 慢响应登录：门户延迟 3s 应答提交（单次注入），仍应在登录等待与网络验证
/// 窗口内成功——弱网门户的常态
#[tokio::test]
async fn login_chain_survives_slow_portal() {
    let _serial = SERIAL.lock().await;
    let Some(python) = preflight() else {
        return;
    };
    let Some(env) = setup_env(&python, "portal-v2").await else {
        return;
    };
    let mock_base = env.mock.base();
    setup_profile_and_task(&env, &format!("{mock_base}/")).await;
    env.api
        .client
        .post(format!("{mock_base}/slowlogin?ms=3000"))
        .send()
        .await
        .expect("mock /slowlogin 不可达");

    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "慢响应登录应成功，message={}",
        r["message"]
    );
}

/// 限时封禁：登录提交被拒（/ban 2s），重试等待跨过封禁窗口后成功——
/// 验证重试链对"暂时性拒绝"的恢复能力
#[tokio::test]
async fn login_chain_retries_across_ban_window() {
    let _serial = SERIAL.lock().await;
    let Some(python) = preflight() else {
        return;
    };
    let Some(env) = setup_env(&python, "portal-v2").await else {
        return;
    };
    let mock_base = env.mock.base();
    setup_profile_and_task(&env, &format!("{mock_base}/")).await;
    env.api
        .client
        .post(format!("{mock_base}/ban?seconds=2"))
        .send()
        .await
        .expect("mock /ban 不可达");

    // 首次提交落在封禁窗口内被拒（验证码/账密校验前拦截），编排器重试时窗口已过
    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "封禁窗口过后重试应成功，message={}",
        r["message"]
    );
    let mock_status: Value = env
        .api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert_eq!(
        mock_status["login_count"].as_u64().unwrap_or(0),
        1,
        "封禁轮不应计入成功登录：{mock_status}"
    );
}

/// kick 掉线 → 监测发现 captive → 引擎自动重登：手动建立在线基线后全程
/// 不再手动触发登录，验证"断线自动恢复"这一核心卖点的端到端闭环
///
/// （不测"无凭证首轮自动登录"：实例启动即开始检测，凭证经 API 写入晚于
/// 首轮检测，空配置登录失败进入编排器退避，监测翻转不再触发——这是用例
/// 自造的启动竞态，真实用户开着监测时凭证早已配置）
#[tokio::test]
async fn login_chain_auto_relogin_after_kick() {
    let _serial = SERIAL.lock().await;
    let Some(python) = preflight() else {
        return;
    };
    let Some(env) = setup_env(&python, "portal-v2").await else {
        return;
    };
    let mock_base = env.mock.base();
    setup_profile_and_task(&env, &format!("{mock_base}/")).await;

    // 手动登录建立在线基线（login_count = 1）
    let r = env.api.request("POST", "/api/login", Some(json!({}))).await;
    assert_eq!(
        r["success"], true,
        "基线登录应成功，message={}",
        r["message"]
    );

    // 模拟被门户踢下线：204 → 200，监测应在下一轮发现 captive 并自动重登
    env.api
        .client
        .post(format!("{mock_base}/kick"))
        .send()
        .await
        .expect("mock /kick 不可达");

    let second =
        wait_for_login_count(&env.api.client, &mock_base, 2, Duration::from_secs(150)).await;
    assert!(
        second,
        "kick 后自动重登未发生（150s 内 login_count 未达 2）"
    );

    let mock_status: Value = env
        .api
        .client
        .get(format!("{mock_base}/status"))
        .send()
        .await
        .expect("mock /status 不可达")
        .json()
        .await
        .expect("mock /status 非 JSON");
    assert_eq!(
        mock_status["authenticated"], true,
        "重登后门户应为已认证态：{mock_status}"
    );
    assert!(
        mock_status["kick_count"].as_u64().unwrap_or(0) >= 1,
        "mock 应记录 kick 事件：{mock_status}"
    );
}
