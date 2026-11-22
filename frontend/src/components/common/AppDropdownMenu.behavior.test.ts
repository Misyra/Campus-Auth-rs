// @vitest-environment happy-dom
/**
 * E6：AppDropdownMenu 行为层（Reka DropdownMenu 收口后白得的键盘行为）。
 * 文件级 happy-dom 环境覆盖全局 node 配置（vite.config.ts 不动）。
 * 不引入 @vue/test-utils，用 Vue 运行时 createApp 直接挂载（同 Modal.focus.test.ts）。
 * 静态守卫：四个任务面板不得再手写菜单状态 / 文档级监听（防回潮）。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createApp, defineComponent, h, nextTick, type App } from "vue";
import AppDropdownMenu from "./AppDropdownMenu.vue";
import AppDropdownMenuItem from "./AppDropdownMenuItem.vue";

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/**
 * 宏任务若干拍：reka 把文档级 pointerdown 监听的注册（防"打开的那次按下即关闭"）
 * 与 FocusScope 的焦点归还都排在 setTimeout(0) 定时器里，且前面还排着同类任务，
 * 单拍宏任务等不到；等一小段真实时间让定时器队列清空后再走微任务。
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20));
  await flush();
}

/** 宿主组件：触发按钮 + 外部按钮 + 两项菜单，select 动作记录到 actions */
function mountMenu(): { actions: string[] } {
  const actions: string[] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(
    defineComponent({
      setup() {
        return () =>
          h("div", [
            h("button", { id: "outside", type: "button" }, "外部按钮"),
            h(AppDropdownMenu, undefined, {
              trigger: () => h("button", { id: "menu-trigger", type: "button" }, "⋯"),
              default: () => [
                h(AppDropdownMenuItem, { onSelect: () => actions.push("first") }, () => "动作一"),
                h(AppDropdownMenuItem, { onSelect: () => actions.push("second") }, () => "动作二"),
              ],
            }),
          ]);
      },
    }),
  );
  app.mount(container);
  return { actions };
}

function triggerButton(): HTMLButtonElement {
  const el = document.getElementById("menu-trigger");
  if (!el) throw new Error("触发按钮未渲染");
  return el as HTMLButtonElement;
}

/** 打开菜单：真实浏览器里点击会顺带聚焦按钮，happy-dom 需显式聚焦（焦点归还依赖它） */
async function openMenu(): Promise<void> {
  triggerButton().focus();
  triggerButton().click();
  await settle();
}

function menuContent(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".tsk-menu");
  if (!el) throw new Error("菜单未渲染");
  return el;
}

/** 真实键盘事件会沿文档冒泡到 window（reka 的 Esc 监听挂在 window 上） */
function pressKey(key: string, target: EventTarget = document): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
}

describe("AppDropdownMenu 行为层（E6）", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  afterEach(() => {
    app?.unmount();
    app = null;
  });

  it("语义输出：触发器 aria-haspopup=menu + aria-expanded 跟随开合，Content 为 role=menu，菜单项为可聚焦的 button[role=menuitem]", async () => {
    mountMenu();
    await flush();

    expect(triggerButton().getAttribute("aria-haspopup")).toBe("menu");
    expect(triggerButton().getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelector(".tsk-menu")).toBeNull();

    await openMenu();

    expect(triggerButton().getAttribute("aria-expanded")).toBe("true");
    const menu = menuContent();
    expect(menu.getAttribute("role")).toBe("menu");
    const items = Array.from(menu.querySelectorAll("button[role='menuitem']"));
    expect(items.map((el) => el.textContent)).toEqual(["动作一", "动作二"]);
  });

  it("Esc 关闭且焦点归还触发器（原手写版只关面板不还焦点）", async () => {
    mountMenu();
    await openMenu();
    expect(document.querySelector(".tsk-menu")).not.toBeNull();

    // Esc 监听与 FocusScope 焦点归还都排在 reka 的 setTimeout(0) 延迟队列
    // （见 settle 注释）：慢 runner 上队列清空可超任何固定预算——轮询内重放
    // Esc（菜单已关时再按是无害 no-op）并断言双终态
    await vi.waitFor(
      () => {
        pressKey("Escape");
        expect(document.querySelector(".tsk-menu")).toBeNull();
        expect(document.activeElement).toBe(triggerButton());
      },
      { timeout: 3000, interval: 50 },
    );
  });

  it("外部 pointerdown 关闭（原文档级监听的职责，由库接管）", async () => {
    mountMenu();
    await openMenu();
    expect(document.querySelector(".tsk-menu")).not.toBeNull();

    // 外点监听器的注册同样在 reka 延迟队列：事件发在挂载前即永久丢失，
    // waitFor 等再久也等不到（round 6/8 windows 实测，预算放宽无效）——
    // 轮询内重放 pointerdown，挂载后必有下一发命中；外点按下除关闭外
    // 无副作用，重放安全
    await vi.waitFor(
      () => {
        document.getElementById("outside")!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
        expect(document.querySelector(".tsk-menu")).toBeNull();
      },
      { timeout: 3000, interval: 50 },
    );
  });

  it("ArrowDown 高亮聚焦首项，Enter 选中并自动关闭（方向键 / 选择 / 关闭一体）", async () => {
    const { actions } = mountMenu();
    await openMenu();
    const menu = menuContent();

    pressKey("ArrowDown", document.activeElement ?? menu);
    await flush();
    const items = Array.from(menu.querySelectorAll("button[role='menuitem']"));
    expect(document.activeElement).toBe(items[0]);

    pressKey("Enter", document.activeElement!);
    await settle();
    expect(actions).toEqual(["first"]);
    expect(document.querySelector(".tsk-menu")).toBeNull();
  });

  it("点击菜单项（click）同样选中并关闭", async () => {
    const { actions } = mountMenu();
    await openMenu();
    const menu = menuContent();
    (menu.querySelectorAll("button[role='menuitem']")[1] as HTMLButtonElement).click();
    await settle();
    expect(actions).toEqual(["second"]);
    expect(document.querySelector(".tsk-menu")).toBeNull();
  });
});

describe("E6 静态守卫：面板不得再手写菜单行为", () => {
  const PANELS = ["HttpTasksPanel", "BrowserTasksPanel", "ScheduledTasksPanel", "ScriptsPanel"];

  it.each(PANELS)("%s 不再包含 openMenuId / 文档级监听（收口后由 AppDropdownMenu 承担）", (name) => {
    const src = readFileSync(resolve(process.cwd(), "src/views/tasks", `${name}.vue`), "utf8");
    expect(src, `${name} 应改用 AppDropdownMenu，不得再持 openMenuId`).not.toContain("openMenuId");
    expect(src, `${name} 不得再挂文档级菜单监听`).not.toContain("document.addEventListener");
    expect(src, `${name} 应使用收口组件`).toContain("AppDropdownMenu");
  });
});
