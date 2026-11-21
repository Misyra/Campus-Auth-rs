//! 浏览器可用性探测与渠道自动选择
//!
//! 一句话摘要：集中系统浏览器（Edge/Chrome）与 Playwright 管理浏览器的安装
//! 探测，供环境引导、登录预检、浏览器列表 API 共用，避免各模块自建口径。

use std::path::Path;

/// 无可用浏览器时的统一失败文案
///
/// 前端手动登录据此子串（"无可用浏览器"）弹窗引导下载 Chromium；修改措辞时
/// 需同步 `frontend/src/composables/useUi.ts` 的匹配逻辑。
pub const NO_BROWSER_MESSAGE: &str =
    "当前无可用浏览器，请下载 Chromium（设置 · 浏览器页可一键安装）";

/// 解析 App Paths 注册表默认值（REG_SZ/REG_EXPAND_SZ 展开后的宽字符）：
/// 剥首尾 NUL、空白与包裹引号后要求非空路径。
///
/// cfg 含 test 使纯逻辑可在任意平台的测试构建单测（生产仅 Windows 编译）
#[cfg(any(windows, test))]
fn clean_app_paths_value(raw: &[u16]) -> Option<String> {
    let text = String::from_utf16_lossy(raw.strip_suffix(&[0]).unwrap_or(raw));
    let cleaned = text.trim().trim_matches('"');
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// 读取 App Paths 注册表默认值并校验目标存在（P3-39）
///
/// HKLM/HKCU 的 `SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\<exe>`
/// 默认值登记可执行文件完整路径：自定义盘符/自定义目录安装的 Chrome/Edge
/// 不在文件系统候选路径（Program Files/LocalAppData）内，仅能经此查到
///（Chromium 下载兜底不受影响）。经 windows-sys 原生 `RegGetValueW`——与
/// `utils::platform_windows` 的注册表读取同手段，无需新依赖；空值名读默认值，
/// REG_EXPAND_SZ 一并接受由系统展开（部分安装器用 %ProgramFiles% 变量登记）；
/// 登记残留指向已卸载路径时不误报（要求文件存在才返回）
#[cfg(target_os = "windows")]
fn app_paths_executable(exe_name: &str) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::Foundation::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };

    // 路径值远小于 4K 字符；不足时 RegGetValueW 返回错误而非截断误报
    let mut bytes = vec![0u8; 8192];
    let subkey = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe_name}");
    let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    // 空值名（仅 NUL 结尾）读取键的默认值——App Paths 的登记形态
    let default_value_name = [0u16];
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut len = bytes.len() as u32;
        let code = unsafe {
            RegGetValueW(
                root,
                subkey_wide.as_ptr(),
                default_value_name.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                std::ptr::null_mut(),
                bytes.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if code != 0 {
            // 未登记（ERROR_FILE_NOT_FOUND 等）或读取失败：换下一个根键
            continue;
        }
        if let Some(path) = clean_app_paths_value(&bytes[..len as usize / 2]) {
            let path = std::path::PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// 检测系统是否安装了 Google Chrome
#[cfg(target_os = "windows")]
pub fn is_chrome_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(std::env::var("PROGRAMFILES").unwrap_or_default())
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        std::path::PathBuf::from(std::env::var("PROGRAMFILES(X86)").unwrap_or_default())
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
    ];
    candidates.iter().any(|p| p.exists())
        || app_paths_executable("chrome.exe").is_some()
}

/// 检测系统是否安装了 Google Chrome（macOS）
#[cfg(target_os = "macos")]
pub fn is_chrome_installed() -> bool {
    // 系统级 /Applications 与用户级 ~/Applications 均可能
    if std::path::Path::new("/Applications/Google Chrome.app").exists() {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME")
        && std::path::Path::new(&format!(
            "{}/Applications/Google Chrome.app",
            home.to_string_lossy()
        ))
        .exists()
    {
        return true;
    }
    false
}

/// 检测系统是否安装了 Google Chrome（Linux）
///
/// 内部经 `which` 子进程探测（阻塞）；async 调用方不得直接调用，须经
/// [`is_channel_available_async`] / [`first_available_channel_async`] 间接执行。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn is_chrome_installed() -> bool {
    for bin in ["google-chrome", "google-chrome-stable"] {
        if std::process::Command::new("which")
            .arg(bin)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// 检测系统是否安装了 Microsoft Edge
#[cfg(target_os = "windows")]
pub fn is_edge_installed() -> bool {
    let candidates = [
        std::path::PathBuf::from(std::env::var("PROGRAMFILES(X86)").unwrap_or_default())
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
        std::path::PathBuf::from(std::env::var("PROGRAMFILES").unwrap_or_default())
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
    ];
    candidates.iter().any(|p| p.exists())
        || app_paths_executable("msedge.exe").is_some()
}

/// 检测系统是否安装了 Microsoft Edge（macOS）
#[cfg(target_os = "macos")]
pub fn is_edge_installed() -> bool {
    if std::path::Path::new("/Applications/Microsoft Edge.app").exists() {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME")
        && std::path::Path::new(&format!(
            "{}/Applications/Microsoft Edge.app",
            home.to_string_lossy()
        ))
        .exists()
    {
        return true;
    }
    false
}

/// 检测系统是否安装了 Microsoft Edge（Linux）
///
/// 内部经 `which` 子进程探测（阻塞）；async 调用方不得直接调用，须经
/// [`is_channel_available_async`] / [`first_available_channel_async`] 间接执行。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn is_edge_installed() -> bool {
    for bin in ["microsoft-edge", "microsoft-edge-stable"] {
        if std::process::Command::new("which")
            .arg(bin)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// 系统浏览器（Edge/Chrome）是否有任一可用
///
/// Playwright 可经 `channel` 直连系统浏览器，无需下载 Chromium 内核；
/// 环境引导据此决定是否跳过 Chromium 下载。
pub fn system_browser_available() -> bool {
    is_edge_installed() || is_chrome_installed()
}

/// 指定浏览器渠道当前是否可用
///
/// - `msedge` / `chrome`：系统安装探测（Windows 默认装 Edge，故默认渠道即走此路，
///   不必下载 Chromium，节省磁盘）
/// - `chromium` / `playwright`（历史别名）：Playwright 缓存探测（要求组件安装完整，
///   见 [`crate::environment::browser_registry`]）
/// - `firefox` / `webkit`：Playwright 缓存探测
/// - `custom`：自定义路径非空且存在
/// - 未知渠道：不可用
pub fn is_channel_available(
    env: &dyn crate::environment::EnvironmentApi,
    channel: &str,
    custom_path: &str,
) -> bool {
    is_channel_available_with(channel, custom_path, |engine| {
        env.browser_engine_ready(engine)
    })
}

/// 渠道可用性判定核心；`managed` 为托管引擎探测，注入以便单测脱离真实环境
fn is_channel_available_with(
    channel: &str,
    custom_path: &str,
    managed: impl Fn(&str) -> bool,
) -> bool {
    match channel.trim().to_ascii_lowercase().as_str() {
        "msedge" => is_edge_installed(),
        "chrome" => is_chrome_installed(),
        "custom" => {
            // 必须是可执行文件而非仅存在的路径：指向目录会通过健康检查、
            // 启动时才以晦涩的 Playwright 报错失败（H2）
            let p = custom_path.trim();
            !p.is_empty() && Path::new(p).is_file()
        }
        // 托管渠道的映射与自愈路径共用同一实现（environment::browser_registry）
        other => match crate::environment::browser_registry::managed_engine_of(other) {
            Some(engine) => managed(engine),
            None => false,
        },
    }
}

/// [`is_channel_available`] 的异步变体：探测整体移入阻塞线程池
///
/// 同步版在 Linux 上需 spawn `which` 子进程（Edge/Chrome 同口径），托管渠道
/// 判定含 `ms-playwright` 缓存目录扫描，均为阻塞 IO；Web 路由等同步调用方
/// 继续用同步版，async 调用方（登录渠道预检/终验，经 login 模块 60s TTL
/// 缓存）改走本变体，TTL 未命中时不再阻塞 tokio worker。`EnvironmentApi`
/// 已要求 Send + Sync，`Arc<dyn>` 可整体移入阻塞闭包独占持有。
pub async fn is_channel_available_async(
    env: std::sync::Arc<dyn crate::environment::EnvironmentApi>,
    channel: &str,
    custom_path: &str,
) -> bool {
    let channel = channel.to_string();
    let custom_path = custom_path.to_string();
    tokio::task::spawn_blocking(move || is_channel_available(env.as_ref(), &channel, &custom_path))
        .await
        // JoinError 仅探测任务 panic 可达（阻塞任务不会被动取消）：探测为纯
        // 查询，保守取「不可用」，让登录侧走渠道切换/失败兜底而非假阳性放行
        .unwrap_or(false)
}

/// 首个可用的浏览器渠道（优先级：Edge → Chrome → Chromium → Firefox → WebKit）
///
/// 系统浏览器优先：免下载、启动快（Windows 出厂即带 Edge，因此绝大多数用户
/// 无需下载 Chromium）；均无时返回 `None`，调用方走 Chromium 下载兜底或直接报
/// 无浏览器可用。
pub fn first_available_channel(
    env: &dyn crate::environment::EnvironmentApi,
) -> Option<&'static str> {
    first_available_channel_with(|engine| env.browser_engine_ready(engine))
}

/// 首个可用渠道的核心判定；`managed` 注入以便单测脱离真实环境
fn first_available_channel_with(managed: impl Fn(&str) -> bool) -> Option<&'static str> {
    first_available_from(system_browser_channel(), managed)
}

/// [`first_available_channel`] 的异步变体：同 [`is_channel_available_async`]
/// 口径，系统浏览器探测（Linux 为 `which` 子进程）与托管引擎缓存扫描一并
/// 移入阻塞线程池，供 async 调用方使用
pub async fn first_available_channel_async(
    env: std::sync::Arc<dyn crate::environment::EnvironmentApi>,
) -> Option<&'static str> {
    tokio::task::spawn_blocking(move || first_available_channel(env.as_ref()))
        .await
        // JoinError 仅探测任务 panic 可达：保守取「全无可用」，与探测本身
        // 返回 None 的语义一致（调用方据此走 Chromium 下载兜底或失败留痕）
        .unwrap_or(None)
}

/// 当前系统浏览器渠道（Edge 优先，其次 Chrome；均无返回 `None`）
fn system_browser_channel() -> Option<&'static str> {
    if is_edge_installed() {
        Some("msedge")
    } else if is_chrome_installed() {
        Some("chrome")
    } else {
        None
    }
}

/// 渠道优先级的纯函数形式：系统浏览器优先，其次 Chromium → Firefox → WebKit
///
/// 区分出来是为了让优先级顺序可被确定性单测覆盖（系统浏览器是否存在依赖机器）。
fn first_available_from(
    system: Option<&'static str>,
    managed: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    if let Some(channel) = system {
        return Some(channel);
    }
    // 托管引擎回退顺序：Chromium → Firefox → WebKit
    ["chromium", "firefox", "webkit"]
        .into_iter()
        .find(|engine| managed(engine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_channel_is_unavailable() {
        // 托管引擎探测恒真也不该让未知渠道变可用
        assert!(!is_channel_available_with("safari", "", |_| true));
        assert!(!is_channel_available_with("", "", |_| true));
        assert!(!is_channel_available_with("  ", "", |_| true));
    }

    #[test]
    fn test_custom_channel_requires_existing_path() {
        assert!(!is_channel_available_with("custom", "", |_| false));
        assert!(!is_channel_available_with("custom", "   ", |_| false));
        assert!(!is_channel_available_with(
            "custom",
            r"C:\definitely\not\here\browser.exe",
            |_| false
        ));
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("browser.exe");
        std::fs::write(&exe, b"x").unwrap();
        assert!(is_channel_available_with(
            "custom",
            &exe.to_string_lossy(),
            |_| false
        ));
    }

    #[test]
    fn test_channel_match_is_case_insensitive() {
        // 未知渠道大小写变化仍不可用（覆盖归一化分支）
        assert!(!is_channel_available_with("SAFARI", "", |_| true));
        // custom 大小写归一后走同一分支：空路径仍不可用
        assert!(!is_channel_available_with("Custom", "", |_| true));
    }

    /// 托管渠道的可用性由引擎判定驱动（ENV-1：不再由「目录非空」直接决定）
    #[test]
    fn managed_channel_follows_engine_availability() {
        let only_chromium = |engine: &str| engine == "chromium";
        assert!(is_channel_available_with("chromium", "", only_chromium));
        // 历史别名 playwright 与 chromium 同渠道
        assert!(is_channel_available_with("playwright", "", only_chromium));
        assert!(is_channel_available_with("PLAYWRIGHT", "", only_chromium));
        // 引擎不可用时对应渠道必须为假（旧实现的误报路径）
        assert!(!is_channel_available_with("chromium", "", |_| false));
        assert!(!is_channel_available_with("firefox", "", only_chromium));
        assert!(!is_channel_available_with("webkit", "", only_chromium));
    }

    /// 渠道优先级：系统浏览器优先，其次 Chromium → Firefox → WebKit
    #[test]
    fn first_available_from_priority() {
        let all = |_: &str| true;
        // 有系统浏览器时永远优先系统浏览器（Windows 默认装 Edge）
        assert_eq!(first_available_from(Some("msedge"), all), Some("msedge"));
        assert_eq!(first_available_from(Some("chrome"), all), Some("chrome"));
        // 无系统浏览器时按 managed 顺序回退
        assert_eq!(first_available_from(None, all), Some("chromium"));
        assert_eq!(
            first_available_from(None, |engine| engine == "firefox"),
            Some("firefox")
        );
        assert_eq!(
            first_available_from(None, |engine| engine == "webkit"),
            Some("webkit")
        );
        assert_eq!(first_available_from(None, |_| false), None);
    }

    #[test]
    fn test_no_browser_message_mentions_chromium() {
        assert!(NO_BROWSER_MESSAGE.contains("无可用浏览器"));
        assert!(NO_BROWSER_MESSAGE.contains("Chromium"));
    }

    /// P3-39：App Paths 默认值解析——剥 NUL/空白/包裹引号，空值拒绝
    ///（注册表读取本体仅 Windows 编译，解析纯逻辑全平台可单测）
    #[test]
    fn test_clean_app_paths_value() {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        assert_eq!(
            clean_app_paths_value(&wide(r"C:\Apps\Chrome\chrome.exe")),
            Some(r"C:\Apps\Chrome\chrome.exe".to_string())
        );
        // 部分安装器登记带引号的路径
        assert_eq!(
            clean_app_paths_value(&wide(r#""D:\My Apps\Edge\msedge.exe""#)),
            Some(r"D:\My Apps\Edge\msedge.exe".to_string())
        );
        // 值缺失（仅 NUL）或全空白：视为未登记
        assert_eq!(clean_app_paths_value(&[0]), None);
        assert_eq!(clean_app_paths_value(&wide("   ")), None);
    }
}
