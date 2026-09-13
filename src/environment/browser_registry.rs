//! Playwright 浏览器 registry 读取与「是否安装完整」判定
//!
//! 事实源是 Playwright 包内的 `driver/package/browsers.json`——它给出各引擎的精确
//! revision。判定 = 「该引擎所需目录都存在且含 `INSTALLATION_COMPLETE` 标记」，
//! 而不是「`<prefix>*` 目录非空」（后者会把下载中断的半成品当成就绪）。
//!
//! 语义与 Python 侧 `playwright_worker.py` 的同名逻辑一致（两端各有等价测试）。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::environment::EnvironmentManager;

/// Playwright 在浏览器目录内写入的安装完成标记（其 registry 的 isInstalled 判据）
pub const INSTALLATION_COMPLETE_MARKER: &str = "INSTALLATION_COMPLETE";

/// venv 内 site-packages 候选目录（Windows / Unix 布局）——与 OCR 探测共用
pub fn site_packages_candidates(mgr: &EnvironmentManager) -> [PathBuf; 2] {
    let venv = mgr.worker_project_path().join(crate::environment::VENV_DIR);
    [
        venv.join("Lib").join("site-packages"),
        venv.join("lib").join("python3.12").join("site-packages"),
    ]
}

/// 渠道名 → 托管引擎；系统浏览器（msedge/chrome）、自定义与未知渠道返回 `None`
///
/// `playwright` 是 `chromium` 的历史别名（见 `config/migration.rs` 的渠道迁移）。
pub fn managed_engine_of(channel: &str) -> Option<&'static str> {
    match channel.trim().to_ascii_lowercase().as_str() {
        "chromium" | "playwright" => Some("chromium"),
        "firefox" => Some("firefox"),
        "webkit" => Some("webkit"),
        _ => None,
    }
}

/// 浏览器健康检查失败时该补装哪个引擎
///
/// 托管渠道补自身；系统浏览器（`msedge`/`chrome`）与自定义路径没有可补的 Playwright
/// 二进制，**兜底补 Chromium**——失败已说明当前浏览器起不来，且登录侧的渠道自动切换
/// 会把新装的 Chromium 纳入候选。正常运行时不会走到这里（引导是 Edge 优先、不下载）。
pub fn fallback_engine_for_channel(channel: &str) -> &'static str {
    managed_engine_of(channel).unwrap_or("chromium")
}

/// 引擎 → registry 条目名
///
/// chromium 需要**两套**：headless 与 headful 用不同二进制。应用自身的
/// `playwright install chromium` 两套都装（registry 中均为默认安装项），
/// 所以「两套都完整」= 安装完整，且与 `headless` 配置无关。
fn entry_names(engine: &str) -> &'static [&'static str] {
    match engine {
        "chromium" => &["chromium", "chromium-headless-shell"],
        "firefox" => &["firefox"],
        "webkit" => &["webkit"],
        _ => &[],
    }
}

/// registry 不可读时的回退前缀。注意 headless shell 目录名是**下划线**，
/// 而 `chromium-` 匹配不到它（这正是旧实现漏判 headless shell 的原因）。
fn fallback_prefixes(engine: &str) -> &'static [&'static str] {
    match engine {
        "chromium" => &["chromium-", "chromium_headless_shell-"],
        "firefox" => &["firefox-"],
        "webkit" => &["webkit-"],
        _ => &[],
    }
}

/// Playwright 浏览器缓存根目录（`PLAYWRIGHT_BROWSERS_PATH` 优先，空 / `"0"` 走 OS 默认）
pub fn browser_cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        if !dir.is_empty() && dir != "0" {
            return Some(PathBuf::from(dir));
        }
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"));
    #[cfg(target_os = "linux")]
    let base = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"));
    base.map(|dir| dir.join("ms-playwright"))
}

