// @vitest-environment happy-dom
/**
 * ConfirmDialog 行为层（Reka Dialog 迁移后）。原 E1 手写焦点归还已由 reka 承担；
 * danger 语义（打开先聚焦取消）由 openAutoFocus 阻止库默认后自行定位。
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
  return nextTick().then(() => nextTick());
}

/**
 * 宏任务若干拍：reka 把 FocusScope 的焦点归还排在 setTimeout(0) 定时器里，
 * 且前面还排着同类任务，单拍宏任务等不到（同 Modal.focus.test.ts 的 settle）。
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20));
  await flush();
}

describe("ConfirmDialog 焦点语义（reka Dialog 承担）", () => {
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

  it("danger 确认框为 alertdialog，打开聚焦取消按钮；取消后焦点归还触发器", async () => {
    const trigger = document.createElement("button");
    trigger.id = "confirm-trigger";
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    const pending = confirm({ message: "确认删除？", danger: true });
    await settle();

    // danger：打开时聚焦取消按钮（第一个按钮），避免误按 Enter 执行危险操作
    const dialog = document.querySelector<HTMLElement>(".confirm-dialog");
    expect(dialog).not.toBeNull();
    expect(dialog?.getAttribute("role")).toBe("alertdialog");
    const buttons = document.querySelectorAll<HTMLElement>(
      ".confirm-dialog .confirm-actions button",
    );
    expect(buttons.length).toBe(2);
    expect(document.activeElement).toBe(buttons[0]);

    // 取消 → visible=false → 焦点回到触发器
    buttons[0].click();
    await settle();

    expect(confirmState.visible).toBe(false);
    expect(document.activeElement).toBe(trigger);
    await expect(pending).resolves.toBe(false);
  });

  it("非 danger 确认框聚焦确认按钮，确认后焦点同样归还触发器", async () => {
    const trigger = document.createElement("button");
    trigger.id = "confirm-trigger-2";
    document.body.appendChild(trigger);
    trigger.focus();

    const pending = confirm({ message: "继续操作？" });
    await settle();

    const buttons = document.querySelectorAll<HTMLElement>(
      ".confirm-dialog .confirm-actions button",
    );
    expect(buttons.length).toBe(2);
    // 非 danger：聚焦确认按钮（最后一个）
    expect(document.activeElement).toBe(buttons[buttons.length - 1]);

    buttons[buttons.length - 1].click();
    await settle();

    expect(confirmState.visible).toBe(false);
    expect(document.activeElement).toBe(trigger);
    await expect(pending).resolves.toBe(true);
  });

  it("Esc 关闭（reka 窗口级监听）按取消结算并归还焦点", async () => {
    const trigger = document.createElement("button");
    trigger.id = "confirm-trigger-3";
    document.body.appendChild(trigger);
    trigger.focus();

    const pending = confirm({ message: "继续操作？" });
    await settle();
    expect(document.querySelector(".confirm-dialog")).not.toBeNull();

    // 真实键盘事件沿文档冒泡到 window（reka 的 Esc 监听挂在 window 上）
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();

    expect(confirmState.visible).toBe(false);
    expect(document.activeElement).toBe(trigger);
    await expect(pending).resolves.toBe(false);
  });
});
