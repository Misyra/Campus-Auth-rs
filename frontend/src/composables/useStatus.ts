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
 */
function mapBackendStatus(raw: Record<string, unknown>): Partial<StatusSnapshot> {
  const out: Partial<StatusSnapshot> & Record<string, unknown> = {};
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
  // 保留后端原始字段也写入，避免遗漏
  Object.assign(out, raw);
  return out;
}

// B6：status 双源竞态防护。
// WS 推送是权威实时源，轮询是低优先级刷新。用一个本地单调递增计数器只标记
// 每次 WS 推送；轮询请求发出时记录当时计数器，响应返回时若期间发生过 WS 推送
// （计数器已前进）则判定为过期数据直接丢弃，避免过期轮询响应短暂回退状态。
let statusEpoch = 0;

/** WebSocket 推送的状态更新入口（权威源，总是应用） */
function updateStatus(data: Partial<StatusSnapshot>): void {
  if (data && typeof data === "object") {
    statusEpoch += 1;
    Object.assign(status, mapBackendStatus(data as Record<string, unknown>));
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
    // B6：请求在途期间若有 WS 推送（epoch 前进），本次轮询响应视为过期丢弃，
    // 避免用旧数据回退 WS 已推送的最新状态
    if (startEpoch !== statusEpoch) return;
    Object.assign(status, mapBackendStatus(data as unknown as Record<string, unknown>));
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