/// registry 名 → 磁盘目录名：`chromium-headless-shell` → `chromium_headless_shell-1234`
fn dir_name(registry_name: &str, revision: &str) -> String {
    format!("{}-{}", registry_name.replace('-', "_"), revision)
}

/// 目录是否是「安装完成」的浏览器目录
fn is_complete(dir: &Path) -> bool {
    dir.join(INSTALLATION_COMPLETE_MARKER).is_file()
}

/// 期望目录中哪些缺失或未完成（空 = 已安装完整）
fn missing_in(cache: &Path, expected: &[String]) -> Vec<String> {
    expected
        .iter()
        .filter(|name| !is_complete(&cache.join(name.as_str())))
        .cloned()
        .collect()
}

#[derive(Debug, Deserialize)]
struct Registry {
    browsers: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    name: String,
    revision: String,
    /// 平台差异化修订（如 webkit 在 mac14 / ubuntu20.04 上 revision 不同）
    /// → 无法可靠推导，交回退处理
    #[serde(default, rename = "revisionOverrides")]
    revision_overrides: Option<serde_json::Value>,
}

/// venv 内 registry 路径
fn registry_path(mgr: &EnvironmentManager) -> Option<PathBuf> {
    let rel = Path::new("playwright")
        .join("driver")
        .join("package")
        .join("browsers.json");
    site_packages_candidates(mgr)
        .into_iter()
        .map(|site| site.join(&rel))
        .find(|path| path.is_file())
}

/// 从 registry 推导期望目录名；不可读或带平台差异化修订时返回 `None`（走回退）
fn expected_dir_names(mgr: &EnvironmentManager, engine: &str) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(registry_path(mgr)?).ok()?;
    let registry: Registry = match serde_json::from_str(&raw) {
        Ok(registry) => registry,
        Err(error) => {
            tracing::warn!("Playwright registry 解析失败，回退前缀判定: {error}");
            return None;
        }
    };
    entry_names(engine)
        .iter()
        .map(|name| {
            let entry = registry.browsers.iter().find(|entry| entry.name == *name)?;
            if entry.revision_overrides.is_some() {
                return None;
            }
            Some(dir_name(&entry.name, &entry.revision))
        })
        .collect()
}

/// 回退判定：任一匹配前缀的目录安装完成即视为可用
fn fallback_missing(cache: &Path, engine: &str) -> Vec<String> {
    let prefixes = fallback_prefixes(engine);
    let placeholder = || {
        prefixes
            .iter()
            .map(|prefix| format!("{prefix}*"))
            .collect::<Vec<_>>()
    };
    let Ok(entries) = std::fs::read_dir(cache) else {
        return placeholder();
    };
    let installed = entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        prefixes.iter().any(|prefix| name.starts_with(*prefix)) && is_complete(&entry.path())
    });
    if installed { Vec::new() } else { placeholder() }
}

/// 指定托管引擎缺失 / 未完成的组件目录名；**空 = 安装完整**
///
/// 供引导判定与失败诊断共用：缺失项非空时可直接作为「缺什么」展示。
pub fn missing_components(mgr: &EnvironmentManager, engine: &str) -> Vec<String> {
    if entry_names(engine).is_empty() {
        return vec![format!("未知引擎 {engine}")];
    }
    let Some(cache) = browser_cache_dir() else {
        return vec!["未找到 Playwright 浏览器缓存目录".to_string()];
    };
    match expected_dir_names(mgr, engine) {
        Some(expected) => missing_in(&cache, &expected),
        None => fallback_missing(&cache, engine),
    }
}

