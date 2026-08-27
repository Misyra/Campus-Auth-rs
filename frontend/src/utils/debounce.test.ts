/**
 * 防抖与 rAF 节流的单元测试（fake timers，不依赖真实时钟）。
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { debounce, throttleRaf } from "./debounce";

afterEach(() => {
  vi.useRealTimers();
});

describe("debounce", () => {
  it("间隔内多次调用只执行最后一次", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const debounced = debounce(fn, 100);

    debounced("a");
    vi.advanceTimersByTime(50);
    debounced("b");
    vi.advanceTimersByTime(50);
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(50);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith("b");
  });

  it("cancel 阻止待执行调用", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const debounced = debounce(fn, 100);

    debounced();
    debounced.cancel();
    vi.advanceTimersByTime(200);
    expect(fn).not.toHaveBeenCalled();
  });
});

describe("throttleRaf", () => {
  it("同一帧内的多次触发合并为一次执行", () => {
    // node 环境无 rAF，用 setTimeout 模拟一帧
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) =>
      setTimeout(() => cb(0), 16),
    );
    vi.useFakeTimers();
    const fn = vi.fn();
    const throttled = throttleRaf(fn);

    throttled();
    throttled();
    throttled();
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(16);
    expect(fn).toHaveBeenCalledTimes(1);
  });
});
