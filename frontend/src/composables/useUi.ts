/**
 * 全局 UI 与生命周期编排（单例）。
 * 替代原 uiData + uiMethods + actionsMethods（部分）+ lifecycleMethods.
 * 协调各 composable 完成初始化、浏览器管理、更新检查、协议向导、退出等。
 */

import { reactive } from "vue";
import type {
  BrowserInfo,
  UpdateInfo,
  InitStatus,
  NetworkInterface,
  LoginHistoryItem,
} from "../api/types";
import { systemApi, browsersApi, monitorApi } from "../api";
import { extractApiError } from "../api/client";
import { LIMITS, TIMING } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { useConfig } from "./useConfig";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useProfiles } from "./useProfiles";
import { useTasks } from "./useTasks";
import { useScripts } from "./useScripts";
import { useScheduledTasks } from "./useScheduledTasks";
import { useAppearance } from "./useAppearance";
import { useWebSocket } from "./useWebSocket";
import { useToast } from "./useToast";
import { useNotifications } from "./useNotifications";
import { useConfirm } from "./useConfirm";

const state = reactive({
  isLoading: true,
  showWizard: false,
  agreedToTerms: false,
  updateInfo: null as UpdateInfo | null,
  updateLoading: false,
  appVersion: "unknown",
  pythonVersion: "",
  availableBrowsers: [] as BrowserInfo[],
  selectedBrowser: "playwright",
  browserLoading: false,
  playwrightDownloading: false,
  fullscreenSrc: "",
  networkInterfaces: [] as NetworkInterface[],
});

const loginHistory = reactive<LoginHistoryItem[]>([]);
const { busy } = useStatus();
const { fetchLogs } = useLogs();
const { toastOnly } = useToast();
const { notify } = useNotifications();
const { confirm } = useConfirm();

let timers: ReturnType<typeof setInterval>[] = [];
let initErrorCount = 0;

function recordInitError(msg: string): void {
  if (initErrorCount < 2) {
    initErrorCount++;
    notify(false, msg);
  }
}

async function fetchAppVersion(): Promise<void> {
  try {
    const data = await systemApi.health();
    if (data?.version) {
      state.appVersion = data.version;
      if (data.python_version) state.pythonVersion = data.python_version;
      return;
    }
  } catch {
    /* 尝试 openapi 回退 */
  }
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), TIMING.OPENAPI_TIMEOUT);
    let resp: Response;
    try {
      resp = await fetch("/openapi.json", { cache: "no-cache", signal: controller.signal });
    } finally {
      clearTimeout(timeoutId);
    }
    if (resp.ok) {
      const schema = await resp.json();
      state.appVersion = schema?.info?.version || "unknown";
      return;
    }
  } catch {
    /* 回退失败 */
  }
  state.appVersion = "unknown";
}

async function fetchLoginHistory(): Promise<void> {
  try {
    const data = await import("../api").then((m) => m.historyApi.fetch(30));
    if (Array.isArray(data)) {
      loginHistory.splice(0, loginHistory.length, ...data);
    }
  } catch (error) {
    frontendLogger.error("history", "获取登录历史失败", error);
  }
}

async function clearLoginHistory(): Promise<void> {
  if (!loginHistory.length) return;
  const ok = await confirm({ title: "清空登录历史", message: `确定要清空所有 ${loginHistory.length} 条登录记录吗？此操作不可撤销。`, danger: true });
  if (!ok) return;
  try {
    const historyApi = (await import("../api")).historyApi;
    const data = await historyApi.clear();
    loginHistory.splice(0, loginHistory.length);
    toastOnly(true, extractMsg(data, "清空完成"));
  } catch (error) {
    toastOnly(false, extractApiError(error, "清空登录历史失败"));
  }
}

