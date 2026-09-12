import { describe, expect, it } from "vitest";
import { clampStartupForm, parseCronToSchedule, STARTUP_FORM_LIMITS } from "./useScheduledTasks";

describe("parseCronToSchedule", () => {
  it("解析标准每日时间表达式", () => {
    expect(parseCronToSchedule("30 8 * * *")).toEqual({
      hour: 8,
      minute: 30,
      valid: true,
    });
  });

  it("非每日表达式（步进/区间/列表）标记 invalid 并回退 08:00", () => {
    for (const cron of ["*/5 * * * *", "0 8-18 * * *", "0 8,12 * * *"]) {
      const r = parseCronToSchedule(cron);
      expect(r.valid, cron).toBe(false);
      expect(r.hour, cron).toBe(8);
      expect(r.minute, cron).toBe(0);
    }
  });

  it("空串与垃圾输入标记 invalid", () => {
    expect(parseCronToSchedule("").valid).toBe(false);
    expect(parseCronToSchedule("abc").valid).toBe(false);
    expect(parseCronToSchedule("not a cron at all").valid).toBe(false);
  });
});

describe("clampStartupForm", () => {
  it("合法值原样保留", () => {
    expect(clampStartupForm({ max_runs_per_day: 2, max_retries: 3, startup_delay_secs: 45 })).toEqual({
      max_runs_per_day: 2,
      max_retries: 3,
      startup_delay_secs: 45,
    });
  });

  it("NaN/非数字回退缺省值（1/2/30）", () => {
    expect(clampStartupForm({ max_runs_per_day: NaN, max_retries: NaN, startup_delay_secs: NaN })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.fallback,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.fallback,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.fallback,
    });
  });

  it("越界值钳制到区间边界", () => {
    expect(clampStartupForm({ max_runs_per_day: 0, max_retries: 999, startup_delay_secs: -5 })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.min,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.max,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.min,
    });
    expect(clampStartupForm({ max_runs_per_day: 1000, max_retries: 99, startup_delay_secs: 999999 })).toEqual({
      max_runs_per_day: STARTUP_FORM_LIMITS.maxRunsPerDay.max,
      max_retries: STARTUP_FORM_LIMITS.maxRetries.max,
      startup_delay_secs: STARTUP_FORM_LIMITS.startupDelaySecs.max,
    });
  });

  it("小数四舍五入为整数", () => {
    expect(clampStartupForm({ max_runs_per_day: 2.6, max_retries: 1.2, startup_delay_secs: 30.5 }).max_runs_per_day).toBe(3);
  });
});
