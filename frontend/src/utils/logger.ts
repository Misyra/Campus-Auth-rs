/**
 * 前端日志器。
 * 从 legacy js/logger.js 迁移：控制台输出 + 通过 WebSocket 上报前端日志。
 * 单例导出，供全局使用。
 */

import { LEVEL_VALUES, LIMITS } from "./constants";
import { localNowTimestamp } from "./formatters";

type LogLevel = "DEBUG" | "INFO" | "WARNING" | "ERROR";

interface FrontendLogMessage {
  level: string;
  scope: string;
  message: string;
  meta: unknown;
}

/**
 * WS 上报前的 meta 序列化。
 * Error（含 ApiError）的 message/stack 是不可枚举属性，直接 JSON.stringify
 * 只得到 {"detail":{},"name":"ApiError"} 之类的空壳（线上曾因此出现 meta={}，
 * 导致组件异常堆栈丢失无法定位）。此处转成普通对象，保留 name/message、
 * 自身可枚举字段（如 status/code/detail/aborted）与 stack（放最后，
 * 后端超长截断时优先保留 message）。控制台输出仍用原始对象
 * （devtools 可展开堆栈），仅上报链路做转换。
 */
function serializeMeta(meta: unknown): unknown {
  if (meta instanceof Error) {
    const out: Record<string, unknown> = { name: meta.name, message: meta.message };
    for (const key of Object.keys(meta)) {
      out[key] = (meta as unknown as Record<string, unknown>)[key];
    }
    if (meta.stack) out.stack = meta.stack;
    return out;
  }
  return meta;
}

class FrontendLogger {
  private currentLevel = "INFO";
  private ws: WebSocket | null = null;
  private buffer: FrontendLogMessage[] = [];
  /** 同 scope+message 的 WS 上行节流窗口（错误风暴等量回流会刷爆日志面板） */
  private static readonly THROTTLE_WINDOW_MS = 5000;
  /** 节流表上限：超过先清理过期键，仍超则整体清空（最坏代价是清空后一轮重复） */
  private static readonly THROTTLE_MAP_MAX = 200;
  private lastSentAt = new Map<string, number>();

  setWebSocket(ws: WebSocket | null): void {
    this.ws = ws;
    this.flushBuffer();
  }

  setLevel(level: string): void {
    const next = String(level || "").toUpperCase();
    this.currentLevel = LEVEL_VALUES[next] !== undefined && LEVEL_VALUES[next] >= 0 ? next : "INFO";
    // eslint-disable-next-line no-console
    console.info(...this.format("INFO", "logger", `前端日志级别已切换为 ${this.currentLevel}`));
  }

  private shouldLog(level: string): boolean {
    const left = LEVEL_VALUES[String(level || "").toUpperCase()] ?? LEVEL_VALUES.INFO;
    const right = LEVEL_VALUES[this.currentLevel] ?? LEVEL_VALUES.INFO;
    return left >= right;
  }

  private format(level: LogLevel, scope: string, message: string, meta?: unknown): unknown[] {
    const stamp = localNowTimestamp();
    return [stamp, level, "FRONTEND", scope, message, meta ?? ""];
  }

  /**
   * WS 上行节流：5 秒窗口内相同 scope+message 只上行一次。
   * 仅约束上报链路，console 输出不受限（保留完整开发信息）。
   */
  private isThrottled(scope: string, message: string): boolean {
    const now = Date.now();
    if (this.lastSentAt.size > FrontendLogger.THROTTLE_MAP_MAX) {
      for (const [key, at] of this.lastSentAt) {
        if (now - at >= FrontendLogger.THROTTLE_WINDOW_MS) this.lastSentAt.delete(key);
      }
      if (this.lastSentAt.size > FrontendLogger.THROTTLE_MAP_MAX) this.lastSentAt.clear();
    }
    const key = `${scope}\u0000${message}`;
    const at = this.lastSentAt.get(key);
    if (at !== undefined && now - at < FrontendLogger.THROTTLE_WINDOW_MS) return true;
    this.lastSentAt.set(key, now);
    return false;
  }

  private send(level: string, scope: string, message: string, meta?: unknown): void {
    if (this.isThrottled(scope, message)) return;
    // 无 meta 时发 null：后端按 null 省略 meta 字段，避免日志里出现 meta="" 噪音
    const payload: FrontendLogMessage = {
      level,
      scope,
      message,
      meta: meta === undefined || meta === "" ? null : serializeMeta(meta),
    };
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      try {
        this.ws.send(JSON.stringify({ type: "frontend_log", data: payload }));
        return;
      } catch {
        // WebSocket 可能在 readyState 检查后瞬间关闭，不能静默丢弃这条日志。
      }
    }
    this.buffer.push(payload);
    if (this.buffer.length > LIMITS.WS_LOG_BUFFER_MAX) this.buffer.shift();
  }

  private flushBuffer(): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN || this.buffer.length === 0) return;
    const socket = this.ws;
    const batch = this.buffer.splice(0, this.buffer.length);
    let sent = 0;
    try {
      for (const msg of batch) {
        socket.send(JSON.stringify({ type: "frontend_log", data: msg }));
        sent += 1;
      }
    } catch {
      // 已成功发送的前缀不能重复入队，只恢复尚未发送的尾部。
      this.buffer.unshift(...batch.slice(sent));
      if (this.buffer.length > LIMITS.WS_LOG_BUFFER_MAX) {
        this.buffer.splice(0, this.buffer.length - LIMITS.WS_LOG_BUFFER_MAX);
      }
    }
  }

  debug(scope: string, message: string, meta?: unknown): void {
    if (this.shouldLog("DEBUG")) {
      // eslint-disable-next-line no-console
      console.debug(...this.format("DEBUG", scope, message, meta));
      this.send("DEBUG", scope, message, meta);
    }
  }

  info(scope: string, message: string, meta?: unknown): void {
    if (this.shouldLog("INFO")) {
      // eslint-disable-next-line no-console
      console.info(...this.format("INFO", scope, message, meta));
      this.send("INFO", scope, message, meta);
    }
  }

  warn(scope: string, message: string, meta?: unknown): void {
    if (this.shouldLog("WARNING")) {
      // eslint-disable-next-line no-console
      console.warn(...this.format("WARNING", scope, message, meta));
      this.send("WARNING", scope, message, meta);
    }
  }

  error(scope: string, message: string, meta?: unknown): void {
    if (this.shouldLog("ERROR")) {
      // eslint-disable-next-line no-console
      console.error(...this.format("ERROR", scope, message, meta));
      this.send("ERROR", scope, message, meta);
    }
  }
}

/** 全局单例 */
export const frontendLogger = new FrontendLogger();
