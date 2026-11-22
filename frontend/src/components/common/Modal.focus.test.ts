// @vitest-environment happy-dom
/**
 * Modal 行为层（Reka Dialog 迁移后）。原 E1 手写焦点归还已由 reka 承担：
 * 触发元素在 DialogContent 挂载时由库记录（activeElement），关闭/卸载时归还。
 * 文件级 happy-dom 环境覆盖全局 node 配置（vite.config.ts 不动）。
 * 不引入 @vue/test-utils，用 Vue 运行时 createApp 直接挂载。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createApp, defineComponent, h, nextTick, ref, type App } from "vue";
import Modal from "./Modal.vue";

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/**
 * 宏任务若干拍：reka 把 FocusScope 的焦点归还排在 setTimeout(0) 定时器里，
 * 且前面还排着同类任务，单拍宏任务等不到；等一小段真实时间让定时器队列
 * 清空后再走微任务（同 AppDropdownMenu.behavior.test.ts 的 settle）。
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20));
  await flush();
}

/** 宿主组件：触发按钮 + Modal，emit("close") 置 open=false（模拟真实使用方） */
function mountHost(initialOpen: boolean): void {
  const open = ref(initialOpen);
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(
    defineComponent({
      setup() {
        return () =>
          h("div", [
            h("button", { id: "trigger", onClick: () => (open.value = true) }, "打开弹窗"),
            h(Modal, {
              open: open.value,
              title: "测试弹窗",
              onClose: () => (open.value = false),
            }),
          ]);
      },
    }),
  );
  app.mount(container);
}

function triggerButton(): HTMLElement {
  const el = document.getElementById("trigger");
  if (!el) throw new Error("触发按钮未渲染");
  return el;
}

describe("Modal 焦点语义（reka Dialog 承担）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  afterEach(() => {
    app?.unmount();
    app = null;
  });

  it("打开时焦点进入弹窗（role=dialog/aria-modal 由库输出）；关闭后焦点归还触发器", async () => {
    mountHost(false);
    await flush();

    // 模拟键盘用户 Tab 到触发按钮（reka 在内容挂载时记录 activeElement 为触发器）
    const trigger = triggerButton();
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    // 打开：焦点进入弹窗（首个可聚焦元素 = 头部关闭按钮）
    trigger.click();
    await settle();
    const container = document.querySelector<HTMLElement>(".modal-container");
    expect(container).not.toBeNull();
    expect(container?.getAttribute("role")).toBe("dialog");
    expect(container?.getAttribute("aria-modal")).toBe("true");
    expect(container?.contains(document.activeElement)).toBe(true);

    // 组件关闭按钮 emit("close") → 宿主置 open=false → 焦点回到触发器
    const closeButton = document.querySelector<HTMLElement>(".modal-header button");
    expect(closeButton).not.toBeNull();
    closeButton!.click();
    await settle();

    expect(document.activeElement).toBe(trigger);
  });

  it("open 状态下组件卸载时焦点归还触发器", async () => {
    // 触发器放在宿主之外，避免随宿主一起卸载
    const outside = document.createElement("button");
    outside.id = "outside-trigger";
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    const container = document.createElement("div");
    document.body.appendChild(container);
    app = createApp(defineComponent({ setup: () => () => h(Modal, { open: true, title: "卸载测试" }) }));
    app.mount(container);
    await settle();
    // 打开路径照常聚焦弹窗内元素
    expect(
      document.querySelector<HTMLElement>(".modal-container")?.contains(document.activeElement),
    ).toBe(true);

    app.unmount();
    app = null;
    await settle();
    expect(document.activeElement).toBe(outside);
  });
});

describe("Modal 静态守卫：行为层不得回潮为手写", () => {
  it("Modal.vue 不再手写滚动锁 / 触发器记录 / 文档级监听", () => {
    const src = readFileSync(resolve(process.cwd(), "src/components/common/Modal.vue"), "utf8");
    expect(src, "滚动锁应交给 reka Dialog（DialogOverlay 内建）").not.toContain("lockBodyScroll");
    expect(src, "焦点归还应交给 reka（触发器记录/还原不回潮）").not.toContain("lastTrigger");
    expect(src, "不得再挂文档级监听").not.toContain("document.addEventListener");
  });
});
