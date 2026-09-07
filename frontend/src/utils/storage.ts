/**
 * localStorage 读写工具。
 * 从 useAppearance / useCustomColors 抽出：两处原为逐字重复的 loadStored 实现。
 */

import { frontendLogger } from "./logger";

/**
 * 从 localStorage 读取 JSON 配置并与 fallback 浅合并（存储值为空时直接返回 fallback）。
 * 值损坏（JSON 解析失败）时移除该键并返回 fallback，避免坏数据常驻。
 * 日志域固定为 "appearance"：当前仅外观域配置（useAppearance / useCustomColors）使用本函数。
 */
export function loadStored<T>(key: string, fallback: T): T {
  const saved = localStorage.getItem(key);
  if (!saved) return fallback;
  try {
    return { ...(fallback as object), ...JSON.parse(saved) } as T;
  } catch (error) {
    frontendLogger.debug("appearance", "本地外观配置损坏，已重置", error);
    localStorage.removeItem(key);
    return fallback;
  }
}
