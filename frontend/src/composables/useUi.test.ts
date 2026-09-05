/**
 * 入口模块求值回归：useUi 被 main.ts 首屏导入，模块顶层引用未导入的
 * 全局（如漏写 `import { reactive } from "vue"`）会在生产包求值时抛
 * ReferenceError，导致整个应用白屏。vitest/node 下导入即复现。
 */
import { describe, it, expect, vi } from "vitest";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
  removeItem: () => {},
});
vi.stubGlobal("matchMedia", () => ({
  matches: false,
  addEventListener: () => {},
  removeEventListener: () => {},
}));
 vi.mock("../router", () => ({
  router: { push: vi.fn(), beforeEach: vi.fn() },
}));

vi.mock("../api", () => {
  const stub = new Proxy(
    {},
    {
      get: (_t, prop) =>
        (..._a: unknown[]) => {
          void prop;
          return Promise.resolve({});
        },
    },
  );
  return {
    systemApi: stub,
    browsersApi: stub,
    monitorApi: stub,
    actionsApi: stub,
    historyApi: stub,
    tasksApi: stub,
    scheduledTasksApi: stub,
    scriptsApi: stub,
    profilesApi: stub,
    configApi: stub,
  };
});

describe("useUi 模块求值", () => {
  it("导入不抛错且暴露 init", async () => {
    const mod = await import("./useUi");
    expect(typeof mod.useUi).toBe("function");
    expect(typeof mod.useUi().init).toBe("function");
  });
});
