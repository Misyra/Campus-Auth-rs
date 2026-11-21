/**
 * useWebSocket 重连调度的单元测试。
 * 覆盖：首次建连走缓存 token、重连前强制刷新、失败计数递增、
 * retryNow 归零、连接成功复位与重连回调；P2-7 新增：
 * - 建连即拒（unauthorized）连续出现不翻牌成 unreachable；
 * - 连上过再断（传输中断）归类 unreachable 且阈值后保持；
 * - 鉴权就绪事件驱动：unauthorized 退避中 token 后到立即重连（带 5s 冷却）；
 * - 鉴权门：引导未完成（token=null）不发起匿名建连，挂起等待就绪事件。
 *
 * WebSocket/window/定时器全部打桩，不触网络。模块为单例（connecting/
 * retryCount/就绪订阅常驻），全链路放在单个顺序用例内，避免多用例间状态串扰；
 * 每个建连后必达 onopen/onclose 终态，不让 connecting 悬空。末尾 destroy 收尾。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const ensureMock = vi.fn(async (): Promise<string | null> => "cached-token");
// refresh 的返回类型允许 null：鉴权门用例里要模拟「引导失败拿不到 token」
const refreshMock = vi.fn(async (): Promise<string | null> => "fresh-token");
const fetchStatusMock = vi.fn(async () => {});
const fetchLogsMock = vi.fn(async () => {});
/** 捕获 onAuthReady 登记的监听器，测试里手动触发「token 后到」事件 */
const authReadyListeners: Array<(token: string) => void> = [];

vi.mock("../api/client", () => ({
  // 桩一律无参转发：被测代码不传参，且裸 vi.fn() 摊开 unknown[] 会触发 TS2556
  ensureAuthToken: () => ensureMock(),
  refreshAuthToken: () => refreshMock(),
  onAuthReady: (listener: (token: string) => void) => {
    authReadyListeners.push(listener);
    return () => {
      const i = authReadyListeners.indexOf(listener);
      if (i !== -1) authReadyListeners.splice(i, 1);
    };
  },
}));

vi.mock("./useStatus", () => ({
  useStatus: () => ({
    fetchStatus: () => fetchStatusMock(),
    updateStatus: vi.fn(),
  }),
}));

vi.mock("./useLogs", () => ({
  useLogs: () => ({
    fetchLogs: () => fetchLogsMock(),
    appendLogs: vi.fn(),
    autoScroll: { value: true },
  }),
}));

vi.mock("./useDebug", () => ({
  useDebug: () => ({
    handleScreenshot: vi.fn(),
    handleStepProgress: vi.fn(),
  }),
}));

class FakeSocket {
  static last: FakeSocket | null = null;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: ((e: unknown) => void) | null = null;
  readyState = 1;
  close = vi.fn();
  constructor(public url: string) {
    FakeSocket.last = this;
  }
}
vi.stubGlobal("WebSocket", FakeSocket as unknown as typeof WebSocket);
vi.stubGlobal("window", {
  location: { protocol: "http:", host: "127.0.0.1:50721" },
});
// node 环境无 document：本文件不调用 setupVisibilityChange，不打桩，
// 避免残缺 document 盖掉导入链（vue runtime-dom 导入期即需 createElement）

const { useWebSocket } = await import("./useWebSocket");
const wsMgr = useWebSocket();

