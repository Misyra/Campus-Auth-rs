//! 更新通道集成测试：三通道清单选取 + 回退语义 + 上次检查状态落盘
//!
//! 进程内起一个回环 HTTP mock 模拟 GitHub Releases API 形态
//! （`releases/latest` 单发布 + `releases` 列表 + `.sha256` 伴随文件），
//! 经真实 `ConfigService` + `UpdaterService::check_update` 走完整网络路径。
//! 更新器对回环 http 放行（见 `check::is_allowed_update_url`），无需 TLS。

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use campus_auth::config::ConfigService;
use campus_auth::status::StatusManager;
use campus_auth::updater::{UpdaterError, UpdaterService};

/// 测试进程可能继承系统代理环境变量（reqwest 跟随系统代理），
/// 回环 mock 流量必须直连，统一声明 NO_PROXY。
/// `set_var` 在 2024 edition 为 unsafe：测试启动早期（任何 Client 构建前）
/// 单点调用，进程内无并发读取，不存在 UB 窗口。
fn ensure_no_proxy() {
    // SAFETY: 见上；所有测试在任何 reqwest Client 构建前先行执行本函数
    unsafe {
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
    }
}

/// 64 位 hex 摘要（格式合法即可，检查路径不校验内容与实物一致性）
const FAKE_SHA: &str = "a0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// 本地安装包用例放置的文件内容
///
/// 检查阶段只比对摘要与大小（不解压），故内容只需**非空且大小确定**：真实 zip
/// 结构会引入与"本地包命中"无关的解析面，且解压失败会掩盖本用例的断言目标。
const LOCAL_PAYLOAD: &[u8] = b"fake-local-package-payload";

/// 当前 crate 版本的核心段（major/minor/patch），编译期常量。
///
/// mock 的远程版本一律据此派生而**不写死**：`check_update` 只在 `remote > current`
/// 时返回 `Some`，写死的远程版本会在版本号提升到同一号时失效——v5.0.0 提升时即发生
/// （mock 的远程正式版是 5.0.0、当前版本也是 5.0.0 → `remote > current` 为假 →
/// 返回 `None` → 三平台稳定失败）。
fn current_version_core() -> (u64, u64, u64) {
    let full = env!("CARGO_PKG_VERSION");
    let core = full.split('-').next().unwrap_or(full);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// 远程正式版 tag：当前版本 patch +1，保证严格高于当前版本
fn remote_stable_tag() -> String {
    let (major, minor, patch) = current_version_core();
    format!("{major}.{minor}.{}", patch + 1)
}

/// 远程预发布 tag：当前版本 minor +1 的 beta.1。
/// minor 更大即保证高于 `remote_stable_tag()`（与 patch 无关），
/// 故「全通道取最高」与「测试版取最高」都稳定选中它。
fn remote_prerelease_tag() -> String {
    let (major, minor, _) = current_version_core();
    format!("{major}.{}.0-beta.1", minor + 1)
}

/// 旧的预发布 tag：当前版本的 alpha.7（低于当前版本）。
/// 用于验证「列表首位不是 semver 最大」——它是列表第一个却是最低版本。
fn remote_old_prerelease_tag() -> String {
    let (major, minor, patch) = current_version_core();
    format!("{major}.{minor}.{patch}-alpha.7")
}

/// 远高于当前版本的 tag（major +4）：草稿条目与镜像清单版本共用
fn remote_far_tag() -> String {
    let (major, _, _) = current_version_core();
    format!("{}.0.0", major + 4)
}

/// 测试侧的当前平台键，与生产侧 `check::CURRENT_PLATFORM_KEY` 的 cfg 矩阵一致
fn current_platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return "windows-x64";
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    return "windows-arm64";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "linux-x64";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "linux-arm64";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "macos-arm64";
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "macos-x64";
    #[allow(unreachable_code)]
    {
        "unknown"
    }
}