/// 缺失项的描述文案（空列表返回空串）
pub fn describe_missing(missing: &[String]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    format!("缺少浏览器组件: {}", missing.join("、"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_browser_dir(cache: &Path, name: &str, complete: bool) {
        let dir = cache.join(name);
        std::fs::create_dir_all(dir.join("payload")).expect("创建目录");
        if complete {
            std::fs::write(dir.join(INSTALLATION_COMPLETE_MARKER), b"").expect("写完成标记");
        }
    }

    #[test]
    fn channel_maps_to_managed_engine() {
        assert_eq!(managed_engine_of("chromium"), Some("chromium"));
        assert_eq!(managed_engine_of("playwright"), Some("chromium"));
        assert_eq!(managed_engine_of("  PLAYWRIGHT "), Some("chromium"));
        assert_eq!(managed_engine_of("firefox"), Some("firefox"));
        assert_eq!(managed_engine_of("webkit"), Some("webkit"));
        // 系统浏览器 / 自定义 / 未知渠道不属托管
        for channel in ["msedge", "chrome", "custom", "safari", ""] {
            assert_eq!(managed_engine_of(channel), None, "{channel}");
        }
    }

    /// 失败自愈要装的引擎：托管渠道装自身，系统浏览器/自定义兜底装 Chromium
    #[test]
    fn fallback_engine_defaults_to_chromium() {
        assert_eq!(fallback_engine_for_channel("chromium"), "chromium");
        assert_eq!(fallback_engine_for_channel("playwright"), "chromium");
        assert_eq!(fallback_engine_for_channel("firefox"), "firefox");
        assert_eq!(fallback_engine_for_channel("webkit"), "webkit");
        for channel in ["msedge", "chrome", "custom", "safari", ""] {
            assert_eq!(
                fallback_engine_for_channel(channel),
                "chromium",
                "{channel}"
            );
        }
    }

    #[test]
    fn dir_name_maps_hyphen_to_underscore() {
        assert_eq!(
            dir_name("chromium-headless-shell", "1234"),
            "chromium_headless_shell-1234"
        );
        assert_eq!(dir_name("chromium", "1234"), "chromium-1234");
    }

    #[test]
    fn chromium_needs_both_headful_and_headless_shell() {
        assert_eq!(
            entry_names("chromium"),
            ["chromium", "chromium-headless-shell"]
        );
        let expected = vec![
            "chromium-1234".to_string(),
            "chromium_headless_shell-1234".to_string(),
        ];
        let cache = tempfile::tempdir().expect("临时目录");
        write_browser_dir(cache.path(), "chromium-1234", true);
        // 只装 headful → 缺 headless shell
        assert_eq!(
            missing_in(cache.path(), &expected),
            ["chromium_headless_shell-1234"]
        );
    }

    /// 核心回归：目录非空但缺完成标记必须算「缺失」（旧实现据此误报就绪）
    #[test]
    fn non_empty_dir_without_marker_is_missing() {
        let cache = tempfile::tempdir().expect("临时目录");
        write_browser_dir(cache.path(), "chromium-1234", false);
        let expected = vec!["chromium-1234".to_string()];
        assert_eq!(missing_in(cache.path(), &expected), ["chromium-1234"]);
    }

    #[test]
    fn marker_makes_component_complete() {
        let cache = tempfile::tempdir().expect("临时目录");
        write_browser_dir(cache.path(), "chromium-1234", true);
        let expected = vec!["chromium-1234".to_string()];
        assert!(missing_in(cache.path(), &expected).is_empty());
    }

    #[test]
    fn fallback_uses_prefix_and_marker() {
        let cache = tempfile::tempdir().expect("临时目录");
        // 空目录 → 给出期望前缀
        assert_eq!(
            fallback_missing(cache.path(), "chromium"),
            ["chromium-*", "chromium_headless_shell-*"]
        );
        // 非空但无标记 → 仍算缺失
        write_browser_dir(cache.path(), "chromium-1234", false);
        assert_eq!(fallback_missing(cache.path(), "chromium").len(), 2);
        // 出现任一安装完成的匹配目录 → 视为可用
        write_browser_dir(cache.path(), "chromium_headless_shell-1243", true);
        assert!(fallback_missing(cache.path(), "chromium").is_empty());
        // firefox 无匹配目录
        assert_eq!(fallback_missing(cache.path(), "firefox"), ["firefox-*"]);
    }
}
