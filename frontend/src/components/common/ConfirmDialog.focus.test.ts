// @vitest-environment happy-dom
/**
 * E1：ConfirmDialog 关闭后焦点归还触发元素。
 * useConfirm 是模块级单例，测试直接 import 同一状态驱动弹窗；
 * 不引入 @vue/test-utils，用 Vue 运行时 createApp 直接挂载组件。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import { useConfirm } from "../../composables/useConfirm";

const { confirmState, confirm } = useConfirm();

let app: App | null = null;

function flush(): Promise<void> {
  // 两拍：watcher 内部还有一次 await nextTick 的续延
  return nextTick().then(() => nextTick());
}

describe("ConfirmDialog 焦点归还（E1）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    confirmState.visible = false;
    const container = document.createElement("div");
    document.body.appendChild(container);
    app = createApp(ConfirmDialog);
    app.mount(container);
  });

  afterEach(() => {
    app?.unmount();
    app = null;
  });

  it("danger 确认框打开聚焦取消按钮；取消后焦点归还触发器", async () => {
    const trigger = document.createElement("button");
    trigger.id = "confirm-trigger";
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    const pending = confirm({ message: "确认删除？", danger: true });
    await flush();

    // danger：打开时聚焦取消按钮（第一个按钮），避免误按 Enter 执行危险操作
    const buttons = document.querySelectorAll<HTMLElement>(
      ".confirm-dialog .confirm-actions button",
    );
    expect(buttons.length).toBe(2);
    expect(document.activeElement).toBe(buttons[0]);

    // 取消 → visible=false → 焦点回到触发器
    buttons[0].click();
    await flush();

    expect(confirmState.visible).toBe(false);
    expect(document.activeElement).toBe(trigger);
    await expect(pending).resolves.toBe(false);
  });

  it("非 danger 确认框确认后焦点同样归还触发器", async () => {
    const trigger = document.createElement("button");
    trigger.id = "confirm-trigger-2";
    document.body.appendChild(trigger);
    trigger.focus();

    const pending = confirm({ message: "继续操作？" });
    await flush();

    const buttons = document.querySelectorAll<HTMLElement>(
      ".confirm-dialog .confirm-actions button",
    );
    expect(buttons.length).toBe(2);
    // 非 danger：聚焦确认按钮（最后一个）
    expect(document.activeElement).toBe(buttons[buttons.length - 1]);

    buttons[buttons.length - 1].click();
    await flush();

    expect(confirmState.visible).toBe(false);
    expect(document.activeElement).toBe(trigger);
    await expect(pending).resolves.toBe(true);
  });
});