/// 为指定 tag 构造含当前平台安装包 + `.sha256` 伴随文件的 assets 数组
///
/// 各平台资产全部给出（windows-x64 / linux-x86_64 / macos-arm64 / linux-aarch64），
/// 生产侧 `collect_current_platform_asset` 只取与编译期 `CURRENT_PLATFORM_KEY`
/// 匹配的一项，多余条目在各平台均被跳过——此前仅给 win/linux 导致
/// macOS 与 linux-arm64 CI 上 `PlatformNotAvailable`。
fn assets_json(port: u16, tag: &str) -> serde_json::Value {
    let base = format!("http://127.0.0.1:{port}/assets/{tag}");
    serde_json::json!([
        { "name": format!("app-{tag}-windows-x64.zip"), "browser_download_url": format!("{base}-win.zip"), "size": 16 },
        { "name": format!("app-{tag}-windows-x64.zip.sha256"), "browser_download_url": format!("{base}-win.zip.sha256") },
        { "name": format!("app-{tag}-linux-x86_64.zip"), "browser_download_url": format!("{base}-linux.zip"), "size": 16 },
        { "name": format!("app-{tag}-linux-x86_64.zip.sha256"), "browser_download_url": format!("{base}-linux.zip.sha256") },
        { "name": format!("app-{tag}-linux-aarch64.zip"), "browser_download_url": format!("{base}-linux-arm.zip"), "size": 16 },
        { "name": format!("app-{tag}-linux-aarch64.zip.sha256"), "browser_download_url": format!("{base}-linux-arm.zip.sha256") },
        { "name": format!("app-{tag}-macos-arm64.zip"), "browser_download_url": format!("{base}-macos-arm.zip"), "size": 16 },
        { "name": format!("app-{tag}-macos-arm64.zip.sha256"), "browser_download_url": format!("{base}-macos-arm.zip.sha256") },
        { "name": format!("app-{tag}-macos-x64.zip"), "browser_download_url": format!("{base}-macos-x64.zip"), "size": 16 },
        { "name": format!("app-{tag}-macos-x64.zip.sha256"), "browser_download_url": format!("{base}-macos-x64.zip.sha256") },
    ])
}

/// 单个 GitHub Release 对象
fn release_json(port: u16, tag: &str, prerelease: bool, draft: bool) -> serde_json::Value {
    serde_json::json!({
        "tag_name": format!("v{tag}"),
        "prerelease": prerelease,
        "draft": draft,
        "published_at": "2026-09-01T00:00:00Z",
        "body": format!("changelog of {tag}"),
        "assets": assets_json(port, tag),
    })
}

