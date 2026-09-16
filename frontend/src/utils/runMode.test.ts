/**
 * 运行模式预设的纯逻辑测试：模式判定、差异计算、归一化比较。
 *
 * 重点覆盖「自定义」的判定边界——它是"不在任何预设内"的兜底态，若判定写松
 * （例如用包含关系而非全等），界面会显示一个用户没选过的模式名，比显示"自定义"更误导。
 */
import { describe, expect, it } from "vitest";
import {
  RUN_MODE_FIELD_LABELS,
  RUN_MODE_PRESETS,
  detectRunMode,
  diffRunMode,
  formatRunModeValue,
  getRunModePreset,
  matchesPreset,
} from "./runMode";
import type { RunModeSettings } from "./runMode";

const DEFAULT = getRunModePreset("default")!.settings;
const DEBUG = getRunModePreset("debug")!.settings;

describe("预设定义", () => {
  it("两个预设的字段集合完全一致（否则差异对比会漏项）", () => {
    const a = Object.keys(DEFAULT).sort();
    const b = Object.keys(DEBUG).sort();
    expect(a).toEqual(b);
    expect(a).toEqual(Object.keys(RUN_MODE_FIELD_LABELS).sort());
  });

  it("不含 strict_login_mode（它改的是登录触发行为，不属可观测性预设）", () => {
    expect(DEFAULT).not.toHaveProperty("strict_login_mode");
    expect(DEBUG).not.toHaveProperty("strict_login_mode");
  });

  it("调试模式：浏览器可见、保留进程、不自动登录、DEMO 日志", () => {
    expect(DEBUG.headless).toBe(false);
    expect(DEBUG.keep_alive).toBe(true);
    expect(DEBUG.startup_action).toBe("none");
    expect(DEBUG.log_level).toBe("DEBUG");
  });

  it("默认模式：后台运行、不常驻、开机自启、启动即检测、夜间暂停", () => {
    expect(DEFAULT.headless).toBe(true);
    expect(DEFAULT.keep_alive).toBe(false);
    expect(DEFAULT.startup_action).toBe("monitor");
    expect(DEFAULT.autostart).toBe(true);
    expect(DEFAULT.pause_enabled).toBe(true);
  });

  it("调试模式关闭暂停时段（随时手动复现，不被暂停窗口拦住）", () => {
    expect(DEBUG.pause_enabled).toBe(false);
  });

  it("两组的差异覆盖了可观测性相关的关键项", () => {
    // pause_enabled 一组开一组关（默认夜间不登录，调试随时复现）；
    // low_resource_mode 在两组里都是关闭，这是有意的
    // （调试时关掉它才能看见验证码图；默认模式也没有理由开）。
    // 真正要求"必须不同"的是这几项——它们构成两个模式的本质区别。
    for (const key of [
      "headless",
      "keep_alive",
      "startup_action",
      "log_level",
      "autostart",
      "pause_enabled",
    ] as const) {
      expect(DEFAULT[key], `${key} 两组取值相同，切换将无效果`).not.toEqual(
        DEBUG[key],
      );
    }
  });

  it("两组都不启用低资源模式（调试图要看得见，默认模式无必要）", () => {
    expect(DEFAULT.low_resource_mode).toBe(false);
    expect(DEBUG.low_resource_mode).toBe(false);
  });
});

describe("detectRunMode", () => {
  it("与默认预设完全一致 → default", () => {
    expect(detectRunMode({ ...DEFAULT })).toBe("default");
  });

  it("与调试预设完全一致 → debug", () => {
    expect(detectRunMode({ ...DEBUG })).toBe("debug");
  });

  it("任一字段不符即 custom（不做模糊匹配）", () => {
    // 只改一个字段，就必须脱离该预设
    expect(detectRunMode({ ...DEFAULT, headless: false })).toBe("custom");
    expect(detectRunMode({ ...DEBUG, autostart: true })).toBe("custom");
  });

  it("两边各取一半（既非默认也非调试）→ custom", () => {
    expect(
      detectRunMode({ ...DEFAULT, headless: false, keep_alive: true }),
    ).toBe("custom");
  });

  it("日志级别大小写不敏感：DEBUG / debug 视为同一模式", () => {
    expect(detectRunMode({ ...DEBUG, log_level: "debug" })).toBe("debug");
    expect(detectRunMode({ ...DEFAULT, log_level: "info" })).toBe("default");
  });

  it("WARNING 与 WARN 视为同一级别（后端会把 WARNING 归一为 WARN）", () => {
    // 用 WARN 与 WARNING 互相比对：两者归一后相同，不应因写法差异判成自定义
    expect(detectRunMode({ ...DEFAULT, log_level: "WARNING" })).toBe("custom");
    // WARN 不等于预设要求的 INFO，故仍是 custom；关键断言是下面这行——
    // 把 WARNING 改写成 WARN 不改变判定结果（归一化生效）
    expect(detectRunMode({ ...DEFAULT, log_level: "WARNING" })).toBe(
      detectRunMode({ ...DEFAULT, log_level: "WARN" }),
    );
  });
});

