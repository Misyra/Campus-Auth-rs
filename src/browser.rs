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
/// - `msedge` / `chrome`：系统安装探测
/// - `chromium` / `playwright`（历史别名）：Playwright 缓存探测
/// - `firefox` / `webkit`：Playwright 缓存探测
/// - `custom`：自定义路径非空且存在
/// - 未知渠道：不可用
pub fn is_channel_available(channel: &str, custom_path: &str) -> bool {
    match channel.trim().to_ascii_lowercase().as_str() {
        "msedge" => is_edge_installed(),
        "chrome" => is_chrome_installed(),
        "chromium" | "playwright" => {
            crate::environment::bootstrap::playwright_browser_installed("chromium")
        }
        "firefox" => crate::environment::bootstrap::playwright_browser_installed("firefox"),
        "webkit" => crate::environment::bootstrap::playwright_browser_installed("webkit"),
        "custom" => {
            let p = custom_path.trim();
            !p.is_empty() && Path::new(p).exists()
        }
        _ => false,
    }
}

/// 首个可用的浏览器渠道（优先级：Edge → Chrome → Chromium → Firefox → WebKit）
///
/// 系统浏览器优先：免下载、启动快；均无时返回 `None`，调用方走 Chromium
/// 下载兜底或直接报无浏览器可用。
pub fn first_available_channel() -> Option<&'static str> {
    if is_edge_installed() {
        return Some("msedge");
    }
    if is_chrome_installed() {
        return Some("chrome");
    }
    if crate::environment::bootstrap::playwright_browser_installed("chromium") {
        return Some("chromium");
    }
    if crate::environment::bootstrap::playwright_browser_installed("firefox") {
        return Some("firefox");
    }
    if crate::environment::bootstrap::playwright_browser_installed("webkit") {
        return Some("webkit");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_channel_is_unavailable() {
        assert!(!is_channel_available("safari", ""));
        assert!(!is_channel_available("", ""));
        assert!(!is_channel_available("  ", ""));
    }

    #[test]
    fn test_custom_channel_requires_existing_path() {
        assert!(!is_channel_available("custom", ""));
        assert!(!is_channel_available("custom", "   "));
        assert!(!is_channel_available(
            "custom",
            r"C:\definitely\not\here\browser.exe"
        ));
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("browser.exe");
        std::fs::write(&exe, b"x").unwrap();
        assert!(is_channel_available("custom", &exe.to_string_lossy()));
    }

    #[test]
    fn test_channel_match_is_case_insensitive() {
        // 未知渠道大小写变化仍不可用（覆盖归一化分支）
        assert!(!is_channel_available("SAFARI", ""));
        // custom 大小写归一后走同一分支：空路径仍不可用
        assert!(!is_channel_available("Custom", ""));
    }

    #[test]
    fn test_no_browser_message_mentions_chromium() {
        assert!(NO_BROWSER_MESSAGE.contains("无可用浏览器"));
        assert!(NO_BROWSER_MESSAGE.contains("Chromium"));
    }
}