/// 回环 mock：按路径返回 GitHub API 形态响应
///
/// 各版本一律由当前 crate 版本派生（见上方 `remote_*_tag` 系列），不写死具体号——
/// 「远程版本高于当前版本」是本文件的共同前提，写死会在版本号提升到同一号时集体失效。
///
/// - `GET /repos/o/r/releases/latest` → 单发布（正式，patch +1）
/// - `GET /repos/o/r/releases` → 列表：旧 alpha（列表首位）、正式（patch +1）、
///   beta（minor +1）、更高版本的 draft（应被排除）——顺序有意与 semver 逆序，
///   用于验证选取按 semver 最大而非列表首位
/// - `GET /repos/empty/r/releases/latest` / `.../releases` → 仅正式版
///   （测试版通道无预发布时的回退数据源）
/// - `GET /mirror/latest.json` → 自定清单格式（major +4，非 GitHub 来源回退数据源）
/// - `GET /mirror/local.json` → 自定清单格式，其 `sha256` 取自 [`GithubMockGuard::local_sha`]
///   （本地安装包用例：测试把该值设为放置文件的真实摘要）
/// - `GET *.sha256` → 64 位 hex 摘要文本
///
/// 返回守卫（COR-7）：Drop 时置停止标志并回连唤醒阻塞的 accept，join 监听
/// 线程退出——此前 6 个用例累计泄漏 6 个监听线程与端口。
fn spawn_github_mock() -> GithubMockGuard {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定回环端口失败");
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    // 本地安装包用例的清单摘要（测试侧在发起请求前写入）
    let local_sha = std::sync::Arc::new(std::sync::Mutex::new(FAKE_SHA.to_string()));
    let local_sha_for_thread = local_sha.clone();
    // 已请求路径记录：用于断言"本地包命中时未走网络下载"
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let requests_for_thread = requests.clone();
    let join = std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if stop_for_thread.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut stream = stream;
            let local_sha = local_sha_for_thread.clone();
            let requests = requests_for_thread.clone();
            // 逐请求处理（Connection: close，无需并发）
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                // 读到请求头结束即可（无请求体）
                loop {
                    let Ok(n) = stream.read(&mut chunk) else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf);
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                // 去掉 query（per_page=100 等）
                let path = path.split('?').next().unwrap_or("").to_string();
                if let Ok(mut log) = requests.lock() {
                    log.push(path.clone());
                }

                let body: Option<(String, &'static str)> = if path.ends_with(".sha256") {
                    Some((format!("{FAKE_SHA}\n"), "text/plain"))
                } else {
                    // 平台安装包本体：检查路径不会下载，占位即可
                    match path.as_str() {
                        p if p.starts_with("/assets/") => {
                            Some(("fake-payload".into(), "application/zip"))
                        }
                        "/repos/o/r/releases/latest" | "/repos/empty/r/releases/latest" => Some((
                            release_json(port, &remote_stable_tag(), false, false).to_string(),
                            "application/json",
                        )),
                        "/repos/o/r/releases" => Some((
                            serde_json::json!([
                                release_json(port, &remote_old_prerelease_tag(), true, false),
                                release_json(port, &remote_stable_tag(), false, false),
                                release_json(port, &remote_prerelease_tag(), true, false),
                                release_json(port, &remote_far_tag(), false, true),
                            ])
                            .to_string(),
                            "application/json",
                        )),
                        "/repos/empty/r/releases" => Some((
                            serde_json::json!([release_json(
                                port,
                                &remote_stable_tag(),
                                false,
                                false
                            )])
                            .to_string(),
                            "application/json",
                        )),
                        "/mirror/latest.json" => Some((
                            serde_json::json!({
                                "version": remote_far_tag(),
                                "platforms": {
                                    // 自定清单按平台键精确查找：键必须与编译期
                                    // CURRENT_PLATFORM_KEY 一致，否则 check_update
                                    // 经 select_platform 查不到包而回 Ok(None)
                                    // （此前写死 windows-x64，非 Windows 全挂）。
                                    // 直接复用 GitHub 形态的平台键名：
                                    // windows-x64 / linux-x64 / linux-arm64 /
                                    // macos-arm64 / macos-x64。
                                    current_platform_key(): {
                                        "url": format!("http://127.0.0.1:{port}/assets/win.zip"),
                                        "sha256": FAKE_SHA,
                                        "size": 16,
                                    },
                                },
                            })
                            .to_string(),
                            "application/json",
                        )),
                        // 本地安装包用例的清单：摘要由测试预先写入（见 local_sha），
                        // 大小与测试放置的文件严格一致（命中窗口同时锁定大小预筛分支）
                        "/mirror/local.json" => {
                            let sha = local_sha
                                .lock()
                                .map(|s| s.clone())
                                .unwrap_or_else(|_| FAKE_SHA.to_string());
                            Some((
                                serde_json::json!({
                                    "version": remote_far_tag(),
                                    "platforms": {
                                        current_platform_key(): {
                                            "url": format!("http://127.0.0.1:{port}/assets/win.zip"),
                                            "sha256": sha,
                                            "size": LOCAL_PAYLOAD.len(),
                                        },
                                    },
                                })
                                .to_string(),
                                "application/json",
                            ))
                        }
                        // 上传包用例的"不高于当前版本"来源：清单版本 = 当前版本
                        "/mirror/current.json" => Some((
                            serde_json::json!({
                                "version": env!("CARGO_PKG_VERSION"),
                                "platforms": {
                                    current_platform_key(): {
                                        "url": format!("http://127.0.0.1:{port}/assets/win.zip"),
                                        "sha256": FAKE_SHA,
                                        "size": 16,
                                    },
                                },
                            })
                            .to_string(),
                            "application/json",
                        )),
                        _ => None,
                    }
                };
                let resp = match body {
                    Some((body, content_type)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    GithubMockGuard {
        port,
        stop,
        join: Some(join),
        requests,
        local_sha,
    }
}

/// mock 服务器守卫：Drop 时置停止标志并回连唤醒阻塞的 accept，join 监听线程
/// 退出（COR-7：此前监听线程与端口存活至测试进程结束，无法验证关闭）
struct GithubMockGuard {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    /// 已请求路径（去 query）：断言"本地包命中时未发起下载请求"
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// `/mirror/local.json` 返回的清单摘要（测试在发起请求前写入）
    local_sha: std::sync::Arc<std::sync::Mutex<String>>,
}

impl GithubMockGuard {
    /// 设置 `/mirror/local.json` 返回的清单摘要
    fn set_local_sha(&self, sha: &str) {
        if let Ok(mut slot) = self.local_sha.lock() {
            *slot = sha.to_string();
        }
    }

    /// 是否请求过指定路径
    fn requested(&self, path: &str) -> bool {
        self.requests
            .lock()
            .map(|log| log.iter().any(|p| p == path))
            .unwrap_or(false)
    }
}

impl Drop for GithubMockGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // 回连一次唤醒阻塞在 accept() 的监听线程，使其看到停止标志后退出
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 构造带指定更新源与通道的服务（settings.json 先落盘再由 ConfigService 加载）
async fn service_with(base: &Path, source_url: &str, channel: &str) -> Arc<UpdaterService> {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let config = ConfigService::new(base.to_path_buf(), tx)
        .await
        .expect("构造 ConfigService 失败");
    let mut settings = config.load_settings();
    settings.global.updater.release_source_url = source_url.to_string();
    settings.global.updater.channel =
        serde_json::from_value(serde_json::json!(channel)).expect("合法通道值");
    settings.global.updater.auto_check_enabled = true;
    config.save_settings(&settings).await.expect("写入设置失败");
    UpdaterService::new(config, Arc::new(StatusManager::new()), base.to_path_buf())
}

/// 读取 base 下 update/last_check.json
fn read_last_check(base: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(base.join("update").join("last_check.json"))
        .expect("上次检查状态文件应存在");
    serde_json::from_str(&raw).expect("状态文件应为合法 JSON")
}

/// 正式版通道：直取 releases/latest 单发布（高于当前版本）
#[tokio::test]
async fn stable_channel_fetches_releases_latest() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("远程正式版应高于当前版本").latest_version,
        remote_stable_tag()
    );
}

