/**
 * 全局 UI 与生命周期编排（单例）。
 * 替代原 uiData + uiMethods + actionsMethods（部分）+ lifecycleMethods.
 * 协调各 composable 完成初始化、浏览器管理、更新检查、协议向导、退出等。
 */

import { reactive } from "vue";
import { router } from "../router";
import type {
  BrowserInfo,
  UpdateInfo,
  InitStatus,
  LoginHistoryItem,
} from "../api/types";
import { systemApi, browsersApi, monitorApi, actionsApi, historyApi } from "../api";
import { ApiError, extractApiError, isNoBrowserMessage } from "../api/client";
import { TIMING } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { createFetchGuard } from "../utils/guards";
import { useConfig } from "./useConfig";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useProfiles } from "./useProfiles";
import { useTasks } from "./useTasks";
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
const statusPollTimerIds: Array<ReturnType<typeof setInterval>> = [];
const autostartPollTimerIds: Array<ReturnType<typeof setInterval>> = [];

// F9：5s 守卫——init 与 Dashboard mount 双触发不再重复请求；force 供显式刷新绕过
const historyFetchGuard = createFetchGuard(5000);
async function fetchLoginHistory(force = false): Promise<void> {
  if (!historyFetchGuard.shouldFetch(force)) return;
  try {
    const data = await historyApi.fetch(30);
    if (Array.isArray(data)) {
      loginHistory.splice(0, loginHistory.length, ...data);
    }
    historyFetchGuard.markSuccess();
  } catch (error) {
    frontendLogger.error("history", "获取登录历史失败", error);
  }
}

async function clearLoginHistory(): Promise<void> {
  if (!loginHistory.length) return;
  const ok = await confirm({ title: "清空登录历史", message: `确定要清空所有 ${loginHistory.length} 条登录记录吗？此操作不可撤销。`, danger: true });
  if (!ok) return;
  try {
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
      frontendLogger.error("app", "密码解密失败，请在设置页面重新输入密码");
      notify(false, "密码解密失败，请在设置页面重新输入密码", "security");
    }
  } catch (error) {
    if (error instanceof ApiError && error.status) state.showWizard = false;
    frontendLogger.warn("app", "检查初始化状态失败", error);
  }
}

async function finishWizard(): Promise<void> {
  busy.save = true;
  try {
    await systemApi.agree();
    state.showWizard = false;
    state.agreedToTerms = false;
    frontendLogger.info("app", "已同意协议");
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
    notify(true, message, "update", { label: "前往更新", page: "settings-network" });
    frontendLogger.warn("update", `${message}，请前往“设置 · 网络与更新”页面更新`);
  } catch (error) {
    frontendLogger.debug("update", "启动自动检查更新失败", error);
  }
}

async function toggleMonitor(): Promise<void> {
  busy.monitor = true;
  const { status } = useStatus();
  try {
    frontendLogger.info("monitor", `${status.monitoring ? "stop" : "start"} monitor`);
    const data = status.monitoring ? await monitorApi.stop() : await monitorApi.start();
    toastOnly(true, extractMsg(data, "操作成功"));
    await useStatus().fetchStatus();
  } catch (error) {
    const msg = extractApiError(error, "操作失败");
    frontendLogger.error("monitor", "切换检测失败", msg);
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
    const loginTimeoutMs = (useConfig().config.browser.login_timeout || 90) * 1000;
    const data = await actionsApi.login(loginTimeoutMs);
    const msg = stripScreenshotHint(extractMsg(data, "登录完成"));
    // 无可用浏览器：弹窗引导去安装 Chromium（后端文案见 browser::NO_BROWSER_MESSAGE，
    // 由 isNoBrowserMessage 统一识别），确认后直达浏览器设置页。
    if (!data.success && isNoBrowserMessage(msg)) {
      notify(false, msg, "login", { label: "前往安装", page: "settings-browser" });
      const go = await confirm({
        title: "无可用浏览器",
        message: `${msg}。是否前往「设置 · 浏览器」安装 Chromium？`,
        confirmText: "前往安装",
        cancelText: "稍后",
      });
      if (go) await router.push({ name: "settings-browser" });
      await fetchLoginHistory(true);
      return;
    }
    notify(data.success, msg, "login");
    await fetchLoginHistory(true);
  } catch (error) {
    const msg = extractApiError(error, "手动登录失败");
    frontendLogger.error("login", "手动登录失败", msg);
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
    const data = await actionsApi.cancelLogin();
    toastOnly(true, data?.message || "已取消");
  } catch (error) {
    toastOnly(false, extractApiError(error, "取消登录失败"));
  }
}

async function testNetwork(): Promise<void> {
  busy.action = true;
  try {
    const data = await actionsApi.testNetwork();
    const summary = {
      online: "公网连接正常",
      captive_portal: "检测到校园网认证门户",
      offline: "网络暂不可达",
      unknown: "证据不足，暂不能判断",
    }[data.status];
    const reason = {
      not_checked: "尚未执行有效探测",
      internet_verified: "HTTP 204 或 URL 内容探测已确认公网可用",
      captive_detected: "探测请求被门户劫持",
      external_failed_auth_reachable: "公网失败，但认证入口可达",
      all_probes_failed: "所有已启用的公网探测均失败",
      weak_evidence_only: "只有 TCP 弱证据，不能确认公网可用",
      inconclusive_evidence: "探测返回非预期结果，无法确认网络状态",
      conflicting_evidence: "不同探测结果相互冲突",
      no_probes_enabled: "没有启用有效公网探测",
    }[data.reason];
    const local = {
      available: "本地链路正常",
      unavailable: "未发现有效本地网络接口",
      probe_failed: "本地链路检查失败",
      not_checked: "本地链路未检查",
    }[data.local_link];
    toastOnly(data.status === "online", `${summary}；${reason}；${local}（${data.duration_ms}ms）`);
  } catch (error) {
    toastOnly(false, extractApiError(error, "网络测试失败"));
  } finally {
    busy.action = false;
  }
}

