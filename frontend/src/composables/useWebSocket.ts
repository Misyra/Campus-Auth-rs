/**
 * WebSocket 连接管理（单例）。
 * 多标签页共存：所有页面同时订阅同一广播通道，互不顶替；多标签页可同时在线。
 * 自动重连（指数退避 1s→60s，仅网络断开时触发）、应用层 ping、状态/日志消息分发到对应 composable。
 */

import { ref } from "vue";
import type { StatusSnapshot, LogEntry } from "../api/types";
import { ensureAuthToken } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { TIMING } from "../utils/constants";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useDebug } from "./useDebug";

const WS_MAX_BACKOFF = 60_000;

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

async function connectWebSocket(): Promise<void> {
  if (destroyed) return;
  if (connecting) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return;
  }
  connecting = true;
  const token = await ensureAuthToken();
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
    connecting = false;
    frontendLogger.error("websocket", "创建 WebSocket 失败", e);
    if (!destroyed) {
      wsReconnecting.value = true;
      const delay = Math.min(TIMING.WS_BACKOFF_BASE * Math.pow(2, retryCount), WS_MAX_BACKOFF);
      retryCount++;
      retryTimer = setTimeout(() => {
        if (!destroyed) connectWebSocket();
      }, delay);
    }
    return;
  }
  frontendLogger.info("websocket", `正在连接日志通道 ${wsUrl.split("?")[0]}`);

  ws.onopen = () => {
    connecting = false;
    retryCount = 0;
    wsRetryCount.value = 0;
    wsReconnecting.value = false;
    if (ws) frontendLogger.setWebSocket(ws);
    frontendLogger.info("websocket", "已连接");
    if (wasConnected) {
      void status.fetchStatus();
      void logs.fetchLogs();
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
        const desc = (typeof d.description === "string" && d.description.trim()) || "执行步骤";
        const total = typeof d.total_steps === "number" ? d.total_steps : "";
        const entry: LogEntry = {
          timestamp: new Date().toISOString(),
          level: "INFO",
          source: "task",
          message: `步骤 ${(d.step_index ?? 0) + 1}${total ? `/${total}` : ""}: ${desc}`,
        };
        logs.appendLogs([entry], logs.autoScroll.value);
      } else {
        frontendLogger.warn("websocket", "step_progress 消息数据无效");
      }
    } else if (parsed.type === "dialog") {
      const d = parsed.data as { message?: string; action?: string } | null;
      if (d && typeof d.message === "string" && d.message.trim()) {
        logs.appendLogs(
          [{ timestamp: new Date().toISOString(), level: "INFO", source: "task", message: `弹窗提示: ${d.message}` }],
          logs.autoScroll.value,
        );
      }
    } else if (parsed.type === "pong") {
      /* 心跳响应 */
    } else {
      frontendLogger.warn("websocket", "未知消息类型: " + parsed.type);
    }
  };

  ws.onclose = () => {
    connecting = false;
    frontendLogger.setWebSocket(null);
    if (pingTimer) {
      clearInterval(pingTimer);
      pingTimer = undefined;
    }
    if (destroyed) return;
    wsReconnecting.value = true;
    wsRetryCount.value = retryCount;
    const delay = Math.min(TIMING.WS_BACKOFF_BASE * Math.pow(2, retryCount), WS_MAX_BACKOFF);
    retryCount++;
    frontendLogger.warn("websocket", `连接已断开，${delay / 1000}s 后重连…`);
    retryTimer = setTimeout(() => {
      if (!destroyed) connectWebSocket();
    }, delay);
  };

  ws.onerror = () => {};

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

export function useWebSocket() {
  return { connectWebSocket, setupVisibilityChange, destroy, onWsReconnect, wsReconnecting, wsRetryCount };
}
