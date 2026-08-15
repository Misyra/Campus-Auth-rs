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
  LoginHistoryItem,
} from "../api/types";
import { systemApi, browsersApi, monitorApi } from "../api";
import { ApiError, extractApiError } from "../api/client";
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
});

const loginHistory = reactive<LoginHistoryItem[]>([]);
const { busy } = useStatus();
const { fetchLogs } = useLogs();
const { toastOnly } = useToast();
const { notify } = useNotifications();
const { confirm } = useConfirm();

// init 防重入守卫：避免重复调用叠加定时器与 WS 监听器（历史遗留 F7）
let initialized = false;
// F9：记录 init 启动的轮询定时器 id，quitApp 时统一清理
const statusPollTimerIds: number[] = [];
const autostartPollTimerIds: number[] = [];

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
    if (error instanceof ApiError && error.status) state.showWizard = false;
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

async function autoCheckUpdateOnStartup(): Promise<void> {
  try {
    const data = await systemApi.checkUpdate();
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

async function quitApp(): Promise<void> {
  const ok = await confirm({ title: "退出应用", message: "确定要退出应用吗？", danger: true });
  if (!ok) return;
  // F9：先清理轮询定时器，避免退出流程中页面仍持续向后端请求
  statusPollTimerIds.forEach((id) => clearInterval(id));
  statusPollTimerIds.length = 0;
  autostartPollTimerIds.forEach((id) => clearInterval(id));
  autostartPollTimerIds.length = 0;
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
  // 防重入：重复 init 会叠加轮询定时器与 WS 监听器（历史遗留 F7）
  if (initialized) {
    frontendLogger.warn("app.init", "init 已执行，跳过重复初始化");
    return;
  }
  initialized = true;
  frontendLogger.info("app.init", "开始初始化");
  state.isLoading = true;

  const config = useConfig();
  const status = useStatus();
  const tasks = useTasks();
  const scripts = useScripts();
  const profiles = useProfiles();
  const scheduled = useScheduledTasks();
  const appearance = useAppearance();

  // 各 fetch 内部均自行 catch，不会 reject；此处仅需并行触发，无需统计失败项
  await Promise.allSettled([
    config.fetchConfig(),
    status.fetchStatus(),
    fetchLogs(),
    status.fetchAutostart(),
    checkInitStatus(),
    tasks.fetchTasks(),
    scripts.fetchScripts(),
    tasks.fetchActiveTask(),
    profiles.fetchProfiles(),
    tasks.fetchPureMode(),
    fetchLoginHistory(),
    scheduled.loadScheduledTasks(),
    config.fetchLogLevels(),
  ]);
  state.isLoading = false;

  const wsMgr = useWebSocket();
  // 注册重连全量刷新回调：断线重连后补齐非实时推送的数据（历史遗留 F1）
  // F1：重连刷新前检查 dirty，设置页有未保存编辑时跳过 fetchConfig，避免覆盖丢弃（对齐 useProfiles 守卫策略）
  wsMgr.onWsReconnect(async () => {
    const pending = [
      profiles.fetchProfiles(),
      tasks.fetchTasks(),
      tasks.fetchActiveTask(),
      scheduled.loadScheduledTasks(),
      // F8 顺带：补齐重连后遗漏的只读数据源
      scripts.fetchScripts(),
      tasks.fetchPureMode(),
      fetchLoginHistory(),
    ];
    if (!config.dirty.value) {
      pending.unshift(config.fetchConfig());
    }
    await Promise.allSettled(pending);
  });
  wsMgr.connectWebSocket();
  wsMgr.setupVisibilityChange();
  void autoCheckUpdateOnStartup();

  // F9：保存轮询定时器 id，退出时 clearInterval，避免 quitApp 后页面仍持续轮询
  const statusPollTimer = setInterval(() => {
    const s = useStatus();
    if (s.fetchStatusFailCount.value > 0) return;
    void s.fetchStatus().catch((err) => frontendLogger.warn("status_poll", err));
  }, TIMING.STATUS_POLL_INTERVAL);
  const autostartPollTimer = setInterval(() => useStatus().fetchAutostart(), TIMING.AUTOSTART_POLL_INTERVAL);
  statusPollTimerIds.push(statusPollTimer);
  autostartPollTimerIds.push(autostartPollTimer);

  // 应用外观
  appearance.applyAppearance();
  frontendLogger.info("app.init", "初始化完成");
}

export function useUi() {
  return {
    state,
    loginHistory,
    init,
    fetchLogs,
    fetchLoginHistory,
    clearLoginHistory,
    checkInitStatus,
    finishWizard,
    autoCheckUpdateOnStartup,
    toggleMonitor,
    manualLogin,
    cancelLogin,
    testNetwork,
    quitApp,
  };
}
