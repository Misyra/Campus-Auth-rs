/** 校验外观应用到界面的实际变量，以及系统跟随和分区重置。 */
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { nextTick } from "vue";
import { DEFAULT_APPEARANCE, ACCENT_COLORS } from "../utils/constants";
import { contrastRatio } from "../utils/formatters";

vi.mock("../api", () => ({ backgroundApi: { remove: vi.fn(async () => {}) } }));
vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: vi.fn() }) }));

let app: ReturnType<typeof import("./useAppearance").useAppearance>;
let styles: Map<string, string>;
let classes: Set<string>;
let system: { matches: boolean; addEventListener: ReturnType<typeof vi.fn> };
let storage: { getItem: ReturnType<typeof vi.fn>; setItem: ReturnType<typeof vi.fn>; removeItem: ReturnType<typeof vi.fn> };

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  styles = new Map();
  classes = new Set();
  const attributes = new Map();
  const classList = { add: (name: string) => classes.add(name), remove: (name: string) => classes.delete(name), toggle: (name: string, on: boolean) => on ? classes.add(name) : classes.delete(name) };
  const style = { setProperty: (name: string, value: string) => styles.set(name, value), removeProperty: (name: string) => styles.delete(name) };
  vi.stubGlobal("document", { documentElement: { style, classList, setAttribute: (name: string, value: string) => attributes.set(name, value), getAttribute: (name: string) => attributes.get(name) }, body: { style, classList } });
  system = { matches: false, addEventListener: vi.fn() };
  vi.stubGlobal("window", { matchMedia: (query: string) => query.includes("color-scheme") ? system : { matches: false } });
  storage = { getItem: vi.fn(() => null), setItem: vi.fn(), removeItem: vi.fn() };
  vi.stubGlobal("localStorage", storage);
  app = (await import("./useAppearance")).useAppearance();
  app.applyAppearance();
});
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("外观实际应用", () => {
  it("深色模式选白色背景和导航时，卡片仍为深色、导航采用深字", async () => {
    Object.assign(app.appearance, { theme: "dark", background_color: "#ffffff", sidebar_color: "#ffffff", sidebar_opacity: 1 });
    await nextTick();
    expect(styles.get("--bg-card")).toContain("15, 23, 42");
    expect(styles.get("--navigation-text")).toBe("#0f172a");
    expect(styles.get("--bg-primary")).not.toBe("#ffffff");
  });
  it("边框跟随当前强调色，旋钮使用该底色的对比色", async () => {
    app.appearance.accent_color = "#ffff00";
    await nextTick();
    expect(styles.get("--border-accent")).toContain("255, 255, 0");
    expect(styles.get("--on-accent")).toBe("#0f172a");
    expect(styles.get("--toggle-knob-active")).toBe("var(--on-accent)");
  });
  it("系统主题变化会同步响应式主题及单色色块", async () => {
    app.appearance.theme = "auto";
    await nextTick();
    system.matches = true;
    system.addEventListener.mock.calls[0][1]();
    expect(app.getEffectiveTheme()).toBe("dark");
    expect(app.getResolvedAccent()).toBe("#ffffff");
  });
  it("字体放大与减少动效即时生效，存储受限仍能继续操作", async () => {
    storage.setItem.mockImplementation(() => { throw new Error("denied"); });
    Object.assign(app.appearance, { font_scale: 1.2, reduce_motion: true });
    await nextTick();
    expect(styles.get("--text-base")).toBe("16.8px");
    expect(classes.has("reduce-motion")).toBe(true);
    expect(app.storageAvailable.value).toBe(false);
  });
  it("卡片重置包含毛玻璃，背景图片不会被卡片重置清除", async () => {
    Object.assign(app.appearance, { backdrop_filter: true, card_blur: 24, background_url: "/api/background/example.jpg" });
    app.resetCard("card");
    await nextTick();
    expect(app.appearance.backdrop_filter).toBe(false);
    expect(app.appearance.card_blur).toBe(DEFAULT_APPEARANCE.card_blur);
    expect(app.appearance.background_url).toBe("/api/background/example.jpg");
    expect(app.cardDirty("card")).toBe(false);
  });
  it("外观持久化往返：保存的偏好在新实例读回（含主题与强调色）", async () => {
    Object.assign(app.appearance, { theme: "auto", accent_color: "#8b5cf6" });
    await nextTick();
    expect(app.storageAvailable.value).toBe(true);
    const saved = storage.setItem.mock.calls.at(-1)![1] as string;
    expect(JSON.parse(saved)).toMatchObject({ theme: "auto", accent_color: "#8b5cf6" });
    // 模拟重新打开页面：新模块实例从同一存储读回并经 normalizeAppearance 校验
    vi.resetModules();
    storage.getItem.mockImplementation(() => saved);
    const reloaded = (await import("./useAppearance")).useAppearance();
    expect(reloaded.appearance.theme).toBe("auto");
    expect(reloaded.appearance.accent_color).toBe("#8b5cf6");
  });
  it("强调色红线：全部预设 × 深浅两主题下，强调文字对画布、文字对强调底色的对比度 ≥ 4.5", async () => {
    // 视觉红线（主要文本组合）：--accent-text 由 readableColor 向前景色混合并保底
    // 4.5:1，--on-accent 由 pickOnColor 在深字/白字/纯黑中选择（不足时补纯黑，
    // 纯黑对任意中间亮度底色 > 4.5）。本测试把「浅色 × 6 预设」与「深色 × 6 预设」
    // 逐组合实际应用后复算对比度，防止重映射层或色板改动引入不可读组合
    // （预设含 MONO_ACCENT，按有效主题解析为黑/白一并覆盖）。
    // 注意：原始 --accent（边框/图标等装饰用途）不做 4.5 断言——青色等亮色在
    // 浅色画布上低于 3:1 是既有设计，装饰元素不适用文本对比度红线。
    for (const theme of ["light", "dark"] as const) {
      for (const preset of ACCENT_COLORS) {
        Object.assign(app.appearance, { theme, accent_color: preset.value });
        await nextTick();
        const accentText = styles.get("--accent-text")!;
        const accent = styles.get("--accent")!;
        const canvas = styles.get("--bg-primary")!;
        expect(contrastRatio(accentText, canvas), `${theme} × ${preset.value}：强调文字对画布`).toBeGreaterThanOrEqual(4.5);
        expect(contrastRatio(styles.get("--on-accent")!, accent), `${theme} × ${preset.value}：强调底色上的文字`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });
});
