/**
 * 格式化与颜色工具函数。
 * 从 legacy js/methods/formatters.js 迁移，补充类型标注。
 */

import { LOG_SOURCES } from "./constants";

/** 秒 → HH:MM:SS */
export function formatDuration(totalSeconds: number): string {
  const sec = Number(totalSeconds || 0);
  const h = String(Math.floor(sec / 3600)).padStart(2, "0");
  const m = String(Math.floor((sec % 3600) / 60)).padStart(2, "0");
  const s = String(sec % 60).padStart(2, "0");
  return `${h}:${m}:${s}`;
}

/** ISO 时间 → YYYY-MM-DD HH:MM:SS */
export function formatTime(isoString: string): string {
  if (!isoString) return "-";
  return isoString.replace("T", " ").substring(0, 19);
}

/** 时间戳 → HH:MM:SS */
export function extractScreenshotUrl(message: string): string {
  const text = String(message || "");
  const match = text.match(
    /截图[:：]\s*(\/(?:logs|debug|temp)\/\S+\.(?:png|jpg|jpeg|webp|gif))/i,
  );
  if (!match) return "";
  const url = match[1];
  if (url.includes("..") || /[\x00-\x1f]/.test(url)) return "";
  return url;
}

/** 去除日志消息中的截图提示文本 */
export function stripScreenshotHint(message: string): string {
  const text = String(message || "");
  return text
    .replace(/\s*[\[(]?\s*截图[:：]\s*\/(?:logs|debug|temp)\/\S+\.(?:png|jpg|jpeg|webp|gif)\s*[\])]?/gi, "")
    .trim();
}

/** 日志来源 → 展示标签 */
export function getSourceLabel(source: string): string {
  return LOG_SOURCES.find((s) => s.value === source)?.label || source || "未知";
}

/** 定时任务调度时间 → HH:MM */
export function formatScheduleTime(schedule: { hour?: number; minute?: number } | null | undefined): string {
  if (!schedule) return "";
  const hour = String(schedule.hour ?? 0).padStart(2, "0");
  const minute = String(schedule.minute ?? 0).padStart(2, "0");
  return `${hour}:${minute}`;
}

/** 秒 → 分钟/秒文本 */
export function formatTimeValue(seconds: number): string {
  if (!seconds) return "-";
  if (seconds < 60) return `${seconds}秒`;
  return `${Math.round(seconds / 60)}分钟`;
}

/** HEX → RGB */
export function hexToRgb(hex: string): { r: number; g: number; b: number } | null {
  let h = (hex || "").replace("#", "");
  if (h.length === 3)
    h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
  const result = /^([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(h);
  return result
    ? {
        r: parseInt(result[1], 16),
        g: parseInt(result[2], 16),
        b: parseInt(result[3], 16),
      }
    : null;
}

/** 调整颜色亮度（正数变亮，负数变暗） */
export function adjustColor(hex: string, amount: number): string {
  let h = hex.replace("#", "");
  if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
  const num = parseInt(h, 16);
  const r = Math.max(0, Math.min(255, (num >> 16) + amount));
  const g = Math.max(0, Math.min(255, ((num >> 8) & 0x00ff) + amount));
  const b = Math.max(0, Math.min(255, (num & 0x0000ff) + amount));
  return `#${(0x1000000 | (r << 16) | (g << 8) | b).toString(16).slice(1)}`;
}