async function checkInitStatus(): Promise<void> {
  try {
    const data: InitStatus = await systemApi.initStatus();
    state.showWizard = !data.agreed;
    if (data.password_decryption_failed) {
      frontendLogger.error("init", "密码解密失败，请在设置页面重新输入密码");
      notify(false, "密码解密失败，请在设置页面重新输入密码", "security");
    }
  } catch (error) {
    const anyErr = error as { response?: { status?: number } };
    if (anyErr?.response?.status) state.showWizard = false;
    frontendLogger.warn("init", "检查初始化状态失败", error);
  }
}

async function finishWizard(): Promise<void> {
  busy.save = true;
  try {
    await systemApi.agree();
    state.showWizard = false;
    state.agreedToTerms = false;
    frontendLogger.info("lifecycle", "已同意协议");
  } catch (error) {
    toastOnly(false, extractApiError(error, "操作失败"));
  } finally {
    busy.save = false;
  }
}

async function checkUpdate(): Promise<void> {
  state.updateLoading = true;
  state.updateInfo = null;
  try {
    const data = await systemApi.checkUpdate();
    state.updateInfo = data;
  } catch {
    state.updateInfo = { has_update: false, error: "检查更新失败，请检查网络连接" };
  } finally {
    state.updateLoading = false;
  }
}

async function autoCheckUpdateOnStartup(): Promise<void> {
  try {
    const data = await systemApi.checkUpdate();
    state.updateInfo = data;
    if (!data?.has_update) return;
    const latest = data.latest ? `v${data.latest}` : "新版本";
    const current = data.current ? `（当前 v${data.current}）` : "";
    const message = `发现新版本 ${latest}${current}`;
    notify(true, message, "update", { label: "前往下载", page: "about" });
    frontendLogger.warn("update", `${message}，请前往“关于”页面下载`);
  } catch (error) {
    frontendLogger.debug("update", "启动自动检查更新失败", error);
  }
}

async function fetchBrowsers(): Promise<void> {
  state.browserLoading = true;
  try {
    const data = await browsersApi.fetch();
    state.availableBrowsers = data.browsers;
    if (state.selectedBrowser) {
      state.selectedBrowser = data.current;
      const { config } = useConfig();
      config.browser.browser_channel = data.current;
    }
  } catch (error) {
    frontendLogger.error("browser", "获取浏览器列表失败", error);
  } finally {
    state.browserLoading = false;
  }
}

function selectBrowser(channel: string): void {
  state.selectedBrowser = channel;
  const { config } = useConfig();
  config.browser.browser_channel = channel;
}

function getActiveBrowserChannel(): string {
  return state.selectedBrowser || useConfig().config.browser.browser_channel;
}

function handleBrowserClick(browser: BrowserInfo): void {
  if (browser.installed) {
    if (browser.channel === "firefox") {
      void confirm({
        title: "Firefox 兼容性警告",
        message:
          "Firefox 可能不支持部分功能（如反检测模式、自定义浏览器参数等）。\n\n建议使用 Chromium 内核浏览器（Playwright Chromium、Edge、Chrome）。\n\n确定要使用 Firefox 吗？",
      }).then((ok) => {
        if (ok) selectBrowser(browser.channel);
      });
    } else {
      selectBrowser(browser.channel);
    }
  } else if (browser.channel === "custom") {
    selectBrowser(browser.channel);
  } else if (browser.channel === "playwright") {
    void confirm({
      title: "下载 Playwright Chromium",
      message: "Playwright Chromium 未安装。\n\n是否自动下载？（约 150MB）",
    }).then((ok) => {
      if (ok) installPlaywrightChromium();
    });
  } else {
    const downloadUrls: Record<string, string> = {
      msedge: "https://www.microsoft.com/edge",
      chrome: "https://www.google.com/chrome/",
      firefox: "https://www.firefox.com/",
    };
    const url = downloadUrls[browser.channel] || "https://playwright.dev/docs/browsers";
    void confirm({ title: "浏览器未安装", message: `${browser.name} 未安装。\n\n是否跳转到官网下载？` }).then(
      (ok) => {
        if (ok) window.open(url, "_blank");
      },
    );
  }
}

