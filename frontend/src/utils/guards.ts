/**
 * 横切异步守卫工具集合。
 * 收敛各 composable 中逐字重复的样板：拉取节流守卫、首败通知计数、busy id 集合。
 */

import { reactive } from "vue";

export interface FetchGuard {
  /** 判断本次拉取是否放行；force 为 true 时恒放行（变更后刷新 / WS 重连等显式场景） */
  shouldFetch(force?: boolean): boolean;
  /** 拉取成功后记录时间戳（失败不记录，便于下次立即重试） */
  markSuccess(): void;
}

/**
 * 创建「N 秒内已成功拉取则跳过」的守卫。
 * 语义（P15）：useUi.init 已拉全部数据后，View mount / 路由往返不再重复请求；
 * lastFetchAt 仅成功后更新（失败不更新以便重试）；
 * force: true 供变更后刷新 / 重连回调等显式刷新场景绕过守卫。
 */
export function createFetchGuard(windowMs = 5000): FetchGuard {
  let lastFetchAt = 0;
  return {
    shouldFetch(force = false) {
      return force || Date.now() - lastFetchAt >= windowMs;
    },
    markSuccess() {
      lastFetchAt = Date.now();
    },
  };
}

export interface FirstFailNotifier {
  /** 记录一次失败；返回 true 表示这是首个失败（应提示用户） */
  trackFailure(): boolean;
  /** 拉取成功：若此前处于失败状态返回 true（已恢复，可提示重连），并复位计数 */
  trackRecovery(): boolean;
}

/**
 * 创建「首次失败才通知」的计数器（F3）。
 * 后续失败保持静默（log-only），成功后复位，再次失败时仍会首次通知。
 */
export function createFirstFailNotifier(): FirstFailNotifier {
  let failCount = 0;
  return {
    trackFailure() {
      failCount += 1;
      return failCount === 1;
    },
    trackRecovery() {
      if (failCount === 0) return false;
      failCount = 0;
      return true;
    },
  };
}

/**
 * 创建响应式 busy id 集合（A11）。
 * 用于「执行中 / 复制中 / 导出中」等按 id 的连点守卫：
 * has/add/delete 均为响应式操作，模板中 `ids.has(id)` 可直接驱动按钮禁用态。
 */
export function useBusyIds(): Set<string> {
  return reactive(new Set<string>());
}
