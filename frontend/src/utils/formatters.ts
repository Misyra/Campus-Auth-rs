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

/** 秒 → "Xh Ym Zs" 开始监控时长（仪表盘统计卡） */
export function formatDuration(sec: number): string {
  if (sec === 0) return "0h 0m 0s";
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  return `${h}h ${m}m ${s}s`;
}

/** 当前本地时间 → "YYYY-MM-DD HH:mm:ss"（前端生成的日志条目用，与后端日志时间戳格式/时区一致） */
export function localNowTimestamp(): string {
  const d = new Date();
  const p = (n: number): string => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** ISO 时间戳 → "YYYY-MM-DD HH:mm:ss"（日志/历史列表展示用） */
export function formatTimestamp(ts: string): string {
  return (ts || "").replace("T", " ").substring(0, 19);
}

/** ISO 时间戳 → "MM-DD HH:mm"（统计卡等紧凑场景，避免换行） */
export function formatShortTime(ts: string): string {
  const full = formatTimestamp(ts);
  return full.length >= 16 ? full.substring(5, 16) : full;
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

/** WCAG 相对亮度（sRGB 通道线性化加权），用于判断底色深浅 */
export function relativeLuminance(hex: string): number {
  const rgb = hexToRgb(hex);
  if (!rgb) return 0;
  const lin = (c: number): number => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b);
}

/** 强调色底上的文字色：亮底配深字、深底配白字（阈值取两种默认主题强调色的分界） */
export function pickOnColor(hex: string): string {
  return relativeLuminance(hex) > 0.4 ? "#0f172a" : "#ffffff";
}