/// 测试版通道：仅预发布中取 semver 最高（列表首位是最旧的 alpha，
/// 但远程 beta 版本更高；draft 条目排除）
#[tokio::test]
async fn prerelease_channel_picks_highest_semver() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("远程 beta 应高于当前版本").latest_version,
        remote_prerelease_tag()
    );
}

/// 全通道最新版：正式 + 预发布一起按 semver 取最高
#[tokio::test]
async fn all_channel_picks_highest_overall() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "all",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("远程 beta 应高于当前版本").latest_version,
        remote_prerelease_tag()
    );
}

/// 测试版通道远程无任何预发布：回退正式版清单（releases/latest），不报错不空转
#[tokio::test]
async fn prerelease_channel_falls_back_when_no_prerelease() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/empty/r/releases/latest"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("回退后的远程正式版应高于当前版本")
            .latest_version,
        remote_stable_tag()
    );
}

/// 非 GitHub 形态来源（自定 latest.json 镜像）+ 非正式通道：
/// 无法枚举列表，回退单清单语义（跟随该清单版本）
#[tokio::test]
async fn non_github_source_falls_back_to_single_manifest() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/latest.json"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("镜像清单版本应高于当前").latest_version,
        remote_far_tag()
    );
}

/// 成功检查后刷新 update/last_check.json（时间/版本/结果），设置页"上次检查时间"数据源
#[tokio::test]
async fn check_records_last_check_state_on_success() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "all",
    )
    .await;
    assert!(svc.check_update().await.expect("检查不应失败").is_some());
    let state = read_last_check(dir.path());
    assert!(!state["last_check_at"].as_str().unwrap().is_empty());
    assert_eq!(state["latest_version"], remote_prerelease_tag());
    assert_eq!(state["has_update"], true);
    assert_eq!(state["error"], "");
    // last_check_state 读取器与文件内容一致
    let read_back = svc.last_check_state().expect("应能回读状态");
    assert_eq!(read_back.latest_version, remote_prerelease_tag());
}

