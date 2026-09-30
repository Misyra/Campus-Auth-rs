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
  try {
    const saved = localStorage.getItem(key);
    if (!saved) return fallback;
    const parsed: unknown = JSON.parse(saved);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return fallback;
    return { ...(fallback as object), ...parsed } as T;
  } catch (error) {
    frontendLogger.debug("appearance", "本地外观配置损坏，已重置", error);
    try { localStorage.removeItem(key); } catch { /* 禁用存储时继续使用内存中的偏好。 */ }
    return fallback;
  }
}

/** 存储被禁用或空间不足时仍允许即时预览，避免设置变化打断界面。 */
export function saveStored(key: string, value: unknown): boolean {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch (error) {
    frontendLogger.debug("appearance", "本地外观配置未能保存", error);
    return false;
  }
}
