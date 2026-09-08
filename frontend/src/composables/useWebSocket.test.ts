/**
 * useWebSocket 重连调度的单元测试。
 * 覆盖：首次建连走缓存 token、重连前强制刷新、失败计数递增与
 * unreachable 标记、retryNow 归零、连接成功复位与重连回调。
 *
 * WebSocket/window/定时器全部打桩，不触网络。模块为单例（connecting/
 * retryCount 常驻），全链路放在单个顺序用例内，避免多用例间状态串扰；
 * 每个建连后必达 onopen/onclose 终态，不让 connecting 悬空。末尾 destroy 收尾。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const ensureMock = vi.fn(async () => "cached-token");
const refreshMock = vi.fn(async () => "fresh-token");
const fetchStatusMock = vi.fn(async () => {});
const fetchLogsMock = vi.fn(async () => {});

vi.mock("../api/client", () => ({
  ensureAuthToken: (...a: unknown[]) => ensureMock(...a),
  refreshAuthToken: (...a: unknown[]) => refreshMock(...a),
}));

vi.mock("./useStatus", () => ({
  useStatus: () => ({
    fetchStatus: (...a: unknown[]) => fetchStatusMock(...a),
    updateStatus: vi.fn(),
  }),
}));

vi.mock("./useLogs", () => ({
  useLogs: () => ({
    fetchLogs: (...a: unknown[]) => fetchLogsMock(...a),
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

/** 触发当前 socket 断开并推进计时器到下一跳建连完成 */
async function failAndNext(): Promise<void> {
  FakeSocket.last?.onclose?.();
  await vi.runOnlyPendingTimersAsync();
  await flushConnect();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useWebSocket 重连调度", () => {
  it("建连→失败退避→retryNow→重连成功完整链路", async () => {
    // 1) 首次建连走缓存 token
    await wsMgr.connectWebSocket();
    await flushConnect();
    expect(ensureMock).toHaveBeenCalled();
    expect(refreshMock).not.toHaveBeenCalled();
    expect(FakeSocket.last?.url).toContain("token=cached-token");

    // 2) 第一次握手失败：计数 0，首轮分类 unauthorized
    FakeSocket.last?.onclose?.();
    expect(wsMgr.wsRetryCount.value).toBe(0);
    expect(wsMgr.wsDisconnectReason.value).toBe("unauthorized");
    expect(wsMgr.wsReconnecting.value).toBe(true);

    // 3) 下一跳重连前强制刷新 token（后端重启场景一次拿到新 token）
    await vi.runOnlyPendingTimersAsync();
    await flushConnect();
    expect(refreshMock).toHaveBeenCalled();
    expect(FakeSocket.last?.url).toContain("token=fresh-token");

    // 4) 再失败两次，计数递增并在第 3 次标记 unreachable
    await failAndNext();
    expect(wsMgr.wsRetryCount.value).toBe(1);
    await failAndNext();
    expect(wsMgr.wsRetryCount.value).toBe(2);
    expect(wsMgr.wsDisconnectReason.value).toBe("unreachable");

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

    // 7) 再次断开→重连→成功：触发重连回调与数据补拉
    await failAndNext();
    FakeSocket.last?.onopen?.();
    expect(fetchStatusMock).toHaveBeenCalled();
    expect(cb).toHaveBeenCalled();
    off();
    wsMgr.destroy();
  });
});
