// @vitest-environment happy-dom
/**
 * SetupWizard 弹层行为测试（Reka Dialog 迁移后）。
 *
 * 向导是全屏阻断的 aria-modal 弹层，迁移前是手写 overlay：无焦点陷阱、无初始焦点，
 * 键盘用户可以真实 Tab 走出遮罩落到背景页面。迁移后焦点语义由 reka Dialog 承担
 * （与 Modal/ConfirmDialog 同范式），本文件锁定：
 * - 打开时焦点进入弹窗（role=dialog / aria-modal 由库输出，DialogTitle 作可访问名）；
 * - Esc / 遮罩外交互不关闭（全屏阻断，唯一退出路径是「跳过向导」确认流程）；
 * - 关闭后焦点归还触发元素。
 *
 * Reka 测试要点：Esc/焦点监听经 setTimeout(0) 落 window，合成事件需 bubbles: true
 * 并留 ~20ms 宏任务沉降（同 Modal.focus.test.ts 的 settle）。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createApp, nextTick, type App } from "vue";
import SetupWizard from "./SetupWizard.vue";
import { useUi } from "../../composables/useUi";

// happy-dom 在 Windows runner 上会尝试从文件系统解析模板里的
// <img src="/icons/...">，盘符路径非法直接抛 TypeError（CI 首跑实锤，PR #33；
// 仓库内仅本文件渲染 BrowserIcon，未处理 rejection 还会漂移归因到同 worker
// 的下一个测试文件）。向导焦点语义与图标渲染无关，桩掉图标组件即断掉
// 资源解析路径（vi.mock 具提升语义，写在导入语句之后仍先于模块求值生效）
vi.mock("./BrowserIcon.vue", () => ({
  default: { name: "BrowserIcon", render: () => null },
}));

let app: App | null = null;
let trigger: HTMLElement | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：reka 把 Esc/焦点监听注册与焦点移动排在 setTimeout(0) 定时器里 */
async function settle(): Promise<void> {
  await new Promise((r) => setTimeout(r, 20));
  await flush();
}

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(SetupWizard);
  app.mount(container);
  // 触发元素放在组件之外（DialogPortal 会把弹层挂到 body），打开前持焦点供归还断言
  trigger = document.createElement("button");
  trigger.id = "wizard-outside-trigger";
  document.body.appendChild(trigger);
}

function wizardContainer(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".wizard-container");
  expect(el, "向导面板未渲染").not.toBeNull();
  return el!;
}

describe("SetupWizard 弹层语义（reka Dialog 承担）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    useUi().state.showWizard = false;
    useUi().state.agreedToTerms = false;
  });

  afterEach(() => {
    app?.unmount();
    app = null;
    trigger = null;
  });

  it("showWizard 打开时焦点进入弹窗，DialogTitle 提供可访问名；Esc 不关闭", async () => {
    mount();
    await flush();
    expect(document.querySelector(".wizard-overlay")).toBeNull();

    trigger!.focus();
    expect(document.activeElement).toBe(trigger);
    useUi().state.showWizard = true;
    await settle();

    const container = wizardContainer();
    expect(container.getAttribute("role")).toBe("dialog");
    expect(container.getAttribute("aria-modal")).toBe("true");
    // 可访问名来自 DialogTitle（h1），不再是手写 aria-label
    const title = container.querySelector("h1");
    expect(title, "DialogTitle 应渲染为 h1").not.toBeNull();
    expect(container.getAttribute("aria-labelledby")).toBe(title!.id);

    // 焦点已进入弹窗（reka FocusScope），不再落在背景触发器上
    expect(container.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).not.toBe(trigger);

    // Esc 不关闭：合成事件需 bubbles 到 document（reka 的监听挂在 document/window）
    document.activeElement!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    await settle();
    expect(document.querySelector(".wizard-overlay")).not.toBeNull();
    expect(useUi().state.showWizard).toBe(true);
  });

  it("遮罩外交互不关闭（全屏阻断，dismiss 路径一律否决）", async () => {
    mount();
    useUi().state.showWizard = true;
    await settle();
    expect(document.querySelector(".wizard-overlay")).not.toBeNull();

    // 在弹层外（body 直属触发器上）按下指针：reka 会发 pointer-down-outside
    trigger!.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await settle();
    expect(document.querySelector(".wizard-overlay")).not.toBeNull();
  });

  it("showWizard 关闭后弹层卸载，焦点归还触发元素", async () => {
    mount();
    trigger!.focus();
    useUi().state.showWizard = true;
    await settle();
    expect(wizardContainer().contains(document.activeElement)).toBe(true);

    useUi().state.showWizard = false;
    await settle();
    expect(document.querySelector(".wizard-overlay")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});

describe("SetupWizard 静态守卫：弹层语义不得回潮为手写", () => {
  it("向导经 reka DialogPortal 渲染，不手写滚动锁", () => {
    const src = readFileSync(resolve(process.cwd(), "src/components/common/SetupWizard.vue"), "utf8");
    expect(src, "弹层应走 reka DialogRoot（焦点陷阱/滚动锁/焦点归还由库承担）").toContain("DialogRoot");
    expect(src, "滚动锁应交给 reka Dialog（内建）").not.toContain("lockBodyScroll");
    expect(src, "面板不应手写 role=dialog（由 DialogContent 输出）").not.toMatch(/class="wizard-overlay"[^>]*role="dialog"/);
  });
});
