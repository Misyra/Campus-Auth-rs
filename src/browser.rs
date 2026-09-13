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
}

/// 检测系统是否安装了 Google Chrome（macOS）
#[cfg(target_os = "macos")]
pub fn is_chrome_installed() -> bool {
    // 系统级 /Applications 与用户级 ~/Applications 均可能
    if std::path::Path::new("/Applications/Google Chrome.app").exists() {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        if std::path::Path::new(&format!(
            "{}/Applications/Google Chrome.app",
            home.to_string_lossy()
        ))
        .exists()
        {
            return true;
        }
    }
    false
}

/// 检测系统是否安装了 Google Chrome（Linux）
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
}

/// 检测系统是否安装了 Microsoft Edge（macOS）
#[cfg(target_os = "macos")]
pub fn is_edge_installed() -> bool {
    if std::path::Path::new("/Applications/Microsoft Edge.app").exists() {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        if std::path::Path::new(&format!(
            "{}/Applications/Microsoft Edge.app",
            home.to_string_lossy()
        ))
        .exists()
        {
            return true;
        }
    }
    false
}

/// 检测系统是否安装了 Microsoft Edge（Linux）
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
}
