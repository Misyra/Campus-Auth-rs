/** 仓库索引条目的运行时校验，网络响应与本地缓存共用。 */
import type { RepoTask } from "../api/types";
import { repoSourceUrl } from "./repoSource";
import type { TaskRepoKind } from "./constants";

/** 缺省类型兼容旧浏览器任务，未知类型不进入可导入列表。 */
export function repoTaskKind(type?: string): TaskRepoKind | "" {
  const value = (type ?? "").trim().toLowerCase();
  if (!value || value === "browser") return "browser";
  return value === "http" || value === "script" ? value : "";
}

/** 跨类别允许同 ID，选中态与缓存以类别和 ID 共同标识。 */
export function repoTaskKey(task: RepoTask): string {
  return `${repoTaskKind(task.type)}:${task.id}`;
}

/** 避免损坏的远端字段进入列表、搜索与截图渲染。 */
export function isRepoTask(value: unknown): value is RepoTask {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const item = value as Record<string, unknown>;
  return typeof item.id === "string" && !!item.id.trim()
    && typeof item.name === "string" && !!item.name.trim()
    && typeof item.url === "string" && !!repoSourceUrl(item.url)
    && ["description", "author", "type", "version", "source", "screenshot"].every(
      (key) => item[key] === undefined || typeof item[key] === "string",
    )
    && (item.tags === undefined || (Array.isArray(item.tags) && item.tags.every((tag) => typeof tag === "string")));
}

/** 同一类别中的 ID 必须唯一，否则选中态与详情无法对应。 */
export function isRepoTaskIndex(value: unknown): value is RepoTask[] {
  return Array.isArray(value) && value.every(isRepoTask)
    && new Set(value.map(repoTaskKey)).size === value.length;
}
