// @vitest-environment happy-dom
/**
 * E1：Modal 关闭后焦点归还触发元素。
 * 文件级 happy-dom 环境覆盖全局 node 配置（vite.config.ts 不动）。
 * 不引入 @vue/test-utils，用 Vue 运行时 createApp 直接挂载。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { createApp, defineComponent, h, nextTick, ref, type App } from "vue";
import Modal from "./Modal.vue";

let app: App | null = null;

function flush(): Promise<void> {
  // 两拍：watcher 内部还有一次 await nextTick 的续延
  return nextTick().then(() => nextTick());
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

describe("Modal 焦点归还（E1）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  afterEach(() => {
    app?.unmount();
    app = null;
  });

  it("打开时记录触发器并聚焦弹窗内元素；emit close 后焦点归还触发器", async () => {
    mountHost(false);
    await flush();

    // 模拟键盘用户 Tab 到触发按钮
    const trigger = triggerButton();
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    // 打开：焦点进入弹窗（首个可聚焦元素 = 头部关闭按钮）
    trigger.click();
    await flush();
    const container = document.querySelector<HTMLElement>(".modal-container");
    expect(container).not.toBeNull();
    expect(container?.contains(document.activeElement)).toBe(true);

    // 组件关闭按钮 emit("close") → 宿主置 open=false → 焦点回到触发器
    const closeButton = document.querySelector<HTMLElement>(".modal-header button");
    expect(closeButton).not.toBeNull();
    closeButton!.click();
    await flush();

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
    await flush();
    // 打开路径照常聚焦弹窗内元素
    expect(
      document.querySelector<HTMLElement>(".modal-container")?.contains(document.activeElement),
    ).toBe(true);

    app.unmount();
    app = null;
    expect(document.activeElement).toBe(outside);
  });
});
