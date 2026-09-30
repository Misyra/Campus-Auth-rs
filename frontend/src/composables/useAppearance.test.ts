/** 校验外观应用到界面的实际变量，以及系统跟随和分区重置。 */
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { nextTick } from "vue";
import { DEFAULT_APPEARANCE } from "../utils/constants";

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
});
