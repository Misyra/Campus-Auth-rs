from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected 1 match, got {count}: {old[:80]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    "await new Promise((resolve) => window.setTimeout(resolve, 1500));",
    "await new Promise<void>((resolve) => window.setTimeout(resolve, 1500));",
)

replace_once(
    "src/web/routes/system.rs",
    '''        if install_target != "chromium" {
            if let Err(e) = env.install_playwright_browser(&install_target).await {
                tracing::error!("Playwright {install_target} 安装失败: {e}");
            }
        }
''',
    '''        if let Err(e) = env.install_playwright_browser(&install_target).await {
            tracing::error!("Playwright {install_target} 安装失败: {e}");
        }
''',
)

print("playwright browser install finalizer applied")
