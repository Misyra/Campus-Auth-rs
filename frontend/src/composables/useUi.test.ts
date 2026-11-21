// @vitest-environment happy-dom
/**
 * 入口模块求值回归 + init 轮询门行为测试。
 *
 * 求值回归：useUi 被 main.ts 首屏导入，模块顶层引用未导入的全局（如漏写
 * `import { reactive } from "vue"`）会在生产包求值时抛 ReferenceError，
 * 导致整个应用白屏。vitest/node 下导入即复现。
 *
 * init 轮询门（P2-7）：状态轮询排在「鉴权就绪」之后——有 token 即刻起表；
 * 引导态（token=null）不起表、就绪事件到达再起；WS 重连成功复位连败降级
 * （5min 慢轮询切回 30s，不等慢轮询自愈）。useUi 的 init 有单次守卫，
 * 两个场景经 vi.resetModules 各取全新模块实例。
 */
import { describe, it, expect, vi } from "vitest";

vi.stubGlobal("matchMedia", () => ({
  matches: false,
  addEventListener: () => {},
  removeEventListener: () => {},
}));

/** 跨模块重置存活的共享桩：vi.hoisted 保证先于 vi.mock 工厂求值 */
const mocks = vi.hoisted(() => ({
  ensure: vi.fn(),
  /** monitorApi.fetchStatus 桩（轮询目标；初始 allSettled 里也会被直接调一次） */
  fetchStatus: vi.fn(),
  /** useWebSocket.connectWebSocket 桩（本文件只验轮询门，WS 门在 useWebSocket.test.ts） */
  wsConnect: vi.fn(),
  /** client.onAuthReady 登记的监听器（测试里手动触发「token 后到」） */
  authReadyCbs: [] as Array<(token: string) => void>,
  /** useWebSocket.onWsReconnect 登记的重连回调（测试里手动模拟重连成功） */
  wsReconnectCbs: [] as Array<() => void | Promise<void>>,
}));

vi.mock("../router", () => ({
  router: { push: vi.fn(), beforeEach: vi.fn() },
}));

vi.mock("../api", () => {
  // 兜底桩：任意 api 方法一律解析 {}。init 链上的各 fetch 对空对象响应均容忍
  //（Array.isArray 守卫 / `data.x || 默认` 口径），个别方法再覆写。
  const anyFn = () => Promise.resolve({});
  const monitorApi = { fetchStatus: () => mocks.fetchStatus(), start: anyFn, stop: anyFn };
  const names = [
    "systemApi", "browsersApi", "actionsApi", "historyApi", "tasksApi",
    "scheduledTasksApi", "scriptsApi", "profilesApi", "configApi", "autostartApi",
    "pureModeApi", "backgroundApi", "debugApi", "repoApi", "configSessionApi",
    "uninstallApi", "ocrApi", "aiApi", "httpTasksApi", "workerApi", "notificationApi",
    "diagnosticsApi",
  ];
  const out: Record<string, unknown> = { monitorApi };
  for (const name of names) {
    out[name] = new Proxy({}, { get: () => anyFn });
  }
  return out;
});

vi.mock("../api/client", () => ({
  // ApiError/extractApiError 形状兼容真模块（useStatus/useProfiles 等按 instanceof
  // 与 message 消费）；ensureAuthToken/onAuthReady 是本文件要操纵的时序桩
  ApiError: class MockApiError extends Error {
    status?: number;
    constructor(message: string, status?: number) {
      super(message);
      this.name = "ApiError";
      this.status = status;
    }
  },
  extractApiError: (error: unknown, fallback = "操作失败") =>
    (error as { message?: string })?.message || fallback,
  isNoBrowserMessage: (m: unknown) => typeof m === "string" && m.includes("无可用浏览器"),
  ensureAuthToken: () => mocks.ensure(),
  onAuthReady: (listener: (token: string) => void) => {
    mocks.authReadyCbs.push(listener);
    return () => {
      const i = mocks.authReadyCbs.indexOf(listener);
      if (i !== -1) mocks.authReadyCbs.splice(i, 1);
    };
  },
}));

