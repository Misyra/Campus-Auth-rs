/**
 * 横切异步守卫的单元测试：拉取节流、首败通知、busy id 集合。
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { createFetchGuard, createFirstFailNotifier, useBusyIds } from "./guards";

afterEach(() => {
  vi.useRealTimers();
});

describe("createFetchGuard", () => {
  it("首次放行，成功后窗口内跳过，force 绕过", () => {
    vi.useFakeTimers();
    const guard = createFetchGuard(5000);
    expect(guard.shouldFetch()).toBe(true);

    guard.markSuccess();
    expect(guard.shouldFetch()).toBe(false);
    expect(guard.shouldFetch(true)).toBe(true);
  });

  it("窗口边界：到期即放行，未到期继续跳过", () => {
    vi.useFakeTimers();
    const guard = createFetchGuard(5000);
    guard.markSuccess();

    vi.advanceTimersByTime(4999);
    expect(guard.shouldFetch()).toBe(false);

    vi.advanceTimersByTime(1);
    expect(guard.shouldFetch()).toBe(true);
  });
});

describe("createFirstFailNotifier", () => {
  it("仅首次失败返回 true，恢复后再次失败仍通知", () => {
    const notifier = createFirstFailNotifier();
    expect(notifier.trackFailure()).toBe(true);
    expect(notifier.trackFailure()).toBe(false);
    expect(notifier.trackFailure()).toBe(false);

    expect(notifier.trackRecovery()).toBe(true);
    expect(notifier.trackRecovery()).toBe(false);

    expect(notifier.trackFailure()).toBe(true);
  });
});

describe("useBusyIds", () => {
  it("按 id 加锁/解锁，模板可直接 has()", () => {
    const ids = useBusyIds();
    expect(ids.has("task-1")).toBe(false);

    ids.add("task-1");
    expect(ids.has("task-1")).toBe(true);

    ids.delete("task-1");
    expect(ids.has("task-1")).toBe(false);
  });
});
