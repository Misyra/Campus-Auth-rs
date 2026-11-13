/**
 * 活动概览统计：GET /api/events 事件数组 → 三类图表数据（纯函数）。
 *
 * 日期分组与后端 JSONL 落盘命名同口径按 **UTC 日期**（事件 timestamp 的前
 * 10 位），前端不做时区换算——与落盘文件边界一致，图表桶与数据文件一一对应。
 * `now` 参数供测试注入固定时刻，生产调用缺省取当前时间。
 */
import type { AppEventType, EventEnvelope } from "@/api/types";

/** 单日登录成败计数 */
export interface LoginDailyCount {
  /** UTC 日期（YYYY-MM-DD） */
  date: string;
  /** 当日登录成功次数 */
  success: number;
  /** 当日登录失败次数 */
  failed: number;
}

/** 网络状态时间段（占回看窗口的比例 0..1） */
export interface NetworkSegment {
  /** 段起点（占窗口比例） */
  start: number;
  /** 段终点（占窗口比例） */
  end: number;
  /** 段内状态；unknown 表示首个翻转事件之前无观测（不推断状态） */
  state: "up" | "down" | "unknown";
}

/** 事件类型计数行 */
export interface TypeCount {
  type: AppEventType;
  count: number;
}

/** 一天的毫秒数（固定值：UTC 日界无夏令时偏移） */
const DAY_MS = 86_400_000;

/** Date → UTC 日期键（YYYY-MM-DD） */
export function utcDateKey(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** 最近 `days` 天（UTC，含今日）逐日登录成败计数，时间升序、缺数据日补零 */
export function loginDailySeries(
  events: EventEnvelope[],
  days: number,
  now: Date = new Date(),
): LoginDailyCount[] {
  // 桶按今日 UTC 零点向回推 days-1 天（与后端「含今日共 N 天」窗口一致）
  const todayUtcMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
  const buckets: LoginDailyCount[] = [];
  for (let i = days - 1; i >= 0; i--) {
    buckets.push({ date: utcDateKey(new Date(todayUtcMs - i * DAY_MS)), success: 0, failed: 0 });
  }
  const index = new Map(buckets.map((b, i) => [b.date, i]));
  for (const e of events) {
    if (e.type !== "login_succeeded" && e.type !== "login_failed") continue;
    const i = index.get(e.timestamp.slice(0, 10));
    if (i === undefined) continue;
    if (e.type === "login_succeeded") buckets[i].success += 1;
    else buckets[i].failed += 1;
  }
  return buckets;
}

/**
 * 网络状态翻转时间线：`network_up` / `network_down` 事件 → 窗口内状态段。
 *
 * 窗口为 [今日-(days-1) 零点 UTC, now]。首个翻转事件之前无观测，渲染为
 * unknown 段（不推断状态）；此后每次翻转开启新段，末段延续到窗口末尾。
 * 同刻连续翻转（零宽段）与同态重复事件自动合并。无任何翻转时整窗为 unknown。
 */
export function networkFlipSegments(
  events: EventEnvelope[],
  days: number,
  now: Date = new Date(),
): NetworkSegment[] {
  const nowMs = now.getTime();
  const todayUtcMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
  const windowStartMs = todayUtcMs - (days - 1) * DAY_MS;
  const span = Math.max(nowMs - windowStartMs, 1);
  // 比例坐标（0..1），同刻/越界翻转钳到窗内
  const frac = (t: number) => Math.min(Math.max((t - windowStartMs) / span, 0), 1);
  const flips = events
    .filter((e) => e.type === "network_up" || e.type === "network_down")
    .map((e) => ({ t: Date.parse(e.timestamp), state: e.type === "network_up" ? ("up" as const) : ("down" as const) }))
    .filter((f) => Number.isFinite(f.t) && f.t >= windowStartMs && f.t <= nowMs)
    .sort((a, b) => a.t - b.t);

  const segments: NetworkSegment[] = [];
  if (flips.length === 0) {
    return [{ start: 0, end: 1, state: "unknown" }];
  }
  // 首个翻转前的观测盲区
  const headEnd = frac(flips[0].t);
  if (headEnd > 0) segments.push({ start: 0, end: headEnd, state: "unknown" });
  let state: "up" | "down" | null = null;
  let pos = 0;
  for (const f of flips) {
    const p = frac(f.t);
    // 零宽段丢弃：同刻连续翻转只保留最终态
    if (state !== null && p > pos) segments.push({ start: pos, end: p, state });
    state = f.state;
    pos = p;
  }
  // 末段延续到窗口末尾（最后一次翻转后的状态保持至今）
  if (state !== null && pos < 1) segments.push({ start: pos, end: 1, state });
  return segments;
}

/**
 * 事件类型分布：按计数降序（并列时类型名升序保证稳定），只含出现过的类型。
 */
export function eventTypeDistribution(events: EventEnvelope[]): TypeCount[] {
  const counts = new Map<AppEventType, number>();
  for (const e of events) {
    counts.set(e.type, (counts.get(e.type) ?? 0) + 1);
  }
  return Array.from(counts, ([type, count]) => ({ type, count })).sort(
    (a, b) => b.count - a.count || a.type.localeCompare(b.type),
  );
}
