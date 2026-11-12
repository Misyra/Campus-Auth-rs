// @vitest-environment happy-dom
/**
 * 阶段3扩展2：FieldHelp 行为层（Reka Tooltip 收口后白得的行为 + 钉住态语义）。
 * 文件级 happy-dom 环境覆盖全局 node 配置（vite.config.ts 不动），不引入
 * @vue/test-utils，用 Vue 运行时 createApp 直接挂载（同 AppDropdownMenu.behavior.test.ts）。
 *
 * 悬停开启用 pointermove 合成事件（handlePointerMove 只排除 touch，
 * MouseEvent.pointerType 为 undefined 即放行）；关闭路径里 grace area 依赖
 * 指针几何（happy-dom 矩形全 0），不做悬停关闭的几何仿真，关闭语义经
 * 滚动 / Esc / 外部按下验证——钉住态对滚动的"驳回"是本次迁移的关键判据。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createApp, defineComponent, h, nextTick, type App } from "vue";
import FieldHelp from "./FieldHelp.vue";

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：reka 的开启定时器（delayDuration=0 仍是宏任务）与文档级监听注册都排在 setTimeout 里 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20));
  await flush();
}

function mountHelp(props: { text: string; wide?: boolean } = { text: "第一段\n第二段" }): { outside: HTMLButtonElement } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const outside = document.createElement("button");
  outside.id = "outside";
  outside.type = "button";
  document.body.appendChild(outside);
  app = createApp(defineComponent({ setup: () => () => h(FieldHelp, props) }));
  app.mount(container);
  return { outside };
}

function trigger(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".field-help");
  if (!el) throw new Error("触发器未渲染");
  return el;
}

function tip(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".field-help-tip");
  if (!el) throw new Error("气泡未渲染");
  return el;
}

/** 悬停开启：pointermove 合成事件（真实浏览器里鼠标掠过必发） */
async function openByHover(): Promise<void> {
  trigger().dispatchEvent(new MouseEvent("pointermove", { bubbles: true }));
  await settle();
}

/** 点击钉住（click 事件即可，组件内部 togglePinned 不依赖 pointer 细节） */
async function openByPin(): Promise<void> {
  trigger().click();
  await settle();
}

function isPinned(): boolean {
  return trigger().classList.contains("field-help--pinned");
}

describe("FieldHelp 行为层（Reka Tooltip 收口）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  afterEach(() => {
    app?.unmount();
    app = null;
  });

  it("悬停即显（delayDuration=0 维持原节奏）：气泡渲染文本，触发器经 aria-describedby 关联 role=tooltip 的真实元素", async () => {
    mountHelp();
    expect(document.querySelector(".field-help-tip")).toBeNull();

    await openByHover();

    const content = tip();
    expect(content.textContent).toContain("第一段");
    const descId = trigger().getAttribute("aria-describedby");
    expect(descId).toBeTruthy();
    const described = document.getElementById(descId!);
    expect(described?.getAttribute("role")).toBe("tooltip");
  });

  it("点击钉住：悬停开启后点击不关反钉（disableClosingTrigger），再点 Esc 前一直存活", async () => {
    mountHelp();
    await openByHover();
    expect(isPinned()).toBe(false);

    trigger().click();
    await flush();

    expect(isPinned()).toBe(true);
    expect(document.querySelector(".field-help-tip")).not.toBeNull();
  });

  it("钉住态跨滚动存活（受控驳回库的滚动关闭），Esc 才真正解除", async () => {
    mountHelp();
    await openByPin();
    expect(document.querySelector(".field-help-tip")).not.toBeNull();

    // 页面滚动：库会对每次 scroll 请求关闭，钉住态在 update:open 里驳回
    document.querySelector(".field-help")!.parentElement!.dispatchEvent(new Event("scroll"));
    await settle();
    expect(document.querySelector(".field-help-tip")).not.toBeNull();
    expect(isPinned()).toBe(true);

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();
    expect(document.querySelector(".field-help-tip")).toBeNull();
    expect(isPinned()).toBe(false);
  });

  it("未钉住的悬停气泡随滚动关闭（库级语义，原 CSS 版会跟着容器滚走）", async () => {
    mountHelp();
    await openByHover();
    expect(document.querySelector(".field-help-tip")).not.toBeNull();

    trigger().parentElement!.dispatchEvent(new Event("scroll"));
    await settle();
    expect(document.querySelector(".field-help-tip")).toBeNull();
  });

  it("外部 pointerdown 解除钉住；Esc 关闭（原手写文档级监听的职责，由库接管）", async () => {
    const { outside } = mountHelp();
    await openByPin();
    expect(document.querySelector(".field-help-tip")).not.toBeNull();

    outside.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    await settle();
    expect(document.querySelector(".field-help-tip")).toBeNull();

    await openByPin();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();
    expect(document.querySelector(".field-help-tip")).toBeNull();
  });

  it("键盘开合：Enter 钉住 / Esc 解除，气泡对屏读器以 role=tooltip 暴露（原伪元素内容不可达）", async () => {
    mountHelp();
    trigger().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();

    expect(isPinned()).toBe(true);
    const content = tip();
    expect(content.textContent).toContain("第二段");

    trigger().dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();
    expect(document.querySelector(".field-help-tip")).toBeNull();
  });

  it("wide 变体与多段文本：--wide 落在气泡上，\\n 原样进入 pre-line 文本", async () => {
    mountHelp({ text: "长文案", wide: true });
    await openByPin();
    expect(tip().classList.contains("field-help-tip--wide")).toBe(true);
    expect(tip().querySelector(".field-help-tip-body")?.textContent).toBe("长文案");
  });
});

describe("FieldHelp 静态守卫：行为层不得回潮", () => {
  const source = (): string =>
    readFileSync(resolve(__dirname, "FieldHelp.vue"), "utf-8");

  it("组件不再手写文档级监听与实测定位代码（归 Reka / floating-ui；注释里提旧实现不受限）", () => {
    const src = source();
    expect(src).not.toContain("document.addEventListener");
    expect(src).not.toContain("getBoundingClientRect");
    expect(src).not.toContain("data-tip=");
  });

  it("ProfilesView 的裸 .field-help span 已收编为组件（防伪元素气泡回潮）", () => {
    const src = readFileSync(resolve(__dirname, "../../views/ProfilesView.vue"), "utf-8");
    expect(src).not.toContain('class="field-help"');
    expect(src).toContain("<FieldHelp");
  });
});
