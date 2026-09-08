/**
 * WebSocket 连接管理（单例）。
 * 多标签页共存：所有页面同时订阅同一广播通道，互不顶替；多标签页可同时在线。
 * 自动重连（指数退避 1s→60s，仅网络断开时触发）、应用层 ping、状态/日志消息分发到对应 composable。
 *
 * 断连分两类展示（顶栏重连条消费）：
 * - unreachable：后端进程没起/端口无监听，提示检查后端是否已启动；
 * - unauthorized：疑似后端重启换发 token，建连前已强制刷新仍被拒，提示稍后自动重试。
 */

import { ref } from "vue";
import type { StatusSnapshot, LogEntry } from "../api/types";
import { ensureAuthToken, refreshAuthToken } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { TIMING } from "../utils/constants";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useDebug } from "./useDebug";

const WS_MAX_BACKOFF = 60_000;
/** 连续 N 次建连失败后判定"后端可能没起"，顶栏提示检查后端而非无脑重连 */
const WS_UNREACHABLE_THRESHOLD = 3;

/** 断连原因：null=已连接/未开始重连，unreachable=后端无响应，unauthorized=疑似 token 失效 */
export type WsDisconnectReason = "unreachable" | "unauthorized" | null;

let ws: WebSocket | null = null;
let destroyed = false;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let pingTimer: ReturnType<typeof setInterval> | undefined;
let retryCount = 0;
let wasConnected = false;
let visibilityHandler: (() => void) | null = null;
let connecting = false;
let reconnectHandlers: Array<() => void | Promise<void>> = [];

const wsReconnecting = ref(false);
const wsRetryCount = ref(0);
/** 当前断连原因（连接成功/首轮建连中为 null）；顶栏据此展示不同指引 */
const wsDisconnectReason = ref<WsDisconnectReason>(null);

const status = useStatus();
const logs = useLogs();

interface WsEnvelope {
  type: string;
  data?: unknown;
}

function isValidStatus(data: unknown): data is Partial<StatusSnapshot> {
  return typeof data === "object" && data !== null;
}
function isValidLog(data: unknown): data is LogEntry {
  return (
    typeof data === "object" &&
    data !== null &&
    typeof (data as LogEntry).timestamp === "string" &&
    typeof (data as LogEntry).level === "string" &&
    typeof (data as LogEntry).message === "string"
  );
}

function onWsReconnect(cb: () => void | Promise<void>): () => void {
  reconnectHandlers.push(cb);
  return () => {
    const i = reconnectHandlers.indexOf(cb);
    if (i !== -1) reconnectHandlers.splice(i, 1);
  };
}

/** 安排下一次重连：指数退避 1s→60s；连续失败达阈值后标记 unreachable */
function scheduleReconnect(reason: Exclude<WsDisconnectReason, null>): void {
  if (destroyed) return;
  connecting = false;
  wsReconnecting.value = true;
  wsRetryCount.value = retryCount;
  // unauthorized（token 疑似失效）连续出现也指向"后端刚重启"同一结论，
  // 与 unreachable 同阈值提示，避免用户对着 401 干等整轮退避。
  wsDisconnectReason.value = retryCount + 1 >= WS_UNREACHABLE_THRESHOLD ? "unreachable" : reason;
  const delay = Math.min(TIMING.WS_BACKOFF_BASE * Math.pow(2, retryCount), WS_MAX_BACKOFF);
  retryCount++;
  frontendLogger.warn("websocket", `连接已断开（${reason}），${delay / 1000}s 后重连…`);
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = setTimeout(() => {
    if (!destroyed) connectWebSocket();
  }, delay);
}

