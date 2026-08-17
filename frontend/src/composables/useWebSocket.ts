/**
 * WebSocket 连接管理（单例）。
 * 替代原 websocketData + lifecycleMethods.connectWebSocket/_setupVisibilityChange。
 * 自动重连（指数退避 1s→30s）、应用层 ping、状态/日志消息分发到对应 composable。
 *
 * 修复 P2-12.10：入站消息做类型校验，非法消息记日志并丢弃。
 */

import { ref } from "vue";
import type { StatusSnapshot, LogEntry } from "../api/types";
import { ensureAuthToken } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { TIMING } from "../utils/constants";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
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
// 重连回调：断线期间 profiles/config/tasks/scheduled 等非实时推送数据会停留在旧值，
// 由上层（useUi）注册全量刷新回调，在重连成功时补齐（历史遗留 F1）。
let reconnectHandlers: Array<() => void | Promise<void>> = [];

const wsReconnecting = ref(false);
const wsRetryCount = ref(0);
// 本页面连接被另一个页面顶替（后端 ws_kicked）：置位后停止自动重连，
// 避免多标签页互相踢下线形成死循环
const wsKicked = ref(false);

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

/**
 * 注册 WebSocket 重连回调。
 *
 * 仅在“重连成功”（非首次连接）时触发，用于补齐断线期间无法通过推送同步的数据。
 * 返回注销函数：调用方销毁时应主动退订，避免回调泄漏（M4 生命周期修复）。
 */
function onWsReconnect(cb: () => void | Promise<void>): () => void {
  reconnectHandlers.push(cb);
  return () => {
    const i = reconnectHandlers.indexOf(cb);
    if (i !== -1) reconnectHandlers.splice(i, 1);
  };
}

async function connectWebSocket(): Promise<void> {
  if (destroyed) return;
  // WS 无法携带自定义头，后端通过 ?token= 查询参数鉴权（缺失将被 403 拒绝升级）
  const token = await ensureAuthToken();
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

  ws = new WebSocket(wsUrl);
  frontendLogger.info("websocket", `正在连接 ${wsUrl}`);

  ws.onopen = () => {
    retryCount = 0;
    wsRetryCount.value = 0;
    wsReconnecting.value = false;
    wsKicked.value = false;
    if (ws) frontendLogger.setWebSocket(ws);
    frontendLogger.info("websocket", "已连接");
    if (wasConnected) {
      void status.fetchStatus();
      // 重连场景：补齐断线期间缺失的日志（后端 broadcast 不缓存历史）
      void logs.fetchLogs();
      // 全量刷新非实时推送数据（profiles/config/tasks/scheduled 等），
      // 避免它们停留在断线前的旧值（历史遗留 F1）
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
        const d = parsed.data as { step_index: number; total_steps?: number; description?: string; step_type?: string; session_type?: string };
        // 仅调试会话（worker 注入 session_type=debug）的步骤进度才写入调试面板；
        // 登录/浏览器任务的步骤进度只进日志流，避免登录步骤被误显示为“调试步骤进度”
        if (d.session_type === "debug") {
          useDebug().handleStepProgress(d);
        }
        // 无论哪种会话都写入主日志流，使任务逐步运行的过程可见
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
      // 浏览器原生弹窗被自动确认（accept）后无法可视化弹出，这里把文案写入日志，
      // 使“登录成功！”等被吞掉的提示仍可见（对应 worker 的 dialog 事件）。
      // 仅记录日志、不弹 toast/通知，避免页面弹窗打扰用户（P3）。
      if (d && typeof d.message === "string" && d.message.trim()) {
        logs.appendLogs(
          [{ timestamp: new Date().toISOString(), level: "INFO", source: "task", message: `弹窗提示: ${d.message}` }],
          logs.autoScroll.value,
        );
      }
    } else if (parsed.type === "pong") {
      /* 心跳响应 */
    } else if (parsed.type === "ws_kicked") {
      // 被另一个页面顶替：停止自动重连，避免互相踢死循环
      wsKicked.value = true;
      frontendLogger.warn("websocket", "连接被另一页面顶替，停止重连");
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
    // 被另一页面顶替（ws_kicked）：停止自动重连，避免多标签页互相踢下线死循环
    if (wsKicked.value) return;
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
  // 幂等：重复调用不叠加 listener（此前直接覆盖赋值会泄漏前一次注册，M4）
  if (visibilityHandler) return;
  visibilityHandler = () => {
    // 被顶替的页面不再重连（即使恢复可见）
    if (wsKicked.value) return;
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
 * 被顶替（ws_kicked）后手动恢复本页连接（A12）。
 *
 * 重置 wsKicked 并主动发起一次全新连接；connectWebSocket 内部会先清理旧
 * 连接（handlers 置空 + close）再新建，因此旧连接残留不会阻断本次连接。
 * 新连接会被后端视为最新世代（顶替另一页面）；若随后 onclose 且非 kicked，
 * 正常自动重连逻辑自然接管（wsKicked 已复位，不会被残留状态卡住）。
 */
function resumeFromKicked(): void {
  if (!wsKicked.value || destroyed) return;
  wsKicked.value = false;
  retryCount = 0;
  wsRetryCount.value = 0;
  frontendLogger.info("websocket", "用户请求在本页恢复连接");
  connectWebSocket();
}

export function useWebSocket() {
  return { connectWebSocket, setupVisibilityChange, destroy, onWsReconnect, resumeFromKicked, wsReconnecting, wsRetryCount, wsKicked };
}
