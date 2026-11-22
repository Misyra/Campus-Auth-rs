// @vitest-environment happy-dom
/**
 * CommunityTasksEntry 行为测试（createApp 直挂，免 @vue/test-utils，同
 * AuthBootstrapGuide.test.ts 范式）。
 *
 * 锁定引导气泡的出现口径（只在「任务目录已加载且没有任何自己的任务」时出现）
 * 与两条关闭路径（✕ / 点按钮本身）都写入 localStorage 永久记忆。
 * useRepoImport 整体 mock 掉——导入弹窗的真实开合由 useRepoImport.test.ts 覆盖，
 * 这里只验证入口按钮触发了 showRepoImport。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import CommunityTasksEntry from "./CommunityTasksEntry.vue";
import { useTaskDirectory } from "../../composables/useTaskDirectory";
import type { TaskItem, Script } from "../../api/types";

const showRepoImport = vi.fn();

vi.mock("../../composables/useRepoImport", () => ({
  useRepoImport: () => ({ showRepoImport }),
}));

// 任务目录是模块级单例：经 useTaskDirectory() 取 refs，测试间直接改值模拟数据
const { browserTasks, httpTasks, scripts, loaded } = useTaskDirectory();

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 浏览器任务目录条目的最小形态（测试只消费 id 字段） */
function taskItem(id: string): TaskItem {
  return { id, name: id, description: "", updated_at: 0 } as unknown as TaskItem;
}

function scriptItem(id: string): Script {
  return { id, name: id, description: "", updated_at: 0 } as unknown as Script;
}

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(CommunityTasksEntry);
  app.mount(container);
}

function guide(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".cte-guide");
}

beforeEach(() => {
  document.body.innerHTML = "";
  localStorage.clear();
  showRepoImport.mockReset();
  loaded.value = false;
  browserTasks.value = [];
  httpTasks.value = [];
  scripts.value = [];
});

afterEach(() => {
  app?.unmount();
  app = null;
});

describe("引导气泡出现口径", () => {
  it("只有内置 default 任务且目录已加载时显示", async () => {
    loaded.value = true;
    browserTasks.value = [taskItem("default")];
    mount();
    await flush();
    expect(guide()).not.toBeNull();
  });

  it("存在用户自建任务时不显示", async () => {
    loaded.value = true;
    browserTasks.value = [taskItem("default"), taskItem("scu")];
    mount();
    await flush();
    expect(guide()).toBeNull();
  });

  it("HTTP 任务或脚本非空时不显示", async () => {
    loaded.value = true;
    browserTasks.value = [taskItem("default")];
    httpTasks.value = [taskItem("http_one")];
    mount();
    await flush();
    expect(guide()).toBeNull();

    app?.unmount();
    httpTasks.value = [];
    scripts.value = [scriptItem("py_one")];
    mount();
    await flush();
    expect(guide()).toBeNull();
  });

  it("目录未加载时不显示（防数据未到先闪气泡）", async () => {
    loaded.value = false;
    browserTasks.value = [taskItem("default")];
    mount();
    await flush();
    expect(guide()).toBeNull();
  });

  it("localStorage 已记忆关闭后不再显示", async () => {
    localStorage.setItem("community-tasks-guide-dismissed", "1");
    loaded.value = true;
    browserTasks.value = [taskItem("default")];
    mount();
    await flush();
    expect(guide()).toBeNull();
  });

  it("localStorage 读取不可用（隐私模式等抛异常）时按未引导处理，静默降级", async () => {
    const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage blocked");
    });
    loaded.value = true;
    browserTasks.value = [taskItem("default")];
    mount();
    await flush();
    expect(guide()).not.toBeNull();
    getItem.mockRestore();
  });
});

describe("关闭路径", () => {
  beforeEach(() => {
    loaded.value = true;
    browserTasks.value = [taskItem("default")];
  });

  it("点 ✕ 关闭并写入 localStorage", async () => {
    mount();
    await flush();
    expect(guide()).not.toBeNull();
    const close = document.querySelector<HTMLButtonElement>(".cte-guide-close");
    expect(close, "找不到关闭按钮").not.toBeNull();
    close!.click();
    await flush();
    expect(guide()).toBeNull();
    expect(localStorage.getItem("community-tasks-guide-dismissed")).toBe("1");
  });

  it("点按钮打开任务库并顺手收掉气泡", async () => {
    mount();
    await flush();
    const open = document.querySelector<HTMLButtonElement>(".cte-anchor > button");
    expect(open, "找不到入口按钮").not.toBeNull();
    open!.click();
    await flush();
    expect(showRepoImport).toHaveBeenCalledTimes(1);
    expect(guide()).toBeNull();
    expect(localStorage.getItem("community-tasks-guide-dismissed")).toBe("1");
  });
});