/// 检查失败（源不可达）同样刷新状态文件并记录原因，时间照常更新
#[tokio::test]
async fn check_records_error_state_on_failure() {
    ensure_no_proxy();
    // 先占后弃：得到一个确定无人监听的回环端口
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{dead}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    assert!(svc.check_update().await.is_err(), "不可达源应返回错误");
    let state = read_last_check(dir.path());
    assert!(
        !state["error"].as_str().unwrap().is_empty(),
        "失败原因应落盘"
    );
    assert!(!state["last_check_at"].as_str().unwrap().is_empty());
    assert_eq!(state["has_update"], false);
}

/// 计算 64 位 hex SHA256（本地包用例需要真实摘要才能命中）
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// 十六进制编码（避免引入 hex crate 依赖）
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 把安装包放进 `<base>/update/` 根目录（用户的手动更新路径）
fn place_local_package(base: &Path, name: &str, payload: &[u8]) -> std::path::PathBuf {
    let update = base.join("update");
    std::fs::create_dir_all(&update).unwrap();
    let path = update.join(name);
    std::fs::write(&path, payload).unwrap();
    path
}

/// 手动更新（命中）：`update/` 根目录下的包摘要与远程清单一致 →
/// `UpdateInfo.local_package` 回报文件名与大小，供前端提示"将跳过下载"
#[tokio::test]
async fn check_update_reports_matching_local_package() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let sha = sha256_hex(LOCAL_PAYLOAD);
    mock.set_local_sha(&sha);
    place_local_package(
        dir.path(),
        "Campus-Auth-local-windows-x64.zip",
        LOCAL_PAYLOAD,
    );
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/local.json"),
        "all",
    )
    .await;

    let info = svc
        .check_update()
        .await
        .expect("检查不应失败")
        .expect("远程版本更高，应返回新版本信息");
    let local = info
        .local_package
        .expect("摘要一致的本地包应被回报（前端据此提示跳过下载）");
    assert_eq!(local.file_name, "Campus-Auth-local-windows-x64.zip");
    assert_eq!(local.size, LOCAL_PAYLOAD.len() as u64);
    assert_eq!(local.sha256, sha);
}

/// 手动更新（未命中）：`update/` 根目录的包摘要与远程清单不符（用户放了旧版本）
/// → 不回报本地包，走正常网络下载路径
#[tokio::test]
async fn check_update_ignores_stale_local_package() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    // 清单声明的摘要是 FAKE_SHA（默认值），放置的文件摘要与之不同
    place_local_package(dir.path(), "Campus-Auth-old-windows-x64.zip", LOCAL_PAYLOAD);
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/local.json"),
        "all",
    )
    .await;

    let info = svc
        .check_update()
        .await
        .expect("检查不应失败")
        .expect("远程版本更高，应返回新版本信息");
    assert!(
        info.local_package.is_none(),
        "摘要不符的本地文件不得被当作可用更新包"
    );
}

