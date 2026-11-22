// @vitest-environment happy-dom
/**
 * Toast live region 行为测试（P2-3 修复）。
 *
 * aria-live 区域必须先于内容变化存在才有播报保证：原实现 role="alert" 挂在
 * 每次新建的 .toast 节点上，屏幕阅读器把消息进出当成节点增删而非 live 区域
 * 更新，会漏播。现在容器常驻、消息体在容器内进出——本文件钉住三个不变量：
 * - 空态下 .toast-live 已存在（live 节点先于消息）；
 * - 连续两条消息使用同一个 live 节点（节点身份不变）；
 * - 消息结束后 live 节点仍在，仅消息体移除（离场动画走内容级 .leaving class）。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { createApp, nextTick, type App } from "vue";
import ToastNotification from "./ToastNotification.vue";
import { useToast } from "../../composables/useToast";
import { TIMING } from "../../utils/constants";

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

function liveRegion(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".toast-live");
  expect(el, "常驻 live region 未渲染").not.toBeNull();
  return el!;
}

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(ToastNotification);
  app.mount(container);
}

describe("Toast live region 常驻", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = "";
  });

  afterEach(() => {
    app?.unmount();
    app = null;
    vi.useRealTimers();
  });

  it("空态下 live region 已存在且不带消息体", async () => {
    mount();
    await flush();
    const live = liveRegion();
    expect(live.getAttribute("aria-live")).toBe("assertive");
    expect(live.querySelector(".toast")).toBeNull();
  });

  it("连续两条消息共用同一个 live 节点（节点身份不变）", async () => {
    mount();
    await flush();
    const { toastOnly } = useToast();
    const before = liveRegion();

    toastOnly(true, "第一条提示");
    await flush();
    expect(liveRegion().querySelector(".toast")?.textContent).toContain("第一条提示");
    expect(liveRegion()).toBe(before);

    // 未等离场直接替换下一条（useToast 替换语义：清旧计时器、立即上新）
    toastOnly(false, "第二条提示");
    await flush();
    const toastEl = liveRegion().querySelector<HTMLElement>(".toast");
    expect(toastEl?.textContent).toContain("第二条提示");
    expect(toastEl?.classList.contains("error")).toBe(true);
    expect(liveRegion()).toBe(before);
  });

  it("消息结束后 live 节点仍在，离场走内容级 .leaving class", async () => {
    mount();
    await flush();
    const { toastOnly } = useToast();
    const before = liveRegion();

    toastOnly(true, "会消失的提示");
    await flush();
    expect(liveRegion().querySelector(".toast.leaving")).toBeNull();

    // 展示期满 → 进入离场（内容级 class，live 节点不动）
    vi.advanceTimersByTime(TIMING.TOAST_DURATION);
    await flush();
    expect(liveRegion().querySelector(".toast.leaving")).not.toBeNull();
    expect(liveRegion()).toBe(before);

    // 离场动画期满 → 消息体移除，live 节点依旧常驻
    vi.advanceTimersByTime(TIMING.TOAST_LEAVE_DELAY);
    await flush();
    expect(liveRegion().querySelector(".toast")).toBeNull();
    expect(liveRegion()).toBe(before);
  });
});