function installPlaywrightChromium(): void {
  state.playwrightDownloading = true;
  notify(true, "Playwright Chromium 下载已开始，你可以继续配置其他选项", "install");
  frontendLogger.info("browser", "开始下载 Playwright Chromium");
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 600000);
  browsersApi
    .installPlaywright({ signal: controller.signal, timeout: 600000 })
    .then((data) => {
      frontendLogger.info("browser", "Playwright Chromium 安装成功");
      notify(true, extractMsg(data, "Playwright Chromium 安装完成！"), "install");
      void fetchBrowsers();
    })
    .catch((error) => {
      const err = error as { name?: string };
      if (err.name === "AbortError" || err.name === "CanceledError") {
        frontendLogger.error("browser", "安装超时（超过 10 分钟）");
        notify(false, "安装超时，请检查网络后重试", "install");
      } else {
        frontendLogger.error("browser", "安装请求失败", error);
        notify(false, "安装请求失败，请查看日志", "install");
      }
    })
    .finally(() => {
      clearTimeout(timeoutId);
      state.playwrightDownloading = false;
    });
}

async function fetchNetworkInterfaces(): Promise<void> {
  try {
    const data = await monitorApi.fetchInterfaces();
    state.networkInterfaces = data;
  } catch (error) {
    frontendLogger.error("network", "获取网络接口失败", error);
  }
}

async function toggleMonitor(): Promise<void> {
  busy.monitor = true;
  const { status } = useStatus();
  try {
    frontendLogger.info("monitor", `${status.monitoring ? "stop" : "start"} monitor`);
    const monitorApi = (await import("../api")).monitorApi;
    const data = status.monitoring ? await monitorApi.stop() : await monitorApi.start();
    toastOnly(true, extractMsg(data, "操作成功"));
    await useStatus().fetchStatus();
  } catch (error) {
    const msg = extractApiError(error, "操作失败");
    frontendLogger.error("monitor", "切换监控失败", msg);
    notify(false, msg, "monitor");
  } finally {
    busy.monitor = false;
  }
}

async function manualLogin(): Promise<void> {
  if (busy.loginCooldown) return;
  busy.action = true;
  busy.login = true;
  try {
    const actionsApi = (await import("../api")).actionsApi;
    const loginTimeoutMs = (useConfig().config.browser.login_timeout || 90) * 1000;
    const data = await actionsApi.login(loginTimeoutMs);
    notify(true, stripScreenshotHint(extractMsg(data, "登录完成")), "login");
    await fetchLoginHistory();
  } catch (error) {
    const msg = extractApiError(error, "手动登录失败");
    frontendLogger.error("action", "手动登录失败", msg);
    notify(false, stripScreenshotHint(msg), "login");
  } finally {
    busy.login = false;
    busy.loginCooldown = true;
    setTimeout(() => {
      busy.loginCooldown = false;
    }, 3000);
    busy.action = false;
  }
}

async function cancelLogin(): Promise<void> {
  try {
    const actionsApi = (await import("../api")).actionsApi;
    const data = await actionsApi.cancelLogin();
    toastOnly(true, data?.message || "已取消");
  } catch (error) {
    toastOnly(false, extractApiError(error, "取消登录失败"));
  }
}

async function testNetwork(): Promise<void> {
  busy.action = true;
  try {
    const actionsApi = (await import("../api")).actionsApi;
    const data = await actionsApi.testNetwork();
    toastOnly(true, extractMsg(data, "测试完成"));
  } catch (error) {
    toastOnly(false, extractApiError(error, "网络测试失败"));
  } finally {
    busy.action = false;
  }
}

function openFullscreen(src: string): void {
  state.fullscreenSrc = src;
}
function closeFullscreen(): void {
  state.fullscreenSrc = "";
}

async function quitApp(): Promise<void> {
  const ok = await confirm({ title: "退出应用", message: "确定要退出应用吗？", danger: true });
  if (!ok) return;
  try {
    busy.monitor = true;
    useWebSocket().destroy();
    await systemApi.shutdown();
  } catch (error) {
    frontendLogger.error("app", "退出应用失败", error);
  } finally {
    busy.monitor = false;
    showExitOverlay();
  }
}

