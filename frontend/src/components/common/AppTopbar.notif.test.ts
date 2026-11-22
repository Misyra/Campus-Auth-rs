// @vitest-environment happy-dom
/**
 * E2：通知下拉的语义与键盘路径。
 * 文件级 happy-dom 环境覆盖全局 node 配置（vite.config.ts 不动）。
 * 不引入 @vue/test-utils，用 Vue 运行时 createApp 直接挂载（同 Modal.focus.test.ts）。
 * AppTopbar 经 useRoute/useRouter 注入路由，挂最小 memory-history 路由即可，
 * 不用业务路由（避免初始导航懒加载真实视图）。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { createApp, defineComponent, h, nextTick, type App } from "vue";
import { createRouter, createMemoryHistory } from "vue-router";
import AppTopbar from "./AppTopbar.vue";
import { useNotifications } from "../../composables/useNotifications";

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

function mountTopbar(): void {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: "/", component: defineComponent({ render: () => null }) }],
  });
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(defineComponent({ render: () => h(AppTopbar) }));
  app.use(router);
  app.mount(container);
}

function bellButton(): HTMLButtonElement {
  const el = document.querySelector<HTMLButtonElement>('button[aria-label="通知历史"]');
  if (!el) throw new Error("铃铛按钮未渲染");
  return el;
}

function pressEscape(): void {
  // E6 起面板的 Esc 由 Reka（DismissableLayer，监听挂在 window）接管：
  // 真实键盘事件沿 document 冒泡到 window，这里合成事件补 bubbles 对齐该路径，
  // 断言语义（Esc 关闭 / 卸载后 Esc 不影响单例）不变。
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
}

describe("通知下拉语义与键盘路径（E2）", () => {
  // useNotifications 是模块单例：showNotifications 跨用例共享，须在钩子里复位
  const { showNotifications } = useNotifications();

  beforeEach(() => {
    document.body.innerHTML = "";
    showNotifications.value = false;
  });

  afterEach(() => {
    app?.unmount();
    app = null;
    showNotifications.value = false;
  });

  it("触发按钮 aria-haspopup=dialog 且 aria-expanded 跟随开合；面板为 role=dialog + 通知历史标注（不再是 menu）", async () => {
    mountTopbar();
    await flush();

    const bell = bellButton();
    expect(bell.getAttribute("aria-haspopup")).toBe("dialog");
    expect(bell.getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelector(".notification-dropdown")).toBeNull();

    bell.click();
    await flush();

    expect(bell.getAttribute("aria-expanded")).toBe("true");
    const panel = document.querySelector<HTMLElement>(".notification-dropdown");
    expect(panel).not.toBeNull();
    expect(panel!.getAttribute("role")).toBe("dialog");
    expect(panel!.getAttribute("aria-label")).toBe("通知历史");
  });

  it("Esc 关闭打开的面板，监听随开合重挂（重开后仍可 Esc）", async () => {
    mountTopbar();
    await flush();

    bellButton().click();
    await flush();
    expect(document.querySelector(".notification-dropdown")).not.toBeNull();

    pressEscape();
    await flush();
    expect(showNotifications.value).toBe(false);
    expect(document.querySelector(".notification-dropdown")).toBeNull();

    // 重开：watch 重新挂监听，Esc 依然可关（若只挂一次此处就会暴露）
    bellButton().click();
    await flush();
    expect(document.querySelector(".notification-dropdown")).not.toBeNull();
    pressEscape();
    await flush();
    expect(document.querySelector(".notification-dropdown")).toBeNull();
  });

  it("打开状态下卸载组件，文档级 Esc 监听被清理（不再影响共享单例状态）", async () => {
    mountTopbar();
    await flush();
    bellButton().click();
    await flush();
    expect(showNotifications.value).toBe(true);

    app!.unmount();
    app = null;

    pressEscape();
    await flush();
    // 若监听泄漏，Escape 会把单例 showNotifications 置回 false
    expect(showNotifications.value).toBe(true);
  });
});