vi.mock("./useWebSocket", () => ({
  // useUi 只消费 connect/onWsReconnect/destroy 等；WS 自身的鉴权门与就绪重启
  // 在 useWebSocket.test.ts 单测（那里用真模块 + FakeSocket）
  useWebSocket: () => ({
    connectWebSocket: mocks.wsConnect,
    setupVisibilityChange: vi.fn(),
    destroy: vi.fn(),
    retryNow: vi.fn(),
    handleAuthReady: vi.fn(),
    onWsReconnect: (cb: () => void | Promise<void>) => {
      mocks.wsReconnectCbs.push(cb);
      return () => {};
    },
    wsReconnecting: { value: false },
    wsRetryCount: { value: 0 },
    wsDisconnectReason: { value: null },
  }),
}));

/** 微任务冲刷：init 的 allSettled 与 ensureAuthToken().then 都靠微任务落定 */
async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

/** 取全新模块实例的 composable 结果（init 单次守卫是模块级状态） */
async function freshUseUi() {
  const mod = await import("./useUi");
  return mod.useUi();
}

describe("useUi 模块求值", () => {
  it("导入不抛错且暴露 init", { timeout: 20_000 }, async () => {
    const mod = await import("./useUi");
    expect(typeof mod.useUi).toBe("function");
    expect(typeof mod.useUi().init).toBe("function");
  });
});

describe("useUi.init 轮询门（P2-7）", () => {
  it("有 token：轮询即刻起表，WS 重连成功复位连败降级", { timeout: 20_000 }, async () => {
    vi.resetModules();
    mocks.ensure.mockResolvedValue("tok");
    mocks.fetchStatus.mockResolvedValue({});
    mocks.wsConnect.mockClear();
    mocks.wsReconnectCbs.length = 0;
    mocks.authReadyCbs.length = 0;

    vi.useFakeTimers();
    const ui = await freshUseUi();
    await ui.init();
    await flushMicrotasks();

    // 初始 allSettled 已直调一次 fetchStatus；轮询起表后每 30s 一跳
    const baseline = mocks.fetchStatus.mock.calls.length;
    expect(baseline).toBeGreaterThanOrEqual(1);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.fetchStatus.mock.calls.length).toBe(baseline + 1);

    // 连败 3 次 → 轮询降级 5min：30s 粒度不再触发
    mocks.fetchStatus.mockRejectedValue(new Error("down"));
    for (let i = 0; i < 3; i++) {
      await vi.advanceTimersByTimeAsync(30_000);
    }
    const atSlow = mocks.fetchStatus.mock.calls.length;
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.fetchStatus.mock.calls.length).toBe(atSlow);

    // 模拟 WS 重连成功（onWsReconnect 回调）：降级立即复位回 30s（P2-7c）
    mocks.fetchStatus.mockResolvedValue({});
    expect(mocks.wsReconnectCbs.length).toBeGreaterThan(0);
    await mocks.wsReconnectCbs[0]!();
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.fetchStatus.mock.calls.length).toBe(atSlow + 1);

    vi.useRealTimers();
  });

  it("无 token：init 不起轮询，鉴权就绪事件到达后起表", { timeout: 20_000 }, async () => {
    vi.resetModules();
    mocks.ensure.mockResolvedValue(null);
    mocks.fetchStatus.mockClear();
    mocks.fetchStatus.mockResolvedValue({});
    mocks.wsConnect.mockClear();
    mocks.wsReconnectCbs.length = 0;
    mocks.authReadyCbs.length = 0;

    vi.useFakeTimers();
    const ui = await freshUseUi();
    await ui.init();
    await flushMicrotasks();

    // 引导态：轮询未起（初始 allSettled 的直调之外零请求），30s 后仍零
    const baseline = mocks.fetchStatus.mock.calls.length;
    await vi.advanceTimersByTimeAsync(30_000);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.fetchStatus.mock.calls.length).toBe(baseline);

    // token 后到（引导页成功 / 401 触发 local-token 重新引导）：就绪事件起表
    for (const listener of [...mocks.authReadyCbs]) listener("late-token");
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.fetchStatus.mock.calls.length).toBe(baseline + 1);

    vi.useRealTimers();
  });
});
