/**
 * 日志流（单例）。
 * 替代原 dashboardData.logs + app-options.filteredLogs + uiMethods._appendLogs。
 * 日志通过 WebSocket 由 useWebSocket 推入（追加 + 去重）；
 * HTTP 历史拉取会重建历史基准，同时保留请求期间到达的实时日志，避免刷新倒退。
 */

import { reactive, ref, computed, watch } from "vue";
import type { LogEntry } from "../api/types";
import { systemApi } from "../api";
import { LEVEL_VALUES, LIMITS } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { debounce } from "../utils/debounce";

const logs = reactive<LogEntry[]>([]);
// 默认展示全部已接收日志；日志级别由用户筛选，避免历史接口与实时流出现“刷新后少日志”。
const logFilter = reactive({ level: "", source: "", search: "" });
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
// 首次历史请求完成前到达的实时日志不能直接丢弃；历史替换完成后按内容键去重回放。
const pendingBeforeInit: LogEntry[] = [];
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
  // 记录请求开始时的序号；响应返回前产生的实时日志需要在历史替换后保留。
  const fetchStartedSeq = logs.reduce(
    (max, entry) => (typeof entry.seq === "number" ? Math.max(max, entry.seq) : max),
    0,
  );
  try {
    const entries = await systemApi.fetchLogs(limit);
    if (Array.isArray(entries)) {
      replaceLogs(entries, fetchStartedSeq);
    }
  } catch (error) {
    frontendLogger.error("logs", "获取日志失败", error);
    // 历史文件暂时不可读时仍开启实时流，避免整个日志面板一直停在空白状态。
    initialized.value = true;
    replayPendingBeforeInit();
  }
}

/** 历史替换或请求失败后回放初始化窗口内暂存的实时日志。 */
function replayPendingBeforeInit(): void {
  const queued = pendingBeforeInit.splice(0, pendingBeforeInit.length);
  for (const entry of queued) {
    // HTTP 历史与 WS 实时日志的 seq 来源不同，按内容键过滤启动窗口重复。
    if (!seenKeys.has(logKey(entry))) appendLogs([entry]);
  }
}

/** 用 HTTP 历史重建日志数组与去重键集合，并合并请求期间产生的实时日志。 */
function replaceLogs(entries: LogEntry[], preserveAfterSeq = 0): void {
  const realtimeDuringFetch =
    preserveAfterSeq > 0
      ? [...logs, ...pendingLogs].filter(
          (entry) => typeof entry.seq === "number" && entry.seq > preserveAfterSeq,
        )
      : [];
  seenKeys.clear();
  seenSeqs.clear();
  pendingLogs.length = 0;
  pendingNotAtBottom = 0;
  // 历史条目 seq 每次请求重新分配（不跨请求稳定），基准只按内容键重建；
  // WS 新日志 seq 全局单调递增，不会与后续实时推送撞 seq
  for (const e of entries) seenKeys.add(logKey(e));
  logs.splice(0, logs.length, ...entries);
  initialized.value = true;
  replayPendingBeforeInit();
  // 历史响应可能早于实时事件落盘，回放请求期间产生的日志，避免刷新造成数据倒退。
  for (const entry of realtimeDuringFetch) {
    if (!seenKeys.has(logKey(entry))) appendLogs([entry]);
  }
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
  // HTTP 历史未就绪前先缓存，历史替换后再回放，避免启动阶段漏日志。
  if (!initialized.value) {
    pendingBeforeInit.push(...entries);
    if (pendingBeforeInit.length > LIMITS.WS_LOG_BUFFER_MAX) {
      pendingBeforeInit.splice(0, pendingBeforeInit.length - LIMITS.WS_LOG_BUFFER_MAX);
    }
    return;
  }
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
  pendingBeforeInit.length = 0;
  pendingNotAtBottom = 0;
  seenKeys.clear();
  seenSeqs.clear();
  newLogCount.value = 0;
}

export function useLogs() {
  return { logs, logFilter, autoScroll, newLogCount, initialized, filteredLogs, fetchLogs, appendLogs, clearLogs };
}