function showExitOverlay(): void {
  const overlay = document.createElement("div");
  overlay.className = "exit-overlay";
  overlay.innerHTML =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="48" height="48"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg><h2>已安全退出</h2><p>后端服务已关闭</p>';
  const btn = document.createElement("button");
  btn.className = "btn btn-primary";
  btn.textContent = "关闭页面";
  btn.addEventListener("click", () => window.close());
  overlay.append(btn);
  document.body.appendChild(overlay);
}

/** 从 API 响应提取消息：后端可能返回字符串或 {message?: string} 对象 */
function extractMsg(data: unknown, fallback: string): string {
  if (typeof data === "string") return data;
  if (data && typeof data === "object" && "message" in data) {
    const msg = (data as { message?: string }).message;
    if (msg) return msg;
  }
  return fallback;
}

function stripScreenshotHint(message: string): string {
  const text = String(message || "");
  return text
    .replace(/\s*[\[(]?\s*截图[:：]\s*\/(?:logs|debug|temp)\/\S+\.(?:png|jpg|jpeg|webp|gif)\s*[\])]?/gi, "")
    .trim();
}

async function init(): Promise<void> {
  frontendLogger.info("app.init", "开始初始化");
  state.isLoading = true;
  initErrorCount = 0;

  const config = useConfig();
  const status = useStatus();
  const tasks = useTasks();
  const scripts = useScripts();
  const profiles = useProfiles();
  const scheduled = useScheduledTasks();
  const appearance = useAppearance();

  const results = await Promise.allSettled([
    config.fetchConfig(),
    status.fetchStatus(),
    fetchLogs(),
    fetchAppVersion(),
    status.fetchAutostart(),
    checkInitStatus(),
    tasks.fetchTasks(),
    scripts.fetchScripts(),
    tasks.fetchActiveTask(),
    profiles.fetchProfiles(),
    tasks.fetchPureMode(),
    fetchLoginHistory(),
    scheduled.loadScheduledTasks(),
    config.fetchShells(),
    config.fetchOcrStatus(),
    config.fetchLogLevels(),
    fetchBrowsers(),
  ]);
  const rejected = results.filter((r) => r.status === "rejected").length;
  if (rejected > 0) {
    frontendLogger.warn("app.init", `部分初始化失败: ${rejected} 项`);
    notify(false, `⚠ 部分数据加载失败（${rejected} 项），请刷新重试`);
  }
  initErrorCount = 0;
  state.isLoading = false;

  const wsMgr = useWebSocket();
  wsMgr.connectWebSocket();
  wsMgr.setupVisibilityChange();
  void autoCheckUpdateOnStartup();

  const statusPoll = setInterval(() => {
    const s = useStatus();
    if (s.fetchStatusFailCount.value > 0) return;
    void s.fetchStatus().catch((err) => frontendLogger.warn("status_poll", err));
  }, TIMING.STATUS_POLL_INTERVAL);
  const autostartPoll = setInterval(() => useStatus().fetchAutostart(), TIMING.AUTOSTART_POLL_INTERVAL);
  timers.push(statusPoll, autostartPoll);

  // 应用外观
  appearance.applyAppearance();
  frontendLogger.info("app.init", "初始化完成");
}

function destroyApp(): void {
  timers.forEach((t) => clearInterval(t));
  timers = [];
  useWebSocket().destroy();
}

export function useUi() {
  return {
    state,
    loginHistory,
    init,
    destroyApp,
    fetchAppVersion,
    fetchLogs,
    fetchLoginHistory,
    clearLoginHistory,
    checkInitStatus,
    finishWizard,
    checkUpdate,
    autoCheckUpdateOnStartup,
    fetchBrowsers,
    selectBrowser,
    getActiveBrowserChannel,
    handleBrowserClick,
    installPlaywrightChromium,
    fetchNetworkInterfaces,
    toggleMonitor,
    manualLogin,
    cancelLogin,
    testNetwork,
    openFullscreen,
    closeFullscreen,
    quitApp,
  };
}