/// 手动更新（防误认）：`update/` 根目录内的程序自身文件与半成品下载文件
/// 内容即便与清单摘要一致，也不得被认作安装包
#[tokio::test]
async fn check_update_skips_non_package_files_in_update_dir() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    let sha = sha256_hex(LOCAL_PAYLOAD);
    mock.set_local_sha(&sha);
    // 三项内容都与清单摘要一致，但都不是安装包：程序自身文件、半成品、隐藏文件
    place_local_package(dir.path(), "pending.json", LOCAL_PAYLOAD);
    place_local_package(dir.path(), "last_check.json", LOCAL_PAYLOAD);
    place_local_package(dir.path(), "helper.lock", LOCAL_PAYLOAD);
    place_local_package(dir.path(), "pkg.zip.crdownload", LOCAL_PAYLOAD);
    place_local_package(dir.path(), ".hidden.zip", LOCAL_PAYLOAD);
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/local.json"),
        "all",
    )
    .await;

    let info = svc
        .check_update()
        .await
        .expect("检查不应失败")
        .expect("远程版本更高，应返回新版本信息");
    assert!(
        info.local_package.is_none(),
        "程序自身文件 / 半成品 / 隐藏文件均不得被认作本地安装包"
    );
    // 检查流程本身写入的 last_check.json 未损坏
    assert_eq!(
        read_last_check(dir.path())["latest_version"],
        remote_far_tag()
    );
}

/// 手动更新（不下载网络包）：命中本地包时应用更新只读本地文件——
/// 断言 mock 从未收到 `/assets/` 下载请求，且远程清单仍被拉取（版本信息来自网络）
///
/// 必须先建出 `<base>/python_worker`：`download_stage_and_pending` 的第一道校验是
/// `self_update_worker_dir`（拒绝 Docker / 外置 Worker 布局），缺该目录会在**触及
/// 本地包分支之前**就返回 `UnsupportedSelfUpdateLayout`，用例会因为"提前失败"而
/// 恒绿——那样它验证的就不是本地包行为，而是一条无关的前置拒绝。
#[tokio::test]
async fn apply_update_uses_local_package_without_download() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    // 布局前置：内置 Worker 目录存在（应用阶段的第一道校验才放行）
    std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
    let sha = sha256_hex(LOCAL_PAYLOAD);
    mock.set_local_sha(&sha);
    place_local_package(
        dir.path(),
        "Campus-Auth-local-windows-x64.zip",
        LOCAL_PAYLOAD,
    );
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/local.json"),
        "all",
    )
    .await;

    let info = svc
        .check_update()
        .await
        .expect("检查不应失败")
        .expect("远程版本更高");
    assert!(info.local_package.is_some(), "前置：本次应命中本地包");

    // LOCAL_PAYLOAD 不是合法 zip，故解压会失败——与本用例目标无关，
    // 只断言"未发起下载请求"：本地包命中即走复制暂存，不触碰网络下载。
    let err = svc
        .apply_update(&info)
        .await
        .expect_err("非 zip 负载应解压失败");
    assert!(
        matches!(err, campus_auth::updater::UpdaterError::ExtractFailed(_)),
        "应已越过本地暂存、在解压阶段失败（而非布局校验提前拒绝）：{err}"
    );
    assert!(
        !mock.requested("/assets/win.zip"),
        "命中本地包时不得发起安装包下载请求"
    );
}

// ============ 手动「选择安装包」（apply_uploaded_package，真实实现） ============

/// 构造一个含 `campus-auth` 可执行文件的真实 zip（与发布包结构的最低要求一致）
///
/// 上传路径会真的解压并校验 `extracted/<exe>` 存在，故不能用占位字节。
fn real_update_zip() -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(exe_name(), zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"#!/bin/sh\necho fake-new-binary\n").unwrap();
    zip.finish().unwrap().into_inner()
}

/// 构造一个 exe 携带 PE VERSIONINFO 资源的真实 zip（版本闸门可提取出版本号）
///
/// exe 内容来自更新器的测试夹具（最小 PE 镜像，见
/// `campus_auth::updater::version_info::build_minimal_pe_with_version`）：
/// 上传路径的版本闸门从解压产物中提取**包内真实版本**，占位字节会被
/// `VersionUnrecognized` 拒绝。
fn versioned_update_zip(version: &str) -> Vec<u8> {
    use std::io::Write;
    let pe = campus_auth::updater::version_info::build_minimal_pe_with_version(version, true);
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(exe_name(), zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&pe).unwrap();
    zip.finish().unwrap().into_inner()
}

