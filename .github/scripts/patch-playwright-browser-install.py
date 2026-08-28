from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected 1 match, got {count}: {old[:80]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# 1) 泛化 Playwright installer，核心 bootstrap 仍保留 Chromium wrapper。
replace_once(
    "src/environment/python.rs",
    '''/// 安装 Playwright Chromium 浏览器
///
/// 执行 `uv run playwright install chromium`，带超时和重试。
pub async fn install_playwright(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let uv_exe = uv_exe_path(mgr);
''',
    '''/// 安装核心 Playwright Chromium 浏览器。
///
/// 核心引导继续只安装 Chromium；Firefox/WebKit 由设置页显式按需安装。
pub async fn install_playwright(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    install_playwright_browser(mgr, "chromium").await
}

/// 安装指定的 Playwright 管理浏览器。
///
/// 仅允许 Chromium / Firefox / WebKit，执行 `uv run playwright install <browser>`，
/// 带统一超时和重试。调用方负责通过 BootstrapGate 串行化显式安装。
pub async fn install_playwright_browser(
    mgr: &EnvironmentManager,
    browser: &str,
) -> Result<(), EnvironmentError> {
    if !matches!(browser, "chromium" | "firefox" | "webkit") {
        return Err(EnvironmentError::UnsupportedPlaywrightBrowser {
            browser: browser.to_string(),
        });
    }

    let uv_exe = uv_exe_path(mgr);
''',
)
replace_once(
    "src/environment/python.rs",
    '''            let msg = format!(
                "重试安装浏览器 ({}/{})...",
                attempt, PLAYWRIGHT_INSTALL_MAX_RETRIES
            );
''',
    '''            let msg = format!(
                "重试安装 {browser} ({}/{})...",
                attempt, PLAYWRIGHT_INSTALL_MAX_RETRIES
            );
''',
)
replace_once(
    "src/environment/python.rs",
    '''                "playwright",
                "install",
                "chromium",
''',
    '''                "playwright",
                "install",
                browser,
''',
)
replace_once(
    "src/environment/python.rs",
    '''                tracing::info!("Playwright Chromium 安装成功");
''',
    '''                tracing::info!("Playwright {browser} 安装成功");
''',
)
replace_once(
    "src/environment/python.rs",
    '''    #[test]
    fn test_ocr_declared_requires_ocr_optional_extra() {
''',
    '''    #[tokio::test]
    async fn test_install_playwright_browser_rejects_unknown_before_io() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
            false,
        );
        let result = install_playwright_browser(&mgr, "chrome").await;
        assert!(matches!(
            result,
            Err(EnvironmentError::UnsupportedPlaywrightBrowser { browser }) if browser == "chrome"
        ));
    }

    #[test]
    fn test_ocr_declared_requires_ocr_optional_extra() {
''',
)

# 2) EnvironmentApi 暴露显式浏览器安装，并与 OCR/bootstrap 共用 BootstrapGate。
replace_once(
    "src/environment/mod.rs",
    '''pub use python::{ensure_venv, install_playwright};
''',
    '''pub use python::{ensure_venv, install_playwright, install_playwright_browser};
''',
)
replace_once(
    "src/environment/mod.rs",
    '''/// playwright install chromium 超时
pub const PLAYWRIGHT_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
''',
    '''/// playwright install 浏览器超时
pub const PLAYWRIGHT_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
''',
)
replace_once(
    "src/environment/mod.rs",
    '''    /// Playwright 安装超时
    #[error("Playwright 安装超时 (>{timeout_secs}s)")]
    PlaywrightInstallTimeout { timeout_secs: u64 },

    /// .venv 损坏，需要重建
''',
    '''    /// Playwright 安装超时
    #[error("Playwright 安装超时 (>{timeout_secs}s)")]
    PlaywrightInstallTimeout { timeout_secs: u64 },

    /// 请求安装了不受支持的 Playwright 浏览器
    #[error("不支持的 Playwright 浏览器: {browser}")]
    UnsupportedPlaywrightBrowser { browser: String },

    /// .venv 损坏，需要重建
''',
)
replace_once(
    "src/environment/mod.rs",
    '''/// 引导互斥门（F1）：串行化并发的 `ensure_capability` / `retry_install`
''',
    '''/// 引导互斥门（F1）：串行化并发的 `ensure_capability` / `retry_install` /
/// OCR 依赖同步 / 显式 Playwright 浏览器安装
''',
)
replace_once(
    "src/environment/mod.rs",
    '''    /// 确保浏览器自动化能力就绪；若未就绪则触发引导。
    async fn ensure_capability(&self) -> Result<(), EnvironmentError>;
    /// 安装 OCR optional extra，并持久记录用户启用偏好。
''',
    '''    /// 确保浏览器自动化能力就绪；若未就绪则触发引导。
    async fn ensure_capability(&self) -> Result<(), EnvironmentError>;
    /// 显式安装指定 Playwright 管理浏览器（chromium/firefox/webkit）。
    async fn install_playwright_browser(&self, browser: &str) -> Result<(), EnvironmentError>;
    /// 安装 OCR optional extra，并持久记录用户启用偏好。
''',
)
replace_once(
    "src/environment/mod.rs",
    '''    async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
        EnvironmentManager::ensure_capability(self).await
    }

    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
''',
    '''    async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
        EnvironmentManager::ensure_capability(self).await
    }

    async fn install_playwright_browser(&self, browser: &str) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(crate::environment::python::install_playwright_browser(
                self, browser,
            ))
            .await
    }

    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
''',
)

# 3) OCR 路由测试 mock 适配新增 trait 方法。
replace_once(
    "src/web/routes/ocr.rs",
    '''        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
''',
    '''        async fn ensure_capability(&self) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_playwright_browser(&self, _browser: &str) -> Result<(), EnvironmentError> {
            Ok(())
        }
        async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
''',
)

# 4) Web API：保留旧路径，不带 query 默认 Chromium；非法 channel 400。
replace_once(
    "src/web/routes/system.rs",
    '''use serde_json::Value;
''',
    '''use serde::Deserialize;
use serde_json::Value;
''',
)
replace_once(
    "src/web/routes/system.rs",
    '''/// POST /api/install/playwright — 安装 Playwright Chromium
///
/// 触发环境管理器安装 Playwright 浏览器（异步执行，进度通过 StatusManager 推送）。
pub async fn install_playwright(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let env = state.environment.clone();
    // 后台执行安装，避免阻塞响应；进度通过 StatusManager 推送
    tokio::spawn(async move {
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("Playwright 安装失败: {e}");
        }
    });
    Ok(data(serde_json::json!({
        "message": "Playwright 安装已启动，进度请通过状态推送查看",
    })))
}
''',
    '''#[derive(Debug, Default, Deserialize)]
pub struct InstallPlaywrightQuery {
    browser: Option<String>,
}

fn normalize_playwright_browser(browser: Option<&str>) -> Result<String, ApiError> {
    let browser = browser.unwrap_or("chromium").trim().to_ascii_lowercase();
    if matches!(browser.as_str(), "chromium" | "firefox" | "webkit") {
        Ok(browser)
    } else {
        Err(ApiError::BadRequest(format!(
            "不支持安装浏览器 {browser:?}，仅支持 chromium/firefox/webkit"
        )))
    }
}

/// POST /api/install/playwright — 安装 Playwright 管理浏览器
///
/// `?browser=chromium|firefox|webkit`；省略参数保持旧行为，默认 Chromium。
/// 后台先确保核心 Chromium 环境就绪，再按需安装 Firefox/WebKit。
pub async fn install_playwright(
    State(environment): State<Arc<dyn crate::environment::EnvironmentApi>>,
    Query(params): Query<InstallPlaywrightQuery>,
) -> Result<Json<Value>, ApiError> {
    let browser = normalize_playwright_browser(params.browser.as_deref())?;
    let env = environment.clone();
    let install_target = browser.clone();
    tokio::spawn(async move {
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("Playwright 核心环境安装失败: {e}");
            return;
        }
        if install_target != "chromium" {
            if let Err(e) = env.install_playwright_browser(&install_target).await {
                tracing::error!("Playwright {install_target} 安装失败: {e}");
            }
        }
    });
    Ok(data(serde_json::json!({
        "browser": browser,
        "message": "Playwright 浏览器安装已启动，请等待浏览器列表更新",
    })))
}
''',
)
replace_once(
    "src/web/routes/system.rs",
    '''    // ============ tracing JSON 日志解析 ============
''',
    '''    #[test]
    fn normalize_playwright_browser_defaults_and_validates() {
        assert_eq!(normalize_playwright_browser(None).unwrap(), "chromium");
        assert_eq!(
            normalize_playwright_browser(Some(" Firefox ")).unwrap(),
            "firefox"
        );
        assert_eq!(
            normalize_playwright_browser(Some("WEBKIT")).unwrap(),
            "webkit"
        );
        assert!(matches!(
            normalize_playwright_browser(Some("chrome")),
            Err(ApiError::BadRequest(_))
        ));
    }

    // ============ tracing JSON 日志解析 ============
''',
)

# 5) 前端 API 支持指定 Playwright 引擎。
replace_once(
    "frontend/src/api/index.ts",
    '''  installPlaywright: (opts?: { signal?: AbortSignal; timeout?: number }) =>
    http.post<MutationResult>("/api/install/playwright", null, opts),
''',
    '''  installPlaywright: (browser = "chromium", opts?: { signal?: AbortSignal; timeout?: number }) =>
    http.post<MutationResult & { browser?: string }>(
      `/api/install/playwright?browser=${encodeURIComponent(browser)}`,
      null,
      opts,
    ),
''',
)

# 6) 设置页：三个 Playwright 引擎都可按需安装，轮询真实 installed 状态。
replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    '''const playwrightDownloading = ref(false);
const stoppingBrowser = ref(false);
''',
    '''const installingBrowser = ref<string | null>(null);
const browserInstallError = ref("");
const stoppingBrowser = ref(false);
const playwrightInstallable = new Set(["chromium", "firefox", "webkit"]);
''',
)
replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    '''function handleBrowserClick(b: typeof browsers.value[0]) {
  if (!b.installed && b.channel !== "chromium" && b.channel !== "custom") return;
  if (b.channel === "chromium" && !b.installed) {
    void installPlaywright();
    return;
  }
  config.config.browser.browser_channel = b.channel;
}

async function installPlaywright() {
  playwrightDownloading.value = true;
  try { await browsersApi.installPlaywright(); } catch { /* */ }
  playwrightDownloading.value = false;
}
''',
    '''function handleBrowserClick(b: typeof browsers.value[0]) {
  if (!b.installed) {
    if (playwrightInstallable.has(b.channel)) void installPlaywright(b.channel);
    return;
  }
  config.config.browser.browser_channel = b.channel;
}

async function installPlaywright(channel: string) {
  if (installingBrowser.value) return;
  installingBrowser.value = channel;
  browserInstallError.value = "";
  try {
    await browsersApi.installPlaywright(channel);
    const deadline = Date.now() + 10 * 60 * 1000;
    while (Date.now() < deadline) {
      await new Promise((resolve) => window.setTimeout(resolve, 1500));
      const data = await browsersApi.fetch();
      browsers.value = data.browsers;
      if (browsers.value.find((b) => b.channel === channel)?.installed) {
        config.config.browser.browser_channel = channel;
        return;
      }
    }
    browserInstallError.value = `${channel} 安装超时，请检查日志后重试`;
  } catch {
    browserInstallError.value = `${channel} 安装失败，请检查日志后重试`;
  } finally {
    installingBrowser.value = null;
  }
}
''',
)
replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    ''':class="{ active: config.config.browser.browser_channel === b.channel, disabled: !b.installed }"
''',
    ''':class="{ active: config.config.browser.browser_channel === b.channel, disabled: !b.installed && !playwrightInstallable.has(b.channel) }"
''',
)
replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    '''                  <span v-else-if="playwrightDownloading && b.channel === 'chromium'" class="status-downloading">
                    <IconApp name="refresh" width="14" height="14" class="spin" /> 下载中...
                  </span>
                  <span v-else class="status-not-installed">
                    <IconApp name="upload" width="14" height="14" /> 未安装
                  </span>
''',
    '''                  <span v-else-if="installingBrowser === b.channel" class="status-downloading">
                    <IconApp name="refresh" width="14" height="14" class="spin" /> 下载中...
                  </span>
                  <span v-else-if="playwrightInstallable.has(b.channel)" class="status-not-installed">
                    <IconApp name="upload" width="14" height="14" /> 点击安装
                  </span>
                  <span v-else class="status-not-installed">
                    <IconApp name="upload" width="14" height="14" /> 未安装
                  </span>
''',
)
replace_once(
    "frontend/src/views/settings/BrowserSettings.vue",
    '''        </div>
      </div>
    </section>

    <!-- 基本设置 -->
''',
    '''        </div>
        <p v-if="browserInstallError" class="form-help-text">{{ browserInstallError }}</p>
      </div>
    </section>

    <!-- 基本设置 -->
''',
)

print("playwright browser install patch applied")