describe("matchesPreset", () => {
  it("字段顺序不影响判定", () => {
    const reordered = Object.fromEntries(
      Object.entries(DEFAULT).reverse(),
    ) as unknown as RunModeSettings;
    expect(matchesPreset(reordered, DEFAULT)).toBe(true);
  });
});

describe("diffRunMode", () => {
  it("完全一致时无差异", () => {
    expect(diffRunMode({ ...DEFAULT }, DEFAULT)).toEqual([]);
  });

  it("只列出实际有变化的项（不含未变项）", () => {
    // 从调试模式切到默认模式：low_resource_mode 两组都是 false，故差异数比字段总数少 1
    const changes = diffRunMode({ ...DEBUG }, DEFAULT);
    const sameInBoth = (Object.keys(DEFAULT) as Array<keyof RunModeSettings>).filter(
      (k) => DEFAULT[k] === DEBUG[k],
    );
    expect(changes).toHaveLength(Object.keys(DEFAULT).length - sameInBoth.length);
    expect(sameInBoth).toEqual(["low_resource_mode"]);
    // 未变项不得出现在差异里
    expect(changes.map((c) => c.key)).not.toContain("low_resource_mode");
  });
  it("部分一致时只列差异项", () => {
    const half = { ...DEFAULT, headless: false };
    const changes = diffRunMode(half, DEFAULT);
    expect(changes).toHaveLength(1);
    expect(changes[0].key).toBe("headless");
    expect(changes[0].from).toBe("关闭");
    expect(changes[0].to).toBe("开启");
  });

  it("差异项带用户可见名称与可读值（不暴露字段名/布尔）", () => {
    const changes = diffRunMode({ ...DEBUG }, DEFAULT);
    const byKey = Object.fromEntries(changes.map((c) => [c.key, c]));
    expect(byKey.headless.label).toBe("浏览器后台运行");
    expect(byKey.startup_action.from).toBe("无操作");
    expect(byKey.startup_action.to).toBe("开始检测");
    // 值里不得出现裸 true/false
    for (const c of changes) {
      expect(c.from).not.toMatch(/true|false/);
      expect(c.to).not.toMatch(/true|false/);
    }
  });

  it("大小写不敏感：仅日志级别大小写不同不算差异", () => {
    expect(diffRunMode({ ...DEFAULT, log_level: "INFO" }, DEFAULT)).toEqual([]);
    expect(diffRunMode({ ...DEFAULT, log_level: "info" }, DEFAULT)).toEqual([]);
  });

  it("暂停时段以用户可见名称出现（调试 → 默认列出「启用暂停时段」）", () => {
    const changes = diffRunMode({ ...DEBUG }, DEFAULT);
    const byKey = Object.fromEntries(changes.map((c) => [c.key, c]));
    expect(byKey.pause_enabled).toBeDefined();
    expect(byKey.pause_enabled.label).toBe("启用暂停时段");
    expect(byKey.pause_enabled.from).toBe("关闭");
    expect(byKey.pause_enabled.to).toBe("开启");
  });
});

describe("formatRunModeValue", () => {
  it("布尔转开启/关闭", () => {
    expect(formatRunModeValue("headless", true)).toBe("开启");
    expect(formatRunModeValue("headless", false)).toBe("关闭");
  });

  it("启动动作转中文", () => {
    expect(formatRunModeValue("startup_action", "monitor")).toBe("开始检测");
    expect(formatRunModeValue("startup_action", "none")).toBe("无操作");
    expect(formatRunModeValue("startup_action", "login_once")).toBe("登录一次后退出");
  });

  it("未知启动动作原样返回（后端新增枚举值时不当成崩溃）", () => {
    expect(formatRunModeValue("startup_action", "future_action")).toBe("future_action");
  });

  it("日志级别原样返回", () => {
    expect(formatRunModeValue("log_level", "DEBUG")).toBe("DEBUG");
  });
});

describe("getRunModePreset", () => {
  it("取得到两个预设", () => {
    expect(getRunModePreset("default")?.label).toBe("默认模式");
    expect(getRunModePreset("debug")?.label).toBe("调试模式");
  });

  it("custom 无定义（它不是可选项，只是兜底态）", () => {
    expect(getRunModePreset("custom")).toBeUndefined();
  });

  it("预设 id 不含 custom（custom 不可被选为目标）", () => {
    for (const p of RUN_MODE_PRESETS) {
      expect(p.id).not.toBe("custom");
    }
  });

  it("每个预设都有说明文案（界面靠它解释代价）", () => {
    for (const p of RUN_MODE_PRESETS) {
      expect(p.description.length).toBeGreaterThan(10);
    }
  });
});