async function connectWebSocket(): Promise<void> {
  if (destroyed) return;
  if (connecting) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return;
  }
  connecting = true;
  // 重连（非首次建连）先强制刷新 token：后端重启会换发 token，缓存旧值
  // 建连必被 401 关掉；刷新失败（后端没起）返回 null，以匿名建连走正常退避。
  // 首次建连走缓存路径：页面加载时 http 请求已并行取过 token，避免重复请求。
  const token = wasConnected || retryCount > 0 ? await refreshAuthToken() : await ensureAuthToken();
  if (destroyed) {
    connecting = false;
    return;
  }
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = `${protocol}//${window.location.host}/ws/logs${token ? `?token=${encodeURIComponent(token)}` : ""}`;
  if (retryTimer) clearTimeout(retryTimer);
  if (ws) {
    frontendLogger.setWebSocket(null);
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
    try {
      ws.close();
    } catch {
      /* ignore */
    }
  }

  try {
    ws = new WebSocket(wsUrl);
  } catch (e) {
    frontendLogger.error("websocket", "创建 WebSocket 失败", e);
    scheduleReconnect(token !== null ? "unauthorized" : "unreachable");
    return;
  }
  frontendLogger.info("websocket", `正在连接日志通道 ${wsUrl.split("?")[0]}`);

  ws.onopen = () => {
    connecting = false;
    retryCount = 0;
    wsRetryCount.value = 0;
    wsReconnecting.value = false;
    wsDisconnectReason.value = null;
    if (ws) frontendLogger.setWebSocket(ws);
    frontendLogger.info("websocket", "已连接");
    if (wasConnected) {
      void status.fetchStatus();
      void logs.fetchLogs(true);
      for (const cb of reconnectHandlers) {
        try {
          void cb();
        } catch (e) {
          frontendLogger.warn("websocket", "重连回调执行失败", e);
        }
      }
    }
    wasConnected = true;
  };

  ws.onmessage = (event: MessageEvent) => {
    let parsed: WsEnvelope;
    try {
      parsed = JSON.parse(event.data) as WsEnvelope;
    } catch (e) {
      frontendLogger.error("websocket", "消息解析错误", e);
      return;
    }
    if (typeof parsed.type !== "string") {
      frontendLogger.warn("websocket", "消息缺少 type 字段");
      return;
    }
    if (parsed.type === "status") {
      if (isValidStatus(parsed.data)) status.updateStatus(parsed.data);
      else frontendLogger.warn("websocket", "status 消息数据无效");
    } else if (parsed.type === "log") {
      if (isValidLog(parsed.data)) logs.appendLogs([parsed.data], logs.autoScroll.value);
      else frontendLogger.warn("websocket", "log 消息数据无效");
    } else if (parsed.type === "screenshot") {
      if (parsed.data && typeof parsed.data === "object") {
        useDebug().handleScreenshot(parsed.data as { url?: string; step_index?: number; description?: string });
      } else {
        frontendLogger.warn("websocket", "screenshot 消息数据无效");
      }
    } else if (parsed.type === "step_progress") {
      if (parsed.data && typeof parsed.data === "object") {
        const d = parsed.data as { step_index: number; total_steps?: number; description?: string; step_type?: string; session_type?: string };
        if (d.session_type === "debug") {
          useDebug().handleStepProgress(d);
        }
        // 步骤日志由后端以任务域 info 留痕（source=task，带 seq/统一时间戳），前端不再合成
      } else {
        frontendLogger.warn("websocket", "step_progress 消息数据无效");
      }
    } else if (parsed.type === "dialog") {
      /* 弹窗提示已由后端留痕为任务域日志（source=task），事件本身无其他前端消费 */
    } else if (parsed.type === "pong") {
      /* 心跳响应 */
    } else {
      frontendLogger.warn("websocket", "未知消息类型: " + parsed.type);
    }
  };

  ws.onclose = () => {
    frontendLogger.setWebSocket(null);
    if (pingTimer) {
      clearInterval(pingTimer);
      pingTimer = undefined;
    }
    if (destroyed) return;
    // 握手 401（token 失效）与后端没起在 onclose 侧无从区分：token 刚刷新过还被拒
    // 归为 unauthorized（下一轮继续刷新），拿不到 token 的归为 unreachable。
    scheduleReconnect(token !== null ? "unauthorized" : "unreachable");
  };

  ws.onerror = (e) => {
    frontendLogger.warn("websocket", "连接错误", e);
  };

  if (pingTimer) clearInterval(pingTimer);
  pingTimer = setInterval(() => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "ping" }));
    }
  }, TIMING.WS_PING_INTERVAL);
}

function setupVisibilityChange(): void {
  if (visibilityHandler) return;
  visibilityHandler = () => {
    // 仅当真正断开且未销毁时才重连；避免可见性切换在连接正常时触发不必要的重连
    // 后端已改为多连接共存，无需因可见性变化强行重连
    if (document.visibilityState === "visible" && !destroyed && ws?.readyState === WebSocket.CLOSED) {
      frontendLogger.info("websocket", "页面恢复可见，尝试重连");
      connectWebSocket();
    }
  };
  document.addEventListener("visibilitychange", visibilityHandler);
}

function destroy(): void {
  destroyed = true;
  connecting = false;
  reconnectHandlers = [];
  if (retryTimer) clearTimeout(retryTimer);
  if (pingTimer) clearInterval(pingTimer);
  if (visibilityHandler) {
    document.removeEventListener("visibilitychange", visibilityHandler);
    visibilityHandler = null;
  }
  if (ws) {
    frontendLogger.setWebSocket(null);
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
    try {
      ws.close();
    } catch {
      /* ignore */
    }
    ws = null;
  }
}

/**
 * 立即重试一次连接（顶栏"立即重试"按钮用）。
 * 清掉待触发的退避计时并归零计数，下一跳以 1s 间隔建连；
 * connecting/OPEN/CONNECTING 由 connectWebSocket 内部防重入。
 */
function retryNow(): void {
  if (destroyed) return;
  if (retryTimer) clearTimeout(retryTimer);
  retryCount = 0;
  wsRetryCount.value = 0;
  void connectWebSocket();
}

export function useWebSocket() {
  return { connectWebSocket, retryNow, setupVisibilityChange, destroy, onWsReconnect, wsReconnecting, wsRetryCount, wsDisconnectReason };
}
