/**
 * 调试会话状态管理 composable 的单元测试。
 * 覆盖刷新恢复的骨架退化与详情退避补全（refillSessionDetails）、停止时取消补全。
 *
 * useDebug 是模块级单例：同一文件内的用例共享状态，用例按声明顺序执行。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { DebugSession } from "../api/types";

// debugApi 全部打桩：status 的返回序列由各用例自行设置
const statusMock = vi.fn();
const stopMock = vi.fn(async () => ({}));

vi.mock("../api", () => ({
  debugApi: {
    start: vi.fn(async () => fullSession()),
    next: vi.fn(),
    runAll: vi.fn(),
    stop: () => stopMock(),
    status: () => statusMock(),
  },
}));

const { useDebug } = await import("./useDebug");

const debug = useDebug();

function fullSession(overrides: Partial<DebugSession> = {}): DebugSession {
  return {
    running: false,
    task_id: "t1",
    current_step: 5,
    total_steps: 5,
    steps: [
      { type: "navigate", description: "打开页面" },
      { type: "fill", description: "输入账号" },
      { type: "fill", description: "输入密码" },
      { type: "ocr", description: "识别验证码" },
      { type: "click", description: "点击登录" },
    ],
    results: [],
    screenshot_url: null,
    ...overrides,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  statusMock.mockReset();
  stopMock.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("刷新恢复与详情补全", () => {
  it("恢复时拿不到会话详情则退化为骨架，并按退避补全", async () => {
    // 首查：会话活跃但 Worker 忙（无 session 负载）→ 骨架
    statusMock.mockResolvedValueOnce({ active: true, screenshot_url: null });
    await debug.restoreIfActive();
    expect(debug.session.running).toBe(true);
    expect(debug.session.steps).toEqual([]);
    expect(debug.visible.value).toBe(true);

    // 第一次补全（+5s）：仍在执行，无详情 → 保持骨架，排下一轮
    statusMock.mockResolvedValueOnce({ active: true, screenshot_url: null });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(debug.session.steps).toEqual([]);
    expect(statusMock).toHaveBeenCalledTimes(2);

    // 第二次补全（+5s+15s）："执行全部"已跑完，返回完整会话 → 步骤整体就位
    statusMock.mockResolvedValueOnce({ active: true, screenshot_url: null, session: fullSession() });
    await vi.advanceTimersByTimeAsync(15_000);
    expect(debug.session.steps.length).toBe(5);
    expect(debug.session.running).toBe(false);
  });

  it("补全期间会话被外部结束时清理面板", async () => {
    // 先制造骨架态
    debug.stopDebug();
    await Promise.resolve();
    statusMock.mockResolvedValueOnce({ active: true, screenshot_url: null });
    await debug.restoreIfActive();
    expect(debug.visible.value).toBe(true);

    // 重试时会话已不存在 → 清空并收起面板，不再继续排程
    statusMock.mockResolvedValueOnce({ active: false });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(debug.visible.value).toBe(false);
    expect(debug.session.running).toBe(false);
    const calls = statusMock.mock.calls.length;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(statusMock.mock.calls.length).toBe(calls);
  });

  it("停止调试会取消未触发的补全定时器", async () => {
    statusMock.mockResolvedValueOnce({ active: true, screenshot_url: null });
    await debug.restoreIfActive();
    const calls = statusMock.mock.calls.length;

    debug.stopDebug();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(statusMock.mock.calls.length).toBe(calls);
    expect(debug.session.steps).toEqual([]);
  });

  it("startDebug 清空上一场残留截图（避免裂图）", async () => {
    debug.session.screenshot_url = "http://127.0.0.1:50721/api/debug/screenshots/stale.png";
    await debug.startDebug("t1");
    // start 响应的 screenshot_url 恒为 null（截图只经 WS 推送），不得保留残留 URL
    expect(debug.session.screenshot_url).toBe(null);
    expect(debug.session.steps.length).toBe(5);
    expect(debug.visible.value).toBe(true);
  });
});
