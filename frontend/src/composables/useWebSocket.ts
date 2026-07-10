/**
 * WebSocket 连接管理（单例）。
 * 替代原 websocketData + lifecycleMethods.connectWebSocket/_setupVisibilityChange。
 * 自动重连（指数退避 1s→30s）、应用层 ping、状态/日志消息分发到对应 composable。
 *
 * 修复 P2-12.10：入站消息做类型校验，非法消息记日志并丢弃。
 */

import { ref } from "vue";
import type { StatusSnapshot, LogEntry } from "../api/types";
import { frontendLogger } from "../utils/logger";
import { TIMING } from "../utils/constants";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useNotifications } from "./useNotifications";
import { useDebug } from "./useDebug";

// 无上限重连：每次重连间隔指数退避（1s → 2s → ... → 上限60s），
// 页面可见性变化时重置计数器。避免网络短暂波动后永久断线。
const WS_MAX_BACKOFF = 60_000;

let ws: WebSocket | null = null;
let destroyed = false;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let pingTimer: ReturnType<typeof setInterval> | undefined;
let retryCount = 0; // 仅影响退避计算，不再作为硬上限
let wasConnected = false;
let visibilityHandler: (() => void) | null = null;

const wsReconnecting = ref(false);
const wsRetryCount = ref(0);

const status = useStatus();
const logs = useLogs();
const { notify } = useNotifications();

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

function connectWebSocket(): void {
  if (destroyed) return;
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = `${protocol}//${window.location.host}/ws/logs`;
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

  ws = new WebSocket(wsUrl);
  frontendLogger.info("websocket", `正在连接 ${wsUrl}`);

  ws.onopen = () => {
    retryCount = 0;
    wsRetryCount.value = 0;
    wsReconnecting.value = false;
    if (ws) frontendLogger.setWebSocket(ws);
    frontendLogger.info("websocket", "已连接");
    if (wasConnected) {
      void status.fetchStatus();
      // 重连场景：补齐断线期间缺失的日志（后端 broadcast 不缓存历史）
      void logs.fetchLogs();
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
      // 根据当前是否处于底部决定：在底部→自动滚动，不在底部→累计“新消息”计数
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
        useDebug().handleStepProgress(parsed.data as { step_index: number; total_steps?: number; description?: string; step_type?: string });
      } else {
        frontendLogger.warn("websocket", "step_progress 消息数据无效");
      }
    } else if (parsed.type === "pong") {
      /* 心跳响应 */
    } else {
      frontendLogger.warn("websocket", "未知消息类型: " + parsed.type);
    }
  };

  ws.onclose = () => {
    frontendLogger.setWebSocket(null);
    frontendLogger.warn("websocket", "连接已关闭");
    if (pingTimer) {
      clearInterval(pingTimer);
      pingTimer = undefined;
    }
    if (destroyed) return;
    // 无限重连：指数退避 1s→2s→4s→...→上限60s，不再因重试次数耗尽而永久断线
    wsReconnecting.value = true;
    wsRetryCount.value = retryCount;
    const delay = Math.min(TIMING.WS_BACKOFF_BASE * Math.pow(2, retryCount), WS_MAX_BACKOFF);
    retryCount++;
    retryTimer = setTimeout(() => {
      if (!destroyed) connectWebSocket();
    }, delay);
  };

  ws.onerror = () => {
    frontendLogger.error("websocket", "连接错误");
  };

  if (pingTimer) clearInterval(pingTimer);
  pingTimer = setInterval(() => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "ping" }));
    }
  }, TIMING.WS_PING_INTERVAL);
}

function setupVisibilityChange(): void {
  visibilityHandler = () => {
    if (document.visibilityState === "visible" && ws?.readyState !== WebSocket.OPEN) {
      retryCount = 0;
      frontendLogger.info("websocket", "页面恢复可见，尝试重连");
      connectWebSocket();
    }
  };
  document.addEventListener("visibilitychange", visibilityHandler);
}

function destroy(): void {
  destroyed = true;
  if (retryTimer) clearTimeout(retryTimer);
  if (pingTimer) clearInterval(pingTimer);
  if (visibilityHandler) document.removeEventListener("visibilitychange", visibilityHandler);
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

export function useWebSocket() {
  return { connectWebSocket, setupVisibilityChange, destroy, wsReconnecting, wsRetryCount };
}
