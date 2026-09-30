/** 外观旧偏好兼容、坏数据恢复与跨主题配色可读性测试。 */
import { describe, it, expect } from "vitest";
import { DEFAULT_APPEARANCE } from "./constants";
import { APPEARANCE_FIELDS, APPEARANCE_PRESETS, normalizeAppearance, normalizeCustomColors, themedBackground, readableColor, mixColor } from "./appearance";
import { contrastRatio, pickOnColor } from "./formatters";

describe("外观偏好校验", () => {
  it("旧偏好保留自定义字段并补齐阅读选项", () => {
    const result = normalizeAppearance({ theme: "dark", accent_color: "#abcdef", card_opacity: 0.7 });
    expect(result).toMatchObject({ theme: "dark", accent_color: "#abcdef", card_opacity: 0.7, font_scale: 1, reduce_motion: false });
  });
  it.each([null, [], "dark", 17])("无效顶层值 %s 回落默认", (value) => {
    expect(normalizeAppearance(value)).toEqual(DEFAULT_APPEARANCE);
  });
  it("拒绝错误类型、非有限值与任意 CSS，越界数字收敛", () => {
    expect(normalizeAppearance({ theme: "oops", accent_color: "red;", background_color: {}, card_opacity: "bad", card_blur: NaN, background_blur: 100, font_scale: -1, sidebar_opacity: 2, backdrop_filter: "false" }))
      .toEqual({ ...DEFAULT_APPEARANCE, background_blur: 30, sidebar_opacity: 1 });
  });
  it("分区字段不重叠且覆盖所有默认项，重置不会遗漏", () => {
    const fields = Object.values(APPEARANCE_FIELDS).flat();
    expect(new Set(fields).size).toBe(fields.length);
    expect([...fields].sort()).toEqual(Object.keys(DEFAULT_APPEARANCE).sort());
  });
  it("清理坏色板并按大小写去重", () => {
    expect(normalizeCustomColors({ accent: ["#ABCDEF", "#abcdef", null, "red"], bg: "oops", sidebar: ["#001122"] }))
      .toEqual({ accent: ["#abcdef"], bg: [], sidebar: ["#001122"], sidebar_accent: [] });
  });
  it("快捷风格不会替换图片、主题、选色和阅读偏好", () => {
    for (const preset of APPEARANCE_PRESETS) {
      expect(Object.keys(preset.values).every((key) => ["card_opacity", "backdrop_filter", "card_blur", "border_intensity", "sidebar_opacity"].includes(key))).toBe(true);
    }
  });
});

describe("配色可读性", () => {
  it.each(["#ffffff", "#000000", "#ffee00", "#8b5cf6", "#10b981", "#0891b2", "#808080"])("颜色 %s 的按钮文字选择较高对比色", (color) => {
    const selected = pickOnColor(color);
    expect(contrastRatio(color, selected)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(color, selected)).toBeGreaterThanOrEqual(contrastRatio(color, selected === "#ffffff" ? "#0f172a" : "#ffffff"));
  });
  it.each([true, false])("自定义黑白及鲜亮背景适配主题 %s", (isLight) => {
    for (const color of ["#ffffff", "#000000", "#ffff00", "#ecfdf5", "#111827"]) {
      const bg = themedBackground(color, isLight);
      expect(contrastRatio(bg, isLight ? "#000000" : "#ffffff")).toBeGreaterThanOrEqual(10);
      const text = readableColor("#22d3ee", bg, isLight ? "#000000" : "#ffffff");
      expect(contrastRatio(bg, text)).toBeGreaterThanOrEqual(4.5);
    }
  });
  it("透明导航以合成后的底色决定前景，不只检查所选原色", () => {
    const bg = mixColor("#ffffff", "#0f172a", 0.3);
    expect(pickOnColor(bg)).toBe("#ffffff");
    expect(contrastRatio(bg, readableColor("#ffffff", bg, pickOnColor(bg)))).toBeGreaterThanOrEqual(4.5);
  });
});