/// 当前平台的可执行文件名（与生产侧 `updater::apply::EXE_NAME` 一致）
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "campus-auth.exe"
    } else {
        "campus-auth"
    }
}

/// 写一个临时文件承载"上传"的包（真实路径是 Web 层的 multipart 临时文件）
fn write_upload_temp(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join("uploaded.tmp");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// 上传路径的收尾是 `spawn_helper`——测试环境（`target/debug/deps/`）下没有
/// `campus-auth-helper`，故该步必然以 `HelperSpawnFailed` 失败。
///
/// 这不是被测行为的缺陷，而是环境缺件。用例据此把"走到哪一步"表达清楚：
/// 允许 Ok 或 HelperSpawnFailed 两种结局，但**必须**已经写出 `pending.json`
/// ——那才是 staging（复制→解压→校验→计算 exe 摘要）全部完成的证据。
fn assert_reached_pending(result: Result<String, UpdaterError>, base: &Path) {
    match &result {
        Ok(_) => {}
        Err(UpdaterError::HelperSpawnFailed(_)) => {}
        Err(other) => panic!("应在 spawn helper 前完成暂存，实际失败于: {other}"),
    }
    assert!(
        base.join("update").join("pending.json").exists(),
        "staging 与 pending 应已完成（result={result:?}）"
    );
}

/// 正常路径（真实实现）：包内 exe 携带更高版本（PE VERSIONINFO）+ 内置 Worker
/// 布局 + 合法 zip → 解压、提取版本、计算 exe 摘要并写出 `pending.json`
///
/// 版本闸门**不依赖远程清单**（远程未发版时自编译包也要能装）：更新源指向
/// 确定无人监听的死端口，更新照常走通——离线可用性被本用例锁定。
/// 非 Windows 平台没有 PE 版本提取实现，包内 exe 无法识别版本 → `VersionUnrecognized`
/// （fail-closed，见 `version_info` 模块说明）。
#[tokio::test]
async fn apply_uploaded_package_stages_and_writes_pending() {
    ensure_no_proxy();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
    // 死端口更新源：上传路径不再拉取远程清单，指向不可达源也应照常走通
    let svc = service_with(
        dir.path(),
        "http://127.0.0.1:9/repos/o/r/releases/latest",
        "stable",
    )
    .await;

    if cfg!(windows) {
        let path = write_upload_temp(dir.path(), &versioned_update_zip("9.9.9"));
        let result = svc.apply_uploaded_package("pkg.zip", &path).await;
        assert_reached_pending(result, dir.path());

        // pending 的版本是**包内提取**的版本号（而非远程清单）；
        // sha256 必须是**解压出的 exe** 的摘要（而非压缩包摘要），
        // 否则 helper 侧的复核恒失败
        let pending: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("update/pending.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pending["version"], "9.9.9");
        let extracted = dir.path().join("update/staging/extracted").join(exe_name());
        assert!(extracted.exists(), "解压产物应存在于 staging");
        assert_eq!(
            pending["sha256"].as_str().unwrap(),
            sha256_hex(&std::fs::read(&extracted).unwrap()),
            "pending.sha256 应为解压后 exe 的摘要"
        );
    } else {
        // 非 Windows：夹具/占位 exe 均无版本资源可解析 → 明确拒绝且不写 pending
        let path = write_upload_temp(dir.path(), &real_update_zip());
        let err = svc
            .apply_uploaded_package("pkg.zip", &path)
            .await
            .expect_err("非 Windows 平台无法从包内提取版本，应拒绝");
        assert!(matches!(err, UpdaterError::VersionUnrecognized), "{err}");
        assert!(!dir.path().join("update").join("pending.json").exists());
    }
}

/// 版本闸门（真实实现）：包内版本不高于当前版本 → 拒绝，且不写 pending
///
/// 版本号来自解压产物 exe 的 PE VERSIONINFO（Windows）；这条闸门与 helper 侧
/// `pending_version_allowed` 同口径——放行会写下一个 helper 必然拒绝的 pending，
/// 留下永远无法应用的待定更新（用户看到"已就绪"却永远更新不了）。
/// 非 Windows 平台无法提取版本，在版本闸门之前就以 `VersionUnrecognized` 拒绝。
#[tokio::test]
async fn apply_uploaded_package_rejects_when_package_not_newer() {
    ensure_no_proxy();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
    let svc = service_with(
        dir.path(),
        "http://127.0.0.1:9/repos/o/r/releases/latest",
        "stable",
    )
    .await;

    if cfg!(windows) {
        // 夹具 PE 的包内版本 0.0.1 低于当前版本
        let path = write_upload_temp(dir.path(), &versioned_update_zip("0.0.1"));
        let err = svc
            .apply_uploaded_package("pkg.zip", &path)
            .await
            .expect_err("包内版本不高于当前时应拒绝");
        assert!(matches!(err, UpdaterError::PackageNotNewer { .. }), "{err}");
        assert!(
            !dir.path().join("update").join("pending.json").exists(),
            "被拒绝时不得写入 pending"
        );
    } else {
        let path = write_upload_temp(dir.path(), &real_update_zip());
        let err = svc
            .apply_uploaded_package("pkg.zip", &path)
            .await
            .expect_err("非 Windows 平台无法提取版本应拒绝");
        assert!(matches!(err, UpdaterError::VersionUnrecognized), "{err}");
        assert!(!dir.path().join("update").join("pending.json").exists());
    }
}

/// 布局闸门（真实实现）：外置 Worker（无内置 `<base>/python_worker`）→ 拒绝
///
/// 与本地包复用、网络下载走同一道 `self_update_worker_dir`：Docker / 外置 Worker
/// 由各自部署系统更新，应用内 overlay 会造成主程序与 Worker 版本分裂。
#[tokio::test]
async fn apply_uploaded_package_rejects_external_worker_layout() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    // 故意不建 python_worker（模拟外置布局）
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    let path = write_upload_temp(dir.path(), &real_update_zip());

    let err = svc
        .apply_uploaded_package("pkg.zip", &path)
        .await
        .expect_err("外置 Worker 布局应被拒绝");
    assert!(
        matches!(err, UpdaterError::UnsupportedSelfUpdateLayout(_)),
        "{err}"
    );
    assert!(
        !dir.path().join("update").join("pending.json").exists(),
        "被拒绝时不得写入 pending"
    );
}

/// 解压失败（不是压缩包）：报错且不写 pending
///
/// 上传路径不比对摘要（用户显式选定），因此"包无效"是这条路径最主要的失败模式，
/// 必须确保它不会留下一个指向空 staging 的 pending。
#[tokio::test]
async fn apply_uploaded_package_rejects_invalid_archive() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    let path = write_upload_temp(dir.path(), b"definitely-not-a-zip");

    let err = svc
        .apply_uploaded_package("pkg.zip", &path)
        .await
        .expect_err("非压缩包应失败");
    assert!(matches!(err, UpdaterError::ExtractFailed(_)), "{err}");
    assert!(
        !dir.path().join("update").join("pending.json").exists(),
        "解压失败时不得写入 pending"
    );
}

/// zip 里没有可执行文件：拒绝（防止把不含主程序的包写进 pending）
#[tokio::test]
async fn apply_uploaded_package_rejects_archive_without_exe() {
    ensure_no_proxy();
    let mock = spawn_github_mock();
    let port = mock.port;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("python_worker")).unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    // 合法 zip，但没有 campus-auth 可执行文件
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("README.md", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"no exe here").unwrap();
    let path = write_upload_temp(dir.path(), &zip.finish().unwrap().into_inner());

    let err = svc
        .apply_uploaded_package("pkg.zip", &path)
        .await
        .expect_err("缺少可执行文件应失败");
    assert!(matches!(err, UpdaterError::ExtractFailed(_)), "{err}");
    assert!(!dir.path().join("update").join("pending.json").exists());
}
