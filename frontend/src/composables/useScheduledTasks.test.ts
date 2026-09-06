import { describe, expect, it } from "vitest";
import { parseCronToSchedule } from "./useScheduledTasks";

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
