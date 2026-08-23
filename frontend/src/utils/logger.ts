/**
 * 前端日志器。
 * 从 legacy js/logger.js 迁移：控制台输出 + 通过 WebSocket 上报前端日志。
 * 单例导出，供全局使用。
 */

import { LEVEL_VALUES, LIMITS } from "./constants";

type LogLevel = "DEBUG" | "INFO" | "WARNING" | "ERROR";

interface FrontendLogMessage {
  level: string;
  scope: string;
  message: string;
  meta: unknown;
}

class FrontendLogger {
  private currentLevel = "INFO";
  private ws: WebSocket | null = null;
  private buffer: FrontendLogMessage[] = [];

  setWebSocket(ws: WebSocket | null): void {
    this.ws = ws;
    this.flushBuffer();
  }

  setLevel(level: string): void {
    const next = String(level || "").toUpperCase();
    this.currentLevel = LEVEL_VALUES[next] !== undefined && LEVEL_VALUES[next] >= 0 ? next : "INFO";
    // eslint-disable-next-line no-console
    console.info(...this.format("INFO", "logger", `frontend log level => ${this.currentLevel}`));
  }

  private shouldLog(level: string): boolean {
    const left = LEVEL_VALUES[String(level || "").toUpperCase()] ?? LEVEL_VALUES.INFO;
    const right = LEVEL_VALUES[this.currentLevel] ?? LEVEL_VALUES.INFO;
    return left >= right;
  }

  private format(level: LogLevel, scope: string, message: string, meta?: unknown): unknown[] {
    const stamp = new Date().toISOString();
    return [stamp, level, "FRONTEND", scope, message, meta ?? ""];
  }

  private send(level: string, scope: string, message: string, meta?: unknown): void {
    const payload: FrontendLogMessage = { level, scope, message, meta: meta ?? "" };
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
