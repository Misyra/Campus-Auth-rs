/**
 * 日志流（单例）。
 * 替代原 dashboardData.logs + app-options.filteredLogs + uiMethods._appendLogs。
 * 日志通过 WebSocket 由 useWebSocket 推入（追加 + 去重）；
 * HTTP 历史拉取为整体替换（重建去重基准），避免重复累积。
 */

import { reactive, ref, computed } from "vue";
import type { LogEntry } from "../api/types";
import { systemApi } from "../api";
import { LEVEL_VALUES, LIMITS } from "../utils/constants";
import { frontendLogger } from "../utils/logger";

const logs = reactive<LogEntry[]>([]);
const logFilter = reactive({ level: "INFO", source: "", search: "" });
const autoScroll = ref(true);
const newLogCount = ref(0);
// 日志是否已加载（控制首次 HTTP 拉取后再启用 WS 实时追加，避免历史与实时乱序）
const initialized = ref(false);

// 已见日志去重键集合（基于 timestamp + message + source）
const seenKeys = new Set<string>();

function logKey(e: LogEntry): string {
  return `${e.timestamp}|${e.message}|${e.source}`;
}

const filteredLogs = computed(() => {
  const { level, source, search } = logFilter;
  const q = search ? search.toLowerCase() : "";
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
  for (const e of entries) seenKeys.add(logKey(e));
  logs.splice(0, logs.length, ...entries);
  initialized.value = true;
}

/** 追加日志（WebSocket 实时推入）。atBottom 表示当前是否已滚动到底部。 */
function appendLogs(entries: LogEntry[], atBottom = true): void {
  // HTTP 历史未就绪前丢弃 WS 实时日志，避免与历史拉取乱序（由 fetchLogs 重建基准）
  if (!initialized.value) return;
  let added = 0;
  for (const e of entries) {
    const key = logKey(e);
    if (seenKeys.has(key)) continue; // 去重：HTTP 历史与 WS 实时可能产生重复
    seenKeys.add(key);
    logs.push(e);
    added++;
  }
  if (logs.length > LIMITS.LOG_MAX_ENTRIES) {
    const overflow = logs.length - LIMITS.LOG_MAX_ENTRIES;
    logs.splice(0, overflow);
    // 裁剪后若 seen 集合过大则重建，防止内存无限增长
    if (seenKeys.size > LIMITS.LOG_MAX_ENTRIES * 2) {
      const keep = logs.map((l) => logKey(l));
      seenKeys.clear();
      keep.forEach((k) => seenKeys.add(k));
    }
  }
  // 不在底部时累计“新消息”计数，供前端“N 条新消息”提示
  if (!atBottom) newLogCount.value += added;
}

function clearLogs(): void {
  logs.splice(0, logs.length);
  seenKeys.clear();
  newLogCount.value = 0;
}

export function useLogs() {
  return { logs, logFilter, autoScroll, newLogCount, initialized, filteredLogs, fetchLogs, appendLogs, clearLogs };
}
