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
use campus_auth::updater::UpdaterService;

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
/// - `GET /repos/o/r/releases/latest` → 单发布 v5.0.0（正式）
/// - `GET /repos/o/r/releases` → 列表：alpha.7（列表首位/最新创建）、5.0.0、
///   5.1.0-beta.1、v9.0.0（draft，应被排除）——顺序有意与 semver 逆序，
///   用于验证选取按 semver 最大而非列表首位
/// - `GET /repos/empty/r/releases/latest` / `.../releases` → 仅正式版 v5.0.0
///   （测试版通道无预发布时的回退数据源）
/// - `GET /mirror/latest.json` → 自定清单格式 v9.9.9（非 GitHub 来源回退数据源）
/// - `GET *.sha256` → 64 位 hex 摘要文本
fn spawn_github_mock() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定回环端口失败");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
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

                let body: Option<(String, &'static str)> = if path.ends_with(".sha256") {
                    Some((format!("{FAKE_SHA}\n"), "text/plain"))
                } else {
                    // 平台安装包本体：检查路径不会下载，占位即可
                    match path.as_str() {
                        p if p.starts_with("/assets/") => {
                            Some(("fake-payload".into(), "application/zip"))
                        }
                        "/repos/o/r/releases/latest" | "/repos/empty/r/releases/latest" => Some((
                            release_json(port, "5.0.0", false, false).to_string(),
                            "application/json",
                        )),
                        "/repos/o/r/releases" => Some((
                            serde_json::json!([
                                release_json(port, "5.0.0-alpha.7", true, false),
                                release_json(port, "5.0.0", false, false),
                                release_json(port, "5.1.0-beta.1", true, false),
                                release_json(port, "9.0.0", false, true),
                            ])
                            .to_string(),
                            "application/json",
                        )),
                        "/repos/empty/r/releases" => Some((
                            serde_json::json!([release_json(port, "5.0.0", false, false)])
                                .to_string(),
                            "application/json",
                        )),
                        "/mirror/latest.json" => Some((
                            serde_json::json!({
                                "version": "9.9.9",
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
    port
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

/// 正式版通道：直取 releases/latest 单发布 v5.0.0（高于当前 alpha 版）
#[tokio::test]
async fn stable_channel_fetches_releases_latest() {
    ensure_no_proxy();
    let port = spawn_github_mock();
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "stable",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(info.expect("5.0.0 高于当前 alpha").latest_version, "5.0.0");
}

/// 测试版通道：仅预发布中取 semver 最高（列表首位 alpha.7 是最新创建的，
/// 但 5.1.0-beta.1 版本更高；draft 9.0.0 排除）
#[tokio::test]
async fn prerelease_channel_picks_highest_semver() {
    ensure_no_proxy();
    let port = spawn_github_mock();
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("5.1.0-beta.1 高于当前 alpha").latest_version,
        "5.1.0-beta.1"
    );
}

/// 全通道最新版：正式 + 预发布一起按 semver 取最高
#[tokio::test]
async fn all_channel_picks_highest_overall() {
    ensure_no_proxy();
    let port = spawn_github_mock();
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        "all",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("5.1.0-beta.1 高于当前 alpha").latest_version,
        "5.1.0-beta.1"
    );
}

/// 测试版通道远程无任何预发布：回退正式版清单（releases/latest），不报错不空转
#[tokio::test]
async fn prerelease_channel_falls_back_when_no_prerelease() {
    ensure_no_proxy();
    let port = spawn_github_mock();
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/repos/empty/r/releases/latest"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("回退后 5.0.0 高于当前 alpha").latest_version,
        "5.0.0"
    );
}

/// 非 GitHub 形态来源（自定 latest.json 镜像）+ 非正式通道：
/// 无法枚举列表，回退单清单语义（跟随该清单版本）
#[tokio::test]
async fn non_github_source_falls_back_to_single_manifest() {
    ensure_no_proxy();
    let port = spawn_github_mock();
    let dir = tempfile::tempdir().unwrap();
    let svc = service_with(
        dir.path(),
        &format!("http://127.0.0.1:{port}/mirror/latest.json"),
        "prerelease",
    )
    .await;
    let info = svc.check_update().await.expect("检查不应失败");
    assert_eq!(
        info.expect("镜像清单 9.9.9 高于当前").latest_version,
        "9.9.9"
    );
}

/// 成功检查后刷新 update/last_check.json（时间/版本/结果），设置页"上次检查时间"数据源
#[tokio::test]
async fn check_records_last_check_state_on_success() {
    ensure_no_proxy();
    let port = spawn_github_mock();
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
    assert_eq!(state["latest_version"], "5.1.0-beta.1");
    assert_eq!(state["has_update"], true);
    assert_eq!(state["error"], "");
    // last_check_state 读取器与文件内容一致
    let read_back = svc.last_check_state().expect("应能回读状态");
    assert_eq!(read_back.latest_version, "5.1.0-beta.1");
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
