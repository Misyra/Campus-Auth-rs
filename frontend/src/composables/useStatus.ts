/**
 * 状态快照 + 自启动 + 全局 busy 锁（单例）。
 * 替代原 statusData / websocketData 中的状态字段 + lifecycleMethods.fetchStatus/fetchAutostart。
 */

import { reactive, computed } from "vue";
import type {
  AutostartStatus,
  ConnectivityAssessment,
  ProbeEvidence,
  NetworkState,
  StatusSnapshot,
} from "../api/types";
import { monitorApi, autostartApi } from "../api";
import { ApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { createFirstFailNotifier } from "../utils/guards";
import { useNotifications } from "./useNotifications";

const status = reactive<StatusSnapshot>({
  monitoring: false,
  network_check_count: 0,
  login_attempt_count: 0,
  consecutive_failures: 0,
  retry_count: 0,
  last_check_time: null,
  monitoring_seconds: 0,
  runtime_seconds: 0,
  network_connected: false,
  network_state: "unknown",
  pause_active: false,
  cooling_down: false,
  cooling_down_remaining: null,
  connectivity: {
    status: "unknown",
    confidence: "low",
    reason: "not_checked",
    auth_endpoint: "not_checked",
    recovery_advice: "not_evaluated",
  },
  last_probe_evidence: null,
  update_progress: null,
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
  env: false,
});

const fetchStatusFail = createFirstFailNotifier();

/** WebSocket 与轮询都可能接入旧后端，未知状态统一收敛为 unknown。 */
function normalizeNetworkState(value: unknown): NetworkState {
  switch (value) {
    case "online":
    case "captive_portal":
    case "offline":
    case "unknown":
      return value;
    default:
      return "unknown";
  }
}

/**
 * 后端 StatusSnapshot 字段 → 前端字段映射。
 * 后端：monitor_enabled/network_status/consecutive_failures/retry_count/uptime_seconds/probe_total/login_total
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
  out.network_state = normalizeNetworkState(raw.network_status ?? status.network_state);
  out.network_connected = out.network_state === "online";
  out.pause_active = Boolean(raw.pause_active ?? status.pause_active);
  out.cooling_down = Boolean(raw.cooling_down ?? status.cooling_down);
  out.cooling_down_remaining = raw.cooling_down_remaining == null
    ? null
    : Number(raw.cooling_down_remaining);
  out.connectivity = (raw.connectivity as ConnectivityAssessment | undefined) ?? status.connectivity;
  out.last_probe_evidence = (raw.last_probe_evidence as ProbeEvidence | null | undefined)
    ?? status.last_probe_evidence;
  // G23：检测/登录次数改用后端新增的累计字段 probe_total / login_total
  //（consecutive_failures / retry_count 是瞬时重试计数，此前被误当作次数展示）。
  // 后端字段缺失时沿用当前值兜底（旧版本后端不至于把计数清零）
  out.network_check_count = Number(raw.probe_total ?? status.network_check_count ?? 0);
  out.login_attempt_count = Number(raw.login_total ?? status.login_attempt_count ?? 0);
  out.consecutive_failures = Number(raw.consecutive_failures ?? status.consecutive_failures ?? 0);
  out.retry_count = Number(raw.retry_count ?? status.retry_count ?? 0);
  out.monitoring_seconds = Number(raw.monitoring_seconds ?? status.monitoring_seconds ?? 0);
  out.runtime_seconds = Number(raw.uptime_seconds ?? status.runtime_seconds ?? 0);
  out.last_check_time = (raw.last_check_time as string | null) ?? status.last_check_time;
  out.login_status = raw.login_status as string | undefined;
  out.snapshot_version = Number(raw.snapshot_version ?? 0);
  out.update_progress = (raw.update_progress as StatusSnapshot["update_progress"]) ?? null;
  return out;
}

// B6：status 双源竞态防护。
// WS 推送是权威实时源，轮询是低优先级刷新。P14：后端快照携带单调新鲜度字段，
// 据此比较而非"epoch 不等即丢"：
// - 优先用 snapshot_version（每次发布严格 +1，可区分同一秒内的多次变化）；
// - 旧后端无该字段（0）时回退 uptime_seconds（运行时长，秒级单调递增）：
//   轮询响应不早于当前已应用状态 → 应用；更早 → 丢弃，避免过期响应回退状态。
// statusEpoch 计数器保留用于中断明显过旧的请求：in-flight 期间 WS 推送超过
// 1 次（差值 > 1）说明期间数据已多次演进，直接丢弃不再比较。
let statusEpoch = 0;
// 当前已应用状态的新鲜度（后端 snapshot_version / uptime_seconds，单调递增；0 表示尚未应用过）
let appliedVersion = 0;
let appliedUptime = 0;

/** 应用映射后的状态并同步已应用新鲜度（raw 为后端原始快照，可能缺新鲜度字段） */
function applyStatus(mapped: Partial<StatusSnapshot>, raw: Record<string, unknown>): void {
  Object.assign(status, mapped);
  const version = Number(raw.snapshot_version);
  if (Number.isFinite(version) && version > 0) appliedVersion = version;
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
  if (status.pause_active) return "checking";
  if (status.network_state === "unknown") return "checking";
  if (status.network_connected === false) return "disconnected";
  return "connected";
});

