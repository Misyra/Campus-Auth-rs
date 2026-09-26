/**
 * 任务仓库双镜像测速（首次启动向导用）。
 *
 * 对两个预设镜像源（GitHub / Gitee）的索引地址**并行**各发起一次真实拉取并计时，
 * 取「成功且最快」的源作为向导本次会话的导入源。走的是与仓库导入弹窗同一条链路
 * （`GET /api/repo/fetch` 后端代理，15s 上限），耗时为绝对值但双源同口径，相对
 * 快慢即可代表当前网络下哪个源可用。
 *
 * `pickFastestSource` 是纯函数便于单测；拉取本身依赖 repoApi（经后端代理，带
 * 鉴权与 SSRF 防护），前端直连 raw 域会被 CORS 拦下，不能绕开后端测。
 */

import type { RepoTask } from "../api/types";
import { repoApi } from "../api";
import { extractApiError } from "../api/client";
import { presetRepoIndexUrl, TASK_REPO_SOURCES, type TaskRepoKind, type TaskRepoSourceId } from "./constants";

/** 单个源的测速结果：`ms` 为 null 表示该源失败（原因在 `error`） */
export interface RepoSourceTiming {
  source: TaskRepoSourceId;
  /** 拉取耗时（毫秒）；失败为 null */
  ms: number | null;
  /** 失败原因（成功为空串） */
  error: string;
  /** 成功时带回的索引条目（向导直接复用，不再对胜出源重复拉一次） */
  tasks: RepoTask[];
}

/** 单源计时拉取：失败不抛（结果进 error），保证双源结果齐平、模板无需处理 Promise 拒绝 */
async function timeSource(kind: TaskRepoKind, source: TaskRepoSourceId): Promise<RepoSourceTiming> {
  const url = presetRepoIndexUrl(kind, source);
  const started = Date.now();
  try {
    const data = await repoApi.fetchIndex(url);
    // 索引必须是 JSON 数组（与 useRepoImport.fetchRepoIndex 同一校验口径）：
    // 格式不对按失败处理，否则关键词过滤会对非数组取 filter 而抛错
    if (!Array.isArray(data)) {
      return { source, ms: null, error: "索引格式不正确（应为 JSON 数组）", tasks: [] };
    }
    return { source, ms: Date.now() - started, error: "", tasks: data };
  } catch (e) {
    return { source, ms: null, error: extractApiError(e, "加载失败"), tasks: [] };
  }
}

/**
 * 并行测速两个预设镜像源。
 *
 * 只测 `TASK_REPO_SOURCES` 里带预设索引地址的源（GitHub / Gitee）；自定义源
 * 需要用户手填地址，不属于"自动选一个可用源"的范畴，向导不测它。
 */
export async function measureRepoSources(kind: TaskRepoKind): Promise<RepoSourceTiming[]> {
  const mirrors = TASK_REPO_SOURCES.filter((s) => s.indexUrls).map((s) => s.id);
  return Promise.all(mirrors.map((source) => timeSource(kind, source)));
}

/** 从测速结果选「成功且最快」的源；全部失败返回 null（向导据此提示离线并回退默认任务） */
export function pickFastestSource(results: RepoSourceTiming[]): TaskRepoSourceId | null {
  let bestMs = Number.POSITIVE_INFINITY;
  let bestSource: TaskRepoSourceId | null = null;
  for (const r of results) {
    if (r.ms === null) continue;
    if (r.ms < bestMs) {
      bestMs = r.ms;
      bestSource = r.source;
    }
  }
  return bestSource;
}
