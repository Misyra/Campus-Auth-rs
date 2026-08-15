/**
 * 格式化与颜色工具函数。
 * 从 legacy js/methods/formatters.js 迁移，补充类型标注。
 */

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