const networkStatusText = computed(() => {
  if (!status.monitoring) return "已停止";
  if (status.pause_active) return "自动监测已暂停";
  // 网络事实与引擎运行态分离；暂停不会覆盖最近一次网络结论。
  switch (status.network_state) {
    case "online":
      return "公网连接正常";
    case "captive_portal":
      return status.cooling_down ? "需要认证 · 恢复冷却中" : "需要校园网认证";
    case "offline":
      return "网络暂不可达";
    default:
      return "等待有效检测结果";
  }
});

const networkStatusDetail = computed(() => {
  if (!status.monitoring) return "自动监测停止后，手动网络测试仍可使用";
  if (status.pause_active) return "暂停期间不会自动检测或登录；上次网络结论已保留";
  if (status.cooling_down) {
    const seconds = status.cooling_down_remaining;
    return seconds == null ? "连续登录失败，稍后自动重试" : `连续登录失败，约 ${seconds} 秒后重试`;
  }
  switch (status.connectivity.reason) {
    case "not_checked":
      return "尚未完成第一轮检测";
    case "internet_verified":
      return "HTTP 204 或 URL 内容探测已确认可访问公网";
    case "captive_detected":
      return status.connectivity.recovery_advice === "attempt_login_once"
        ? "发现门户劫持；认证入口预检失败，本次门户事件仅谨慎尝试一次"
        : "发现门户劫持，自动登录会在运行态门控通过后启动";
    case "external_failed_auth_reachable":
      return "公网探测失败但校园网认证入口可达，按需要认证处理";
    case "all_probes_failed":
      return "所有已启用的公网探测均失败，等待链路恢复";
    case "weak_evidence_only":
      return "仅有 TCP 弱证据，暂不据此认定公网可用";
    case "conflicting_evidence":
      return "探测证据相互冲突，将在下一轮重新确认";
    case "no_probes_enabled":
      return "没有启用有效公网探测，自动恢复不会启动";
    default:
      return "检测状态待确认";
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
    // 否则按单调新鲜度比较：仅当响应不早于当前已应用状态才应用，
    // 替换原"epoch 不等即丢"——相同数据的 WS 推送不再导致轮询响应被无谓丢弃。
    // 优先 snapshot_version（严格单调）；旧后端缺字段时回退 uptime_seconds 比较
    const freshVersion = Number(raw.snapshot_version ?? 0);
    const freshUptime = Number(raw.uptime_seconds ?? 0);
    if (freshVersion > 0) {
      if (appliedVersion > 0 && freshVersion < appliedVersion) return;
    } else if (freshUptime > 0 && freshUptime < appliedUptime) {
      return;
    }
    applyStatus(mapBackendStatus(raw), raw);
    // F3：从失败恢复时提示已重连（trackRecovery 仅在之前处于失败状态时返回 true）
    if (fetchStatusFail.trackRecovery()) {
      notify(true, "已重新连接到服务器", "network");
    }
  } catch (error) {
    frontendLogger.warn("status", "获取状态失败", error);
    if (fetchStatusFail.trackFailure()) {
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
    networkStatus,
    networkStatusText,
    networkStatusDetail,
    updateStatus,
    fetchStatus,
    fetchAutostart,
  };
}
