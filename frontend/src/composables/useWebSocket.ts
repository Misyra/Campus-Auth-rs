/**
 * WebSocket 连接管理（单例）。
 * 多标签页共存：所有页面同时订阅同一广播通道，互不顶替；多标签页可同时在线。
 * 自动重连（指数退避 1s→60s，仅网络断开时触发）、应用层 ping、状态/日志消息分发到对应 composable。
 *
 * 鉴权门（P2-7）：建连前先取 token，引导未完成（无 fragment/缓存/local-token 失败）
 * 时**不发起匿名建连**——后端 /ws 必拒 401，匿名重连只会刷出一圈风暴与误导提示。
 * 挂起等待 client 的鉴权就绪事件，token 后到（引导页成功 / 401 触发重新引导）即连。
 * WS 已因旧凭据被拒处于退避等待时，就绪事件同样触发立即重连（handleAuthReady）。
 *
 * 断连分两类展示（顶栏重连条消费）：
 * - unreachable：真传输失败（后端进程没起/端口无监听），提示检查后端是否已启动；
 * - unauthorized：凭据被拒（后端在跑但 token 待换），静默等待重认证/就绪重连——
 *   连续出现**不**翻牌成 unreachable，「检查后端是否已启动」只用于真传输失败。
 */

import { ref } from "vue";
import type { StatusSnapshot, LogEntry } from "../api/types";
import { ensureAuthToken, onAuthReady, refreshAuthToken } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { TIMING } from "../utils/constants";
import { useStatus } from "./useStatus";
import { useLogs } from "./useLogs";
import { useDebug } from "./useDebug";

const WS_MAX_BACKOFF = 60_000;
/** 连续 N 次建连失败后判定"后端可能没起"，顶栏提示检查后端而非无脑重连 */
const WS_UNREACHABLE_THRESHOLD = 3;
/** 就绪事件触发立即重连的最小间隔：防同款被拒 token 反复下发把事件驱动退化成紧密循环 */
const AUTH_READY_RESTART_COOLDOWN_MS = 5_000;

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
/** 鉴权未就绪时的挂起等待退订句柄：token 后到即重连（P2-7 门） */
let authWaitOff: (() => void) | null = null;
/** 就绪事件订阅是否已登记（首个建连起订阅一次，与单例同生命周期） */
let authReadySubscribed = false;
/** 上次就绪事件触发立即重连的时刻（冷却限流用） */
let lastAuthReadyRestartAt = 0;

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

/** 安排下一次重连：指数退避 1s→60s；连续传输失败达阈值后标记 unreachable */
function scheduleReconnect(reason: Exclude<WsDisconnectReason, null>): void {
  if (destroyed) return;
  connecting = false;
  wsReconnecting.value = true;
  wsRetryCount.value = retryCount;
  // P2-7：unauthorized 不在阈值处翻牌成 unreachable——凭据被拒时后端明明在跑，
  // 「请检查后端是否已启动」只应指向真传输失败；鉴权被拒静默等待重认证即可，
  // 顶栏的「疑似后端重启，正在刷新凭据」文案已给足行动指引。
  wsDisconnectReason.value =
    reason === "unauthorized" || retryCount + 1 < WS_UNREACHABLE_THRESHOLD ? reason : "unreachable";
  const delay = Math.min(TIMING.WS_BACKOFF_BASE * Math.pow(2, retryCount), WS_MAX_BACKOFF);
  retryCount++;
  frontendLogger.warn("websocket", `连接已断开（${reason}），${delay / 1000}s 后重连…`);
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = setTimeout(() => {
    if (!destroyed) connectWebSocket();
  }, delay);
}

/**
 * 鉴权就绪事件入口（P2-7）：WS 因凭据被拒处于退避等待时立即重连，不再干等最长
 * 60s 的退避。正常在线（reason 非 unauthorized）不动连接；5s 冷却防同款被拒 token
 * 反复下发时事件驱动退化成紧密重连循环。
 */
function handleAuthReady(): void {
  if (destroyed) return;
  if (wsDisconnectReason.value !== "unauthorized") return;
  const now = Date.now();
  if (now - lastAuthReadyRestartAt < AUTH_READY_RESTART_COOLDOWN_MS) return;
  lastAuthReadyRestartAt = now;
  frontendLogger.info("websocket", "鉴权凭据已就绪，立即重连日志通道");
  retryNow();
}

async function connectWebSocket(): Promise<void> {
  if (destroyed) return;
  if (connecting) return;
  // 已在挂起等待鉴权就绪：不重复登记等待（retryNow/可见性触发都会走到这里）
  if (authWaitOff) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return;
  }
  // 首次建连时登记就绪事件订阅：WS 已起而 token 后到（401 触发重新引导）时
  // 立即重启退避中的连接（P2-7）
  if (!authReadySubscribed) {
    authReadySubscribed = true;
    onAuthReady(handleAuthReady);
  }
  connecting = true;
  // 重连（非首次建连）经 refreshAuthToken 清内存缓存后重读：token 已由后端持久化
  // （重启不变），恢复出的仍是同一 token，重连直接复用（口径见 client.ts
  // refreshAuthToken 注释；401 只在凭据被吊销/轮换时出现，由 request() 统一处理）。
  // 首次建连走缓存路径：页面加载时 http 请求已并行取过 token，避免重复请求。
  const token = wasConnected || retryCount > 0 ? await refreshAuthToken() : await ensureAuthToken();
  if (destroyed) {
    connecting = false;
    return;
  }
  if (token === null) {
    // 引导未完成（无 fragment/缓存，local-token 不可用）：匿名建连必被 401 拒——
    // 不发起（P2-7 风暴源头），挂起等待鉴权就绪事件，token 后到即带新凭据建连
    connecting = false;
    frontendLogger.info("websocket", "等待鉴权凭据就绪后再连接日志通道");
    authWaitOff = onAuthReady(() => {
      authWaitOff?.();
      authWaitOff = null;
      void connectWebSocket();
    });
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
    // 构造抛错属环境问题（非法 URL 等），按传输失败归类；token 恒在手（null 已被上面的鉴权门拦下）
    scheduleReconnect("unreachable");
    return;
  }
  frontendLogger.info("websocket", `正在连接日志通道 ${wsUrl.split("?")[0]}`);

  // 本条连接是否成功握手过：区分「连上过再断」（传输中断）与「建连即被拒」
  let handshakeOpened = false;

  ws.onopen = () => {
    handshakeOpened = true;
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
    // 分类（P2-7）：连上过再断 = 传输层中断（后端退出/网络断）→ unreachable；
    // 建连即被拒且 token 在手 = 鉴权拒绝 → unauthorized（走到这里 token 恒非空——
    // 后端没起时引导/刷新返回 null，建连前已被鉴权门拦下，不会匿名连接）。
    // 401 握手与连接拒绝的关闭码同为 1006，浏览器侧无从细分；以「token 是否刚经
    // 网络取得」为代理：鉴权端点活着才发得出 token，token 在手仍被拒指向鉴权问题。
    scheduleReconnect(handshakeOpened ? "unreachable" : "unauthorized");
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
  // 挂起中的鉴权等待随实例一并撤销：就绪回调不再触发建连
  authWaitOff?.();
  authWaitOff = null;
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
  return { connectWebSocket, retryNow, setupVisibilityChange, destroy, onWsReconnect, handleAuthReady, wsReconnecting, wsRetryCount, wsDisconnectReason };
}
