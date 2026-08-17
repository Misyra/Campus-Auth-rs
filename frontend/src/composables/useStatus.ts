/**
 * 状态快照 + 自启动 + 全局 busy 锁（单例）。
 * 替代原 statusData / websocketData 中的状态字段 + lifecycleMethods.fetchStatus/fetchAutostart。
 */

import { reactive, ref, computed } from "vue";
import type { StatusSnapshot, AutostartStatus } from "../api/types";
import { monitorApi, autostartApi } from "../api";
import { ApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useNotifications } from "./useNotifications";

const status = reactive<StatusSnapshot>({
  monitoring: false,
  network_check_count: 0,
  login_attempt_count: 0,
  last_check_time: null,
  runtime_seconds: 0,
  network_connected: false,
  network_state: "unknown",
});

const autostart = reactive<AutostartStatus>({
  platform: "-",
  enabled: false,
  method: "-",
  location: "",
  runtime_mode: "full",
});

/** 全局并发锁，跨多个 composable 共享 */
const busy = reactive({
  save: false,
  monitor: false,
  action: false,
  login: false,
  loginCooldown: false,
  autostart: false,
  detect: false,
  editorDetect: false,
  debug: false,
  uninstall: false,
  ocr: false,
  ocrRec: false,
});

const fetchStatusFailCount = ref(0);

/**
 * 后端 StatusSnapshot 字段 → 前端字段映射。
 * 后端：monitor_enabled/network_status/consecutive_failures/retry_count/uptime_seconds
 * 前端：monitoring/network_state+network_connected/network_check_count/login_attempt_count/runtime_seconds
 * P17：仅显式映射前端实际消费的字段，不再 Object.assign(out, raw) 混入后端
 * 原始字段——索引签名兜底会让拼写错误也能编译，新增字段消费需在此显式登记。
 */
function mapBackendStatus(raw: Record<string, unknown>): Partial<StatusSnapshot> {
  const out: Partial<StatusSnapshot> = {};
  // monitoring 映射到 engine_state==="running"（monitor_enabled 是配置层面，stop 后不更新）
  const engineState = String(raw.engine_state ?? "");
  out.monitoring = engineState === "running";
  out.engine_state = engineState || undefined;
  out.network_state = String(raw.network_status ?? status.network_state ?? "unknown");
  out.network_connected = raw.network_status === "online";
  out.network_check_count = Number(raw.consecutive_failures ?? status.network_check_count ?? 0);
  out.login_attempt_count = Number(raw.retry_count ?? status.login_attempt_count ?? 0);
  out.runtime_seconds = Number(raw.uptime_seconds ?? status.runtime_seconds ?? 0);
  out.last_check_time = (raw.last_check_time as string | null) ?? status.last_check_time;
  out.login_status = raw.login_status as string | undefined;
  return out;
}

// B6：status 双源竞态防护。
// WS 推送是权威实时源，轮询是低优先级刷新。P14：后端快照携带单调新鲜度字段
// uptime_seconds（运行时长，只增不减），据此比较而非"epoch 不等即丢"：
// - 轮询响应的 uptime_seconds ≥ 当前已应用状态 → 应用（即使 in-flight 期间
//   有过 WS 推送，只要数据不比已应用状态旧就不会回退界面）；
// - < 已应用状态 → 丢弃，避免过期轮询响应回退状态。
// statusEpoch 计数器保留用于中断明显过旧的请求：in-flight 期间 WS 推送超过
// 1 次（差值 > 1）说明期间数据已多次演进，直接丢弃不再比较。
let statusEpoch = 0;
// 当前已应用状态的新鲜度（后端 uptime_seconds，单调递增；0 表示尚未应用过）
let appliedUptime = 0;

/** 应用映射后的状态并同步已应用新鲜度（raw 为后端原始快照，可能缺 uptime_seconds） */
function applyStatus(mapped: Partial<StatusSnapshot>, raw: Record<string, unknown>): void {
  Object.assign(status, mapped);
  const uptime = Number(raw.uptime_seconds);
  if (Number.isFinite(uptime) && uptime > 0) appliedUptime = uptime;
}

/** WebSocket 推送的状态更新入口（权威源，总是应用） */
function updateStatus(data: Partial<StatusSnapshot>): void {
  if (data && typeof data === "object") {
    statusEpoch += 1;
    const raw = data as Record<string, unknown>;
    applyStatus(mapBackendStatus(raw), raw);
  }
}

/** 轮询请求发起时记录计数器快照，供响应到达时判定是否已过期 */
function statusEpochAtRequest(): number {
  return statusEpoch;
}

const networkStatus = computed(() => {
  if (!status.monitoring) return "idle";
  if (status.network_state === "unknown") return "checking";
  if (status.network_connected === false) return "disconnected";
  return "connected";
});

const networkStatusText = computed(() => {
  if (!status.monitoring) return "已停止";
  // 后端 network_status 实际取值：online / captive_portal / offline / paused / unknown
  switch (status.network_state) {
    case "online":
      return "在线监测中";
    case "captive_portal":
      return "检测到门户劫持";
    case "offline":
      return "网络断开";
    case "paused":
      return "暂停时段";
    default:
      return "正在启动监控";
  }
});

async function fetchStatus(): Promise<void> {
  const { notify } = useNotifications();
  const startEpoch = statusEpochAtRequest();
  try {
    const data = await monitorApi.fetchStatus();
    const raw = data as unknown as Record<string, unknown>;
    // B6/P14：in-flight 期间 WS 推送超过 1 次（差值 > 1）→ 请求明显过旧，直接丢弃
    if (statusEpoch - startEpoch > 1) return;
    // 否则按单调新鲜度比较：仅当响应不早于当前已应用状态（uptime_seconds）才应用，
    // 替换原"epoch 不等即丢"——相同数据的 WS 推送不再导致轮询响应被无谓丢弃
    const freshUptime = Number(raw.uptime_seconds ?? 0);
    if (freshUptime > 0 && freshUptime < appliedUptime) return;
    applyStatus(mapBackendStatus(raw), raw);
    if (fetchStatusFailCount.value > 0) {
      fetchStatusFailCount.value = 0;
      notify(true, "已重新连接到服务器", "network");
    }
  } catch (error) {
    fetchStatusFailCount.value++;
    frontendLogger.warn("status", "获取状态失败", error);
    if (fetchStatusFailCount.value === 1) {
      notify(false, "无法连接到服务器，请检查后端是否已关闭", "network");
    }
  }
}

let autostartInFlight = false;
async function fetchAutostart(): Promise<void> {
  if (autostartInFlight) return;
  autostartInFlight = true;
  try {
    const data = await autostartApi.fetchStatus();
    Object.assign(autostart, data);
  } catch (error) {
    frontendLogger.warn("autostart", "获取自启动状态失败", error);
    if (error instanceof ApiError && error.status === 404) {
      Object.assign(autostart, {
        platform: "-",
        enabled: false,
        method: "当前后端不支持",
        location: "",
        runtime_mode: "full",
      });
    }
  } finally {
    autostartInFlight = false;
  }
}

export function useStatus() {
  return {
    status,
    autostart,
    busy,
    fetchStatusFailCount,
    networkStatus,
    networkStatusText,
    updateStatus,
    fetchStatus,
    fetchAutostart,
  };
}
