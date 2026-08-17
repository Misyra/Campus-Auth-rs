/**
 * 日志流（单例）。
 * 替代原 dashboardData.logs + app-options.filteredLogs + uiMethods._appendLogs。
 * 日志通过 WebSocket 由 useWebSocket 推入（追加 + 去重）；
 * HTTP 历史拉取为整体替换（重建去重基准），避免重复累积。
 */

import { reactive, ref, computed, watch } from "vue";
import type { LogEntry } from "../api/types";
import { systemApi } from "../api";
import { LEVEL_VALUES, LIMITS } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { debounce } from "../utils/debounce";

const logs = reactive<LogEntry[]>([]);
const logFilter = reactive({ level: "INFO", source: "", search: "" });
const autoScroll = ref(true);
const newLogCount = ref(0);
// 日志是否已加载（控制首次 HTTP 拉取后再启用 WS 实时追加，避免历史与实时乱序）
const initialized = ref(false);

// P16 混合去重策略：
// - WS 实时路径：仅按全局单调 seq 去重（seq 重复才是真重复，
//   同毫秒同文案的两条真实日志不再被内容键误丢），见 seenSeqs；
// - 历史合并/无 seq 回退路径：按内容键（timestamp+message+source）去重，
//   防止 WS 已收到的与 /api/logs 文件读取的重复（历史 seq 每次请求重分配，不可用 seq 比较），见 seenKeys。
const seenSeqs = new Set<number>();
const seenKeys = new Set<string>();

// F7：高频日志微任务批量合并。同一事件循环 tick 内到达的多条 WS 日志先进入
// 待刷新缓冲，在下一个微任务统一写入 `logs`（一次 reactive 更新），
// 避免每消息一次 push 造成渲染压力。去重在入队时完成，顺序语义不变。
const pendingLogs: LogEntry[] = [];
let pendingNotAtBottom = 0;
let flushScheduled = false;

function logKey(e: LogEntry): string {
  return `${e.timestamp}|${e.message}|${e.source}`;
}

// P11：搜索词 300ms 防抖。logFilter.search 保持"当前输入"语义（输入框即时回显），
// filteredLogs 改用防抖后的搜索词，避免高频按键每字符触发一次全量过滤。
const debouncedSearch = ref(logFilter.search);
const applySearchDebounced = debounce((v: string) => {
  debouncedSearch.value = v;
}, 300);
watch(
  () => logFilter.search,
  (v) => applySearchDebounced(v),
);

const filteredLogs = computed(() => {
  const { level, source } = logFilter;
  const q = debouncedSearch.value ? debouncedSearch.value.toLowerCase() : "";
  const minLevel = LEVEL_VALUES[level] ?? 0;
  return logs.filter(
    (l) =>
      (!level || (LEVEL_VALUES[l.level] ?? 0) >= minLevel) &&
      (!source || l.source === source) &&
      (!q || l.message.toLowerCase().includes(q)),
  );
});

/** 从后端拉取历史日志（整体替换，并重建去重基准） */
async function fetchLogs(limit = LIMITS.LOG_MAX_ENTRIES): Promise<void> {
  try {
    const entries = await systemApi.fetchLogs(limit);
    if (Array.isArray(entries)) {
      replaceLogs(entries);
    }
  } catch (error) {
    frontendLogger.error("logs", "获取日志失败", error);
  }
}

/** 整体替换日志数组（HTTP 历史拉取），并重建去重键集合 */
function replaceLogs(entries: LogEntry[]): void {
  seenKeys.clear();
  seenSeqs.clear();
  pendingLogs.length = 0;
  pendingNotAtBottom = 0;
  // 历史条目 seq 每次请求重新分配（不跨请求稳定），基准只按内容键重建；
  // WS 新日志 seq 全局单调递增，不会与后续实时推送撞 seq
  for (const e of entries) seenKeys.add(logKey(e));
  logs.splice(0, logs.length, ...entries);
  initialized.value = true;
}

/** 刷新待写入缓冲：统一 push、裁剪上限、累加“新消息”计数 */
function flushPendingLogs(): void {
  flushScheduled = false;
  if (!pendingLogs.length) return;
  logs.push(...pendingLogs);
  pendingLogs.length = 0;
  if (logs.length > LIMITS.LOG_MAX_ENTRIES) {
    const overflow = logs.length - LIMITS.LOG_MAX_ENTRIES;
    logs.splice(0, overflow);
    // 裁剪后若 seen 集合过大则重建，防止内存无限增长
    if (seenKeys.size > LIMITS.LOG_MAX_ENTRIES * 2) {
      const keep = logs.map((l) => logKey(l));
      seenKeys.clear();
      keep.forEach((k) => seenKeys.add(k));
    }
    if (seenSeqs.size > LIMITS.LOG_MAX_ENTRIES * 2) {
      seenSeqs.clear();
      for (const l of logs) {
        if (typeof l.seq === "number") seenSeqs.add(l.seq);
      }
    }
  }
  // 不在底部时累计“新消息”计数，供前端“N 条新消息”提示
  if (pendingNotAtBottom > 0) {
    newLogCount.value += pendingNotAtBottom;
    pendingNotAtBottom = 0;
  }
}

/** 追加日志（WebSocket 实时推入）。atBottom 表示当前是否已滚动到底部。 */
function appendLogs(entries: LogEntry[], atBottom = true): void {
  // HTTP 历史未就绪前丢弃 WS 实时日志，避免与历史拉取乱序（由 fetchLogs 重建基准）
  if (!initialized.value) return;
  // P16 混合去重：有 seq 仅按 seq（真重复）；缺 seq（旧后端/本地构造条目）回退内容键
  for (const e of entries) {
    if (typeof e.seq === "number") {
      if (seenSeqs.has(e.seq)) continue;
      seenSeqs.add(e.seq);
    } else {
      const key = logKey(e);
      if (seenKeys.has(key)) continue;
      seenKeys.add(key);
    }
    pendingLogs.push(e);
    if (!atBottom) pendingNotAtBottom++;
  }
  // 微任务批量合并：同一 tick 内多条日志只触发一次写入
  if (!flushScheduled && pendingLogs.length > 0) {
    flushScheduled = true;
    queueMicrotask(flushPendingLogs);
  }
}

function clearLogs(): void {
  logs.splice(0, logs.length);
  pendingLogs.length = 0;
  pendingNotAtBottom = 0;
  seenKeys.clear();
  seenSeqs.clear();
  newLogCount.value = 0;
}

export function useLogs() {
  return { logs, logFilter, autoScroll, newLogCount, initialized, filteredLogs, fetchLogs, appendLogs, clearLogs };
}
