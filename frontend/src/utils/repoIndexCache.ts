/** 任务仓库索引的本地缓存：按完整 URL 区分来源，两小时后重新获取。 */

import type { RepoTask } from "../api/types";
import { isRepoTaskIndex } from "./repoTask";

const STORAGE_KEY = "campus-auth.repo-index-cache.v1";
const MAX_ENTRIES = 8;
export const REPO_INDEX_CACHE_TTL_MS = 2 * 60 * 60 * 1000;

interface CacheEntry {
  url: string;
  fetchedAt: number;
  tasks: RepoTask[];
}

/** 读取结构有效的缓存条目；存储不可用时退化为每次请求。 */
function readEntries(): CacheEntry[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((item): item is CacheEntry =>
      item !== null && typeof item === "object"
      && typeof item.url === "string"
      && typeof item.fetchedAt === "number"
      && Number.isFinite(item.fetchedAt)
      && isRepoTaskIndex(item.tasks),
    );
  } catch {
    return [];
  }
}

/** 仅返回仍在两小时有效期内的索引，空数组也算一次成功缓存。 */
export function readRepoIndexCache(url: string, now = Date.now()): CacheEntry | null {
  const entry = readEntries().find((item) => item.url === url);
  if (!entry || entry.fetchedAt > now || now - entry.fetchedAt >= REPO_INDEX_CACHE_TTL_MS) return null;
  return entry;
}

/** 成功拉取后写入缓存；配额不足或禁用存储时不影响界面使用。 */
export function writeRepoIndexCache(url: string, tasks: RepoTask[], now = Date.now()): void {
  const entries = readEntries()
    .filter((item) => item.url !== url && item.fetchedAt <= now && now - item.fetchedAt < REPO_INDEX_CACHE_TTL_MS)
    .slice(-(MAX_ENTRIES - 1));
  entries.push({ url, fetchedAt: now, tasks });
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // 浏览器禁用存储或配额已满时直接使用本次网络结果。
  }
}
