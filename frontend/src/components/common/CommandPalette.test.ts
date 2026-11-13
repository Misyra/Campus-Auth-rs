// @vitest-environment happy-dom
/**
 * 命令面板行为层。同 Modal.focus.test.ts 范式：文件级 happy-dom 环境覆盖全局
 * node 配置（vite.config.ts 不动），不引入 @vue/test-utils，用 Vue 运行时
 * createApp 直接挂载。覆盖：开关（快捷键 / 按钮）、过滤、键盘上下选择 +
 * Enter 执行、Esc 关闭（reka dismiss 通道）、空态，以及 App.vue 触发按钮
 * 接线的静态守卫（按钮本体在 App.vue，过重无法直挂）。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createApp, nextTick, type App } from "vue";
import CommandPalette from "./CommandPalette.vue";
import { useCommandPalette, installCommandPaletteHotkey } from "@/composables/useCommandPalette";
import { router } from "@/router";
import { useAppearance } from "@/composables/useAppearance";
import { DEFAULT_APPEARANCE } from "@/utils/constants";

let app: App | null = null;
const palette = useCommandPalette();

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/**
 * 宏任务若干拍：reka 把 FocusScope 的焦点落点与文档级监听的注册排在
 * setTimeout(0) 定时器里，单拍微任务等不到（同 Modal.focus.test.ts 的 settle）。
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20));
  await flush();
}

/** 直挂面板本体：开态由单例状态（快捷键 / 按钮 / openPalette）驱动，无需宿主包装 */
function mountPalette(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(CommandPalette);
  app.mount(container);
}

/** 模拟键盘按下：默认打在 window 上（App.vue 的快捷键监听处） */
function pressKey(key: string, init: KeyboardEventInit = {}, target: EventTarget = window): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init }));
}

function options(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("[role='option']")];
}

function searchInput(): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>(".command-palette-input");
  if (!input) throw new Error("搜索框未渲染");
  return input;
}

function typeQuery(value: string): void {
  const input = searchInput();
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

beforeEach(() => {
  document.body.innerHTML = "";
  palette.closePalette();
  Object.assign(useAppearance().appearance, DEFAULT_APPEARANCE);
});

afterEach(() => {
  app?.unmount();
  app = null;
});

describe("命令面板开关", () => {
  it("Ctrl+K 打开，再次按下关闭，默认行为被拦截", async () => {
    mountPalette();
    const uninstall = installCommandPaletteHotkey();
    const open = new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true, cancelable: true });
    window.dispatchEvent(open);
    expect(open.defaultPrevented).toBe(true);
    expect(palette.open.value).toBe(true);
    await settle();
    expect(document.querySelector(".command-palette-panel")).not.toBeNull();
    // 焦点落在搜索框（reka openAutoFocus 显式落点），打开即打字
    expect(document.activeElement).toBe(searchInput());

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true, cancelable: true }));
    expect(palette.open.value).toBe(false);
    uninstall();
  });

  it("懒加载首开回归：open 先于挂载置真时命令集仍完整（不再是空列表）", async () => {
    // 真实入口是 defineAsyncComponent：首次点击时 open 已是 true，组件随后才挂载。
    // 若构建命令集的 watch 不是 immediate，这一生命周期顺序会让列表恒为空，
    // 用户看到「无匹配命令」而任何关键词都搜不到。
    palette.openPalette();
    mountPalette();
    await settle();
    expect(palette.open.value).toBe(true);
    expect(options().length).toBeGreaterThan(0);
    // 「浏览器」应命中设置分区命令（用户实测回归词）
    typeQuery("浏览器");
    await flush();
    const labels = options().map((el) => el.textContent ?? "");
    expect(labels.some((text) => text.includes("浏览器"))).toBe(true);
  });

  it("macOS Cmd+K 同样触发；焦点在文本框时仍可用（输入现场不丢失）", () => {
    mountPalette();
    const uninstall = installCommandPaletteHotkey();
    const input = document.createElement("input");
    input.value = "正在编辑的内容";
    document.body.appendChild(input);
    input.focus();
    expect(document.activeElement).toBe(input);
    // 大写 K 与 meta 键（mac）：事件目标为文本框，冒泡到 window 命中监听
    const event = new KeyboardEvent("keydown", { key: "K", metaKey: true, bubbles: true, cancelable: true });
    input.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(palette.open.value).toBe(true);
    uninstall();
  });

  it("触发按钮（App.vue 同款接线：点击 togglePalette）点击打开、再次点击关闭", async () => {
    mountPalette();
    // 按钮以裸 DOM 复刻 App.vue 的接线（click → togglePalette），避免测试文件
    // 里再定义宿主组件（vue/one-component-per-file）
    const trigger = document.createElement("button");
    trigger.className = "command-palette-trigger";
    trigger.setAttribute("aria-label", "打开命令面板");
    trigger.addEventListener("click", () => palette.togglePalette());
    document.body.appendChild(trigger);
    trigger.click();
    expect(palette.open.value).toBe(true);
    await settle();
    expect(document.querySelector(".command-palette-panel")).not.toBeNull();
    trigger.click();
    expect(palette.open.value).toBe(false);
  });

  it("Esc 经 reka dismiss 通道关闭", async () => {
    mountPalette();
    palette.openPalette();
    await settle();
    expect(palette.open.value).toBe(true);
    pressKey("Escape", {}, document);
    await settle();
    expect(palette.open.value).toBe(false);
  });
});

