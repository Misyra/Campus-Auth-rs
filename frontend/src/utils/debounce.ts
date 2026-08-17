/**
 * 防抖与 rAF 节流工具。
 */

/**
 * 防抖：延迟 ms 后执行最后一次调用；期间再次调用会重置计时。
 * 返回包装函数，附带 cancel() 用于组件卸载等场景取消未执行的调用。
 */
export function debounce<A extends unknown[]>(fn: (...args: A) => unknown, ms: number): ((...args: A) => void) & { cancel(): void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const wrapped = (...args: A): void => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      fn(...args);
    }, ms);
  };
  wrapped.cancel = (): void => {
    if (timer) clearTimeout(timer);
    timer = undefined;
  };
  return wrapped;
}

/**
 * rAF 节流：将同一帧内的多次调用合并为一次（在下一帧执行最后一次前调用的参数）。
 * 无 requestAnimationFrame 环境（如测试）降级为直接调用。
 */
export function throttleRaf<A extends unknown[]>(fn: (...args: A) => void): (...args: A) => void {
  let scheduled = false;
  let pendingArgs: A;
  return (...args: A): void => {
    pendingArgs = args;
    if (scheduled) return;
    scheduled = true;
    const run = (): void => {
      scheduled = false;
      fn(...pendingArgs);
    };
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(run);
    } else {
      run();
    }
  };
}
