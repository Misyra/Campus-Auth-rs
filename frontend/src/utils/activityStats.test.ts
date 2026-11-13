/**
 * activityStats 纯函数测试：事件数组 → 图表数据的映射逻辑。
 *
 * now 注入固定时刻（2026-11-13T10:00:00Z），断言不随运行日期漂移。
 */
import { describe, it, expect } from "vitest";
import {
  eventTypeDistribution,
  loginDailySeries,
  networkFlipSegments,
  utcDateKey,
} from "./activityStats";
import type { EventEnvelope } from "@/api/types";

const NOW = new Date("2026-11-13T10:00:00Z");

/** 构造事件封装（timestamp 必填，其余字段按需） */
function ev(type: EventEnvelope["type"], timestamp: string, extra: Partial<EventEnvelope> = {}): EventEnvelope {
  return { id: `${type}-${timestamp}-${Math.random()}`, timestamp, type, ...extra };
}

describe("utcDateKey", () => {
  it("取 UTC 日期部分（非本地时区）", () => {
    expect(utcDateKey(new Date("2026-11-13T23:30:00Z"))).toBe("2026-11-13");
    // UTC+8 的本地日期已是 14 日，UTC 仍是 13 日
    expect(utcDateKey(new Date("2026-11-13T16:00:00Z"))).toBe("2026-11-13");
  });
});

describe("loginDailySeries", () => {
  it("按 UTC 日期桶计数，缺数据日补零、窗口外事件忽略", () => {
    const events = [
      ev("login_succeeded", "2026-11-13T08:00:00Z"),
      ev("login_succeeded", "2026-11-13T09:00:00Z"),
      ev("login_failed", "2026-11-12T22:00:00Z"),
      // 窗口（近 3 天：11-11..11-13）之外
      ev("login_failed", "2026-11-08T10:00:00Z"),
      // 非登录事件不计数
      ev("network_up", "2026-11-13T07:00:00Z"),
    ];
    const series = loginDailySeries(events, 3, NOW);
    expect(series.map((d) => d.date)).toEqual(["2026-11-11", "2026-11-12", "2026-11-13"]);
    expect(series[0]).toEqual({ date: "2026-11-11", success: 0, failed: 0 });
    expect(series[1]).toEqual({ date: "2026-11-12", success: 0, failed: 1 });
    expect(series[2]).toEqual({ date: "2026-11-13", success: 2, failed: 0 });
  });

  it("空事件数组返回全零桶且桶数等于 days", () => {
    const series = loginDailySeries([], 7, NOW);
    expect(series).toHaveLength(7);
    expect(series.every((d) => d.success === 0 && d.failed === 0)).toBe(true);
  });
});

describe("networkFlipSegments", () => {
  it("无翻转时整窗 unknown", () => {
    const segs = networkFlipSegments([], 7, NOW);
    expect(segs).toEqual([{ start: 0, end: 1, state: "unknown" }]);
  });

  it("首个翻转前为 unknown 盲区，此后按翻转分段、末段延续到窗口末尾", () => {
    const events = [
      ev("network_down", "2026-11-13T02:00:00Z"),
      ev("network_up", "2026-11-13T04:00:00Z"),
      ev("network_down", "2026-11-13T06:00:00Z"),
    ];
    const segs = networkFlipSegments(events, 1, NOW);
    // 窗口 = [2026-11-13T00:00Z, 10:00Z]，span = 10h
    const frac = (h: number) => h / 10;
    expect(segs[0]).toEqual({ start: 0, end: frac(2), state: "unknown" });
    expect(segs[1]).toEqual({ start: frac(2), end: frac(4), state: "down" });
    expect(segs[2]).toEqual({ start: frac(4), end: frac(6), state: "up" });
    expect(segs[3]).toEqual({ start: frac(6), end: 1, state: "down" });
  });

  it("首刻即翻转则无 unknown 段；同刻连续翻转只保留最终态", () => {
    const events = [
      ev("network_up", "2026-11-13T00:00:00Z"),
      // 同刻 down→up：零宽 down 段被吞掉
      ev("network_down", "2026-11-13T05:00:00Z"),
      ev("network_up", "2026-11-13T05:00:00Z"),
    ];
    const segs = networkFlipSegments(events, 1, NOW);
    const states = segs.map((s) => s.state);
    expect(states).toEqual(["up", "up"]);
    // 全部非零宽且首段从 0 起
    for (const s of segs) expect(s.end).toBeGreaterThan(s.start);
    expect(segs[0].start).toBe(0);
    expect(segs[segs.length - 1].end).toBe(1);
  });

  it("窗口外、未来时刻与非法时间戳事件被忽略", () => {
    const events = [
      ev("network_up", "2026-11-01T00:00:00Z"),
      ev("network_down", "not-a-time"),
      // 未来时刻（时钟偏差兜底）：窗口右端之外
      ev("network_down", "2026-11-13T23:00:00Z"),
      ev("network_up", "2026-11-13T01:00:00Z"),
    ];
    const segs = networkFlipSegments(events, 1, NOW);
    expect(segs).toEqual([
      { start: 0, end: 0.1, state: "unknown" },
      { start: 0.1, end: 1, state: "up" },
    ]);
  });
});

describe("eventTypeDistribution", () => {
  it("按计数降序、并列按类型名升序，只含出现过的类型", () => {
    const events = [
      ev("login_failed", "2026-11-13T01:00:00Z"),
      ev("login_failed", "2026-11-13T02:00:00Z"),
      ev("network_up", "2026-11-13T03:00:00Z"),
      ev("network_down", "2026-11-13T04:00:00Z"),
      ev("network_up", "2026-11-13T05:00:00Z"),
      ev("task_failed", "2026-11-13T06:00:00Z"),
    ];
    const rows = eventTypeDistribution(events);
    // 并列（2/2、1/1）按类型名升序
    expect(rows.map((r) => r.type)).toEqual(["login_failed", "network_up", "network_down", "task_failed"]);
    expect(rows.map((r) => r.count)).toEqual([2, 2, 1, 1]);
  });

  it("空事件数组返回空分布", () => {
    expect(eventTypeDistribution([])).toEqual([]);
  });
});