describe("命令过滤与执行", () => {
  it("打开即列出全部命令并按导航/操作/外观分组", async () => {
    mountPalette();
    palette.openPalette();
    await settle();
    const groupLabels = [...document.querySelectorAll(".command-palette-group-label")].map((el) => el.textContent);
    expect(groupLabels).toEqual(["导航", "操作", "外观"]);
    // 导航 14（仪表盘/配置方案/任务子项 5/设置子项 6/关于）+ 操作 4（默认未登录时
    // 「取消登录」被 when 隐藏）+ 主题 2（默认浅色下「主题：浅色」被 when 隐藏）
    expect(options().length).toBe(20);
    expect(options()[0].textContent).toContain("仪表盘");
  });

  it("输入过滤命中任务子项，无结果显示空态", async () => {
    mountPalette();
    palette.openPalette();
    await settle();
    typeQuery("定时");
    await flush();
    // 「定时」同时命中脚本子项的 title（定时执行的辅助脚本）与定时任务子项
    const matched = options();
    expect(matched.map((el) => el.textContent)).toEqual(["脚本", "定时任务"]);
    typeQuery("不存在的命令xyz");
    await flush();
    expect(document.querySelector(".command-palette-empty")).not.toBeNull();
    expect(options().length).toBe(0);
  });

  it("ArrowDown/Up 在结果间循环选择，Enter 执行当前项（导航走 router.push）并关闭面板", async () => {
    const pushSpy = vi.spyOn(router, "push").mockResolvedValue(undefined);
    mountPalette();
    palette.openPalette();
    await settle();
    const input = searchInput();
    expect(options()[0].getAttribute("aria-selected")).toBe("true");
    expect(input.getAttribute("aria-activedescendant")).toBe(options()[0].id);

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await flush();
    expect(options()[1].getAttribute("aria-selected")).toBe("true");
    // Enter 执行第二项（配置方案）后收起面板
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    await flush();
    expect(pushSpy).toHaveBeenCalledWith({ name: "profiles" });
    expect(palette.open.value).toBe(false);
    pushSpy.mockRestore();
  });

  it("主题命令执行后写入外观单例，已选主题不再出现在命令集", async () => {
    const { appearance } = useAppearance();
    mountPalette();
    palette.openPalette();
    await settle();
    typeQuery("深色");
    await flush();
    const matched = options();
    expect(matched.length).toBe(1);
    matched[0].click();
    await flush();
    expect(appearance.theme).toBe("dark");
    expect(palette.open.value).toBe(false);

    // 重开面板：浅色/跟随系统仍在，深色已被 when 隐藏（查询带冒号排除
    // 「设置 · 外观」——其 title「主题、背景与卡片样式」同样含「主题」）
    palette.openPalette();
    await settle();
    typeQuery("主题：");
    await flush();
    const themeLabels = options().map((el) => el.textContent);
    expect(themeLabels).toEqual(["主题：浅色", "主题：跟随系统"]);
  });
});

describe("命令面板入口静态守卫", () => {
  it("顶栏搜索入口接通命令面板，App.vue 保留快捷键与懒加载", () => {
    const appSrc = readFileSync(resolve(process.cwd(), "src/App.vue"), "utf8");
    expect(appSrc).toContain("installCommandPaletteHotkey");
    expect(appSrc).toContain("defineAsyncComponent(() => import(\"./components/common/CommandPalette.vue\"))");
    // 入口按钮统一在顶栏搜索（右下角常驻触发按钮已随顶栏改版移除）
    const topbarSrc = readFileSync(resolve(process.cwd(), "src/components/common/AppTopbar.vue"), "utf8");
    expect(topbarSrc).toContain('class="topbar-search"');
    expect(topbarSrc).toContain("@click=\"openPalette()\"");
    expect(appSrc).not.toContain("command-palette-trigger");
  });
});