async function quitApp(): Promise<void> {
  const ok = await confirm({ title: "退出应用", message: "确定要退出应用吗？", danger: true });
  if (!ok) return;
  try {
    busy.monitor = true;
    await systemApi.shutdown();
    // 只有后端确认收到关闭请求后才停止前端活动；请求失败时保留可恢复的现有会话。
    statusPollTimerIds.forEach((id) => clearInterval(id));
    statusPollTimerIds.length = 0;
    autostartPollTimerIds.forEach((id) => clearInterval(id));
    autostartPollTimerIds.length = 0;
    useWebSocket().destroy();
    showExitOverlay();
  } catch (error) {
    frontendLogger.error("app", "退出应用失败", error);
    toastOnly(false, extractApiError(error, "退出应用失败"));
  } finally {
    busy.monitor = false;
  }
}

/** 渲染"已安全退出"全屏遮罩：后端已关闭、WS 断开，前端不再有任何可用交互，只能关页 */
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

/** 剥离登录失败消息末尾附加的"截图: /logs/..."提示路径（对用户是噪音，日志面板里仍可看到）。
 *  正则与后端消息格式耦合：后端修改截图提示的括号样式或路径前缀时必须同步此处。
 *  注意：DashboardView.vue 另有针对"截图已保存：路径"格式的独立剥离实现——两套正则对应不同时代的后端文案，合并前先确认消息来源已统一。 */
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
  const profiles = useProfiles();
  const scheduled = useScheduledTasks();
  const appearance = useAppearance();

  // 各 fetch 内部均自行 catch，不会 reject；此处仅需并行触发，无需统计失败项
  // 任务与脚本共用任务目录（useTaskDirectory）的单次拉取，fetchTasks 即同时刷新两者
  await Promise.allSettled([
    config.fetchConfig(),
    status.fetchStatus(),
    fetchLogs(),
    status.fetchAutostart(),
    checkInitStatus(),
    tasks.fetchTasks(),
    tasks.fetchActiveTask(),
    profiles.fetchProfiles(),
    config.fetchPureMode(),
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
      // P15：重连刷新为显式刷新场景，force: true 绕过 5 秒守卫
      // 任务/脚本由任务目录单次拉取同时刷新，无需再调 fetchScripts
      profiles.fetchProfiles(true),
      tasks.fetchTasks(true),
      tasks.fetchActiveTask(),
      scheduled.loadScheduledTasks(true),
      // F8 顺带：补齐重连后遗漏的只读数据源
      config.fetchPureMode(true),
      fetchLoginHistory(true),
    ];
    if (!config.dirty.value) {
      pending.unshift(config.fetchConfig());
    }
    await Promise.allSettled(pending);
  });
  wsMgr.connectWebSocket();
  wsMgr.setupVisibilityChange();
  // 自动检查更新受设置门控（fetchConfig 已在上面 allSettled 中完成）：
  // 关闭后启动不再检查，仅保留设置页/关于页的手动"立即检查"
  if (config.config.updater.auto_check_enabled) {
    void autoCheckUpdateOnStartup();
  }

  // F9：保存轮询定时器 id，退出时 clearInterval，避免 quitApp 后页面仍持续轮询
  // 断连退避：后端失联时固定 30s 轮询会刷出大量失败日志；连续失败 3 次后
  // 间隔拉到 5min（WS 重连成功/轮询成功即恢复 30s），quitApp 时同样清理。
  let statusFailStreak = 0;
  let statusPollTimer: ReturnType<typeof setInterval> | undefined;
  const armStatusPoll = (intervalMs: number) => {
    if (statusPollTimer) {
      clearInterval(statusPollTimer);
      // 旧计时器已清除，从退出清理名单中移除，避免数组攒过期 id
      const i = statusPollTimerIds.indexOf(statusPollTimer);
      if (i !== -1) statusPollTimerIds.splice(i, 1);
    }
    statusPollTimer = setInterval(() => {
      const s = useStatus();
      void s
        .fetchStatus()
        .then(() => {
          if (statusFailStreak >= 3) {
            statusFailStreak = 0;
            armStatusPoll(TIMING.STATUS_POLL_INTERVAL);
          } else {
            statusFailStreak = 0;
          }
        })
        .catch((err) => {
          frontendLogger.warn("status", err);
          statusFailStreak += 1;
          if (statusFailStreak === 3) armStatusPoll(TIMING.STATUS_POLL_SLOW_INTERVAL);
        });
    }, intervalMs);
    statusPollTimerIds.push(statusPollTimer);
  };
  armStatusPoll(TIMING.STATUS_POLL_INTERVAL);
  const autostartPollTimer = setInterval(() => useStatus().fetchAutostart(), TIMING.AUTOSTART_POLL_INTERVAL);
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
    fetchLoginHistory,
    clearLoginHistory,
    finishWizard,
    toggleMonitor,
    manualLogin,
    cancelLogin,
    testNetwork,
    quitApp,
  };
}