/** 等待 connectWebSocket 内的 token await 落定、FakeSocket 建好 */
async function flushConnect(): Promise<void> {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

/** 触发当前 socket 断开并推进计时器到下一跳建连完成（不 onopen = 建连即拒） */
async function failAndNext(): Promise<void> {
  FakeSocket.last?.onclose?.();
  await vi.runOnlyPendingTimersAsync();
  await flushConnect();
}

/** 推进到下一跳并完整走一次「连上 → 断开」（会话中断形态） */
async function openAndDrop(): Promise<void> {
  await vi.runOnlyPendingTimersAsync();
  await flushConnect();
  FakeSocket.last?.onopen?.();
  FakeSocket.last?.onclose?.();
}

/** 触发一次鉴权就绪事件（token 后到）并等建连落定 */
async function fireAuthReady(): Promise<void> {
  for (const listener of [...authReadyListeners]) listener("late-token");
  await flushConnect();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useWebSocket 重连调度", () => {
  it("建连→分类→就绪重启→鉴权门完整链路", async () => {
    // 1) 首次建连走缓存 token
    await wsMgr.connectWebSocket();
    await flushConnect();
    expect(ensureMock).toHaveBeenCalled();
    expect(refreshMock).not.toHaveBeenCalled();
    expect(FakeSocket.last?.url).toContain("token=cached-token");

    // 2) 第一次建连被拒：计数 0，首轮分类 unauthorized
    FakeSocket.last?.onclose?.();
    expect(wsMgr.wsRetryCount.value).toBe(0);
    expect(wsMgr.wsDisconnectReason.value).toBe("unauthorized");
    expect(wsMgr.wsReconnecting.value).toBe(true);

    // 3) 下一跳重连前强制刷新 token（后端重启场景一次拿到新 token）
    await vi.runOnlyPendingTimersAsync();
    await flushConnect();
    expect(refreshMock).toHaveBeenCalled();
    expect(FakeSocket.last?.url).toContain("token=fresh-token");

    // 4) 再被拒两次，计数递增；P2-7：unauthorized 不在阈值处翻牌成
    //    unreachable——「请检查后端是否已启动」只指向真传输失败
    await failAndNext();
    expect(wsMgr.wsRetryCount.value).toBe(1);
    await failAndNext();
    expect(wsMgr.wsRetryCount.value).toBe(2);
    expect(wsMgr.wsDisconnectReason.value).toBe("unauthorized");
    await failAndNext();
    expect(wsMgr.wsDisconnectReason.value).toBe("unauthorized");

    // 5) retryNow 归零计数并立即建连（不等退避）
    wsMgr.retryNow();
    await flushConnect();
    expect(wsMgr.wsRetryCount.value).toBe(0);
    expect(FakeSocket.last).not.toBeNull();

    // 6) 首次建连成功：复位状态但不触发重连回调（wasConnected 尚为 false）
    const cb = vi.fn();
    const off = wsMgr.onWsReconnect(cb);
    fetchStatusMock.mockClear();
    FakeSocket.last?.onopen?.();
    expect(wsMgr.wsReconnecting.value).toBe(false);
    expect(wsMgr.wsDisconnectReason.value).toBeNull();
    expect(wsMgr.wsRetryCount.value).toBe(0);
    expect(cb).not.toHaveBeenCalled();

    // 7) 连上过再断 = 会话中断：分类 unreachable（P2-7），连续中断达阈值保持 unreachable
    FakeSocket.last?.onclose?.();
    expect(wsMgr.wsDisconnectReason.value).toBe("unreachable");
    await openAndDrop();
    expect(wsMgr.wsDisconnectReason.value).toBe("unreachable");
    await openAndDrop();
    expect(wsMgr.wsDisconnectReason.value).toBe("unreachable");

    // 8) 重连成功：触发重连回调与数据补拉
    await vi.runOnlyPendingTimersAsync();
    await flushConnect();
    FakeSocket.last?.onopen?.();
    expect(fetchStatusMock).toHaveBeenCalled();
    expect(cb).toHaveBeenCalled();
    off();

    // 9) P2-7 就绪事件驱动：回到 unauthorized 退避后 token 后到 → 立即重连
    FakeSocket.last?.onclose?.(); // 会话中断 → unreachable，退避计时器挂起
    await vi.runOnlyPendingTimersAsync();
    await flushConnect();
    FakeSocket.last?.onclose?.(); // 退避到点建连即拒 → unauthorized
    expect(wsMgr.wsDisconnectReason.value).toBe("unauthorized");
    vi.setSystemTime(1_000_000); // 钉死时钟：冷却窗口断言可复现
    const beforeRestart = FakeSocket.last;
    await fireAuthReady();
    expect(FakeSocket.last).not.toBe(beforeRestart); // 不推进计时器即完成重连

    // 新 token 仍被拒（后端反复发同款被拒 token 的异常窗口）：冷却窗口内就绪事件不再触发重连
    FakeSocket.last?.onclose?.();
    const atCooldown = FakeSocket.last;
    await fireAuthReady();
    expect(FakeSocket.last).toBe(atCooldown);

    // 10) 鉴权门：退避建连时引导失败（refresh 返回 null）→ 不发起匿名建连，挂起等待就绪事件
    refreshMock.mockImplementationOnce(async () => null);
    await vi.runOnlyPendingTimersAsync();
    await flushConnect();
    expect(FakeSocket.last).toBe(atCooldown); // 没有任何新 socket 被创建
    // token 后到：就绪事件解除挂起并带新凭据建连
    await fireAuthReady();
    expect(FakeSocket.last).not.toBe(atCooldown);
    expect(FakeSocket.last?.url).toContain("token=fresh-token");

    wsMgr.destroy();
  });
});
