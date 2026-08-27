/**
 * 格式化与颜色工具函数的单元测试。
 * 重点覆盖被界面直接消费的边界值（0、缺字段、非法输入）与
 * pickOnColor 的亮度阈值（两种默认主题强调色必须落在正确一侧）。
 */
import { describe, it, expect } from "vitest";
import {
  formatScheduleTime,
  formatTimeValue,
  formatDuration,
  formatTimestamp,
  formatShortTime,
  hexToRgb,
  adjustColor,
  relativeLuminance,
  pickOnColor,
} from "./formatters";

describe("formatScheduleTime", () => {
  it("空调度返回空串", () => {
    expect(formatScheduleTime(null)).toBe("");
    expect(formatScheduleTime(undefined)).toBe("");
  });

  it("小时与分钟补零到两位", () => {
    expect(formatScheduleTime({ hour: 9, minute: 5 })).toBe("09:05");
    expect(formatScheduleTime({ hour: 23, minute: 59 })).toBe("23:59");
  });

  it("缺失字段按 0 处理", () => {
    expect(formatScheduleTime({})).toBe("00:00");
  });
});

describe("formatTimeValue", () => {
  it("0 与 undefined 返回占位符", () => {
    expect(formatTimeValue(0)).toBe("-");
  });

  it("一分钟内按秒展示", () => {
    expect(formatTimeValue(30)).toBe("30秒");
    expect(formatTimeValue(59)).toBe("59秒");
  });

  it("超过一分钟向上取整为分钟", () => {
    expect(formatTimeValue(60)).toBe("1分钟");
    expect(formatTimeValue(90)).toBe("2分钟");
  });
});

describe("formatDuration", () => {
  it("0 秒显示全零", () => {
    expect(formatDuration(0)).toBe("0h 0m 0s");
  });

  it("时分秒拆分", () => {
    expect(formatDuration(3725)).toBe("1h 2m 5s");
    expect(formatDuration(3600)).toBe("1h 0m 0s");
  });
});

describe("formatTimestamp", () => {
  it("ISO 时间转为空格分隔并截断到秒", () => {
    expect(formatTimestamp("2026-08-26T10:20:30.123Z")).toBe("2026-08-26 10:20:30");
  });

  it("空值安全", () => {
    expect(formatTimestamp("")).toBe("");
  });
});

describe("formatShortTime", () => {
  it("截取月日 + 时分", () => {
    expect(formatShortTime("2026-08-26T17:35:32")).toBe("08-26 17:35");
  });

  it("过短输入原样返回", () => {
    expect(formatShortTime("")).toBe("");
  });
});

describe("hexToRgb", () => {
  it("解析六位 HEX", () => {
    expect(hexToRgb("#22d3ee")).toEqual({ r: 34, g: 211, b: 238 });
  });

  it("解析三位简写", () => {
    expect(hexToRgb("#abc")).toEqual({ r: 170, g: 187, b: 204 });
  });

  it("非法输入返回 null", () => {
    expect(hexToRgb("not-a-color")).toBeNull();
    expect(hexToRgb("#12345")).toBeNull();
  });
});

describe("adjustColor", () => {
  it("正向调亮（三通道同加）", () => {
    expect(adjustColor("#000000", 10)).toBe("#0a0a0a");
  });

  it("负向调暗并钳制到 0", () => {
    expect(adjustColor("#0a0a0a", -20)).toBe("#000000");
  });

  it("钳制到 255", () => {
    expect(adjustColor("#ffffff", 30)).toBe("#ffffff");
  });
});

describe("relativeLuminance / pickOnColor", () => {
  it("黑白两端", () => {
    expect(relativeLuminance("#ffffff")).toBeCloseTo(1);
    expect(relativeLuminance("#000000")).toBeCloseTo(0);
  });

  it("非法输入按全黑处理", () => {
    expect(relativeLuminance("not-a-color")).toBe(0);
  });

  it("深色主题默认强调色（亮青）配深字", () => {
    expect(pickOnColor("#22d3ee")).toBe("#0f172a");
  });

  it("浅色主题默认强调色（深青）配白字", () => {
    expect(pickOnColor("#0891b2")).toBe("#ffffff");
  });
});
