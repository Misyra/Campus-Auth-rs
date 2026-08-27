/**
 * 日志流 composable 的单元测试。
 * 覆盖启动缓冲回放、seq/内容键混合去重、缓冲上限裁剪、
 * "新消息"计数与级别/来源/搜索过滤。
 *
 * useLogs 是模块级单例：同一文件内的用例共享状态，
 * 用例按声明顺序执行（先测初始化前的缓冲，再初始化，最后测运行时行为）。
 */
import { describe, it, expect, vi, beforeAll } from "vitest";
import type { LogEntry } from "../api/types";

// systemApi.fetchLogs 不发真实请求；返回空数组即完成"历史替换"初始化
vi.mock("../api", () => ({
  systemApi: { fetchLogs: vi.fn(async () => []) },
}));

const { useLogs } = await import("./useLogs");

function entry(seq: number, message = `msg-${seq}`, level = "INFO", source = "backend"): LogEntry {
  return { seq, timestamp: `2026-08-26T00:00:${String(seq % 60).padStart(2, "0")}`, level, source, message };
}

/** appendLogs 经 queueMicrotask 批量落盘，让出一个微任务即完成 flush */
async function flush(): Promise<void> {
  await Promise.resolve();
}

const logs = useLogs();

beforeAll(async () => {
  logs.clearLogs();
});

describe("初始化前的实时缓冲", () => {
  it("未初始化时 appendLogs 只缓存不落盘", () => {
    logs.appendLogs([entry(1), entry(2)]);
    expect(logs.logs.length).toBe(0);
  });

  it("缓冲超过 WS_LOG_BUFFER_MAX(100) 时裁掉最旧的", () => {
    for (let i = 3; i <= 200; i++) logs.appendLogs([entry(i)]);
    expect(logs.initialized.value).toBe(false);
  });

  it("fetchLogs 完成后初始化，并回放缓冲（seq 重复条目被去重）", async () => {
    // 重复推一条已在缓冲中的日志：回放时按 seenSeqs 去重
    logs.appendLogs([entry(200)]);
    await logs.fetchLogs();
    expect(logs.initialized.value).toBe(true);
    // 缓冲上限 100：追加重复项触发裁剪后保留 102~200，回放去重后共 99 条
    expect(logs.logs.length).toBe(99);
    expect(logs.logs[0].seq).toBe(102);
    expect(logs.logs[98].seq).toBe(200);
  });
});

describe("实时追加与去重", () => {
  it("相同 seq 只落盘一次", async () => {
    const before = logs.logs.length;
    logs.appendLogs([entry(300), entry(300)]);
    await flush();
    expect(logs.logs.length).toBe(before + 1);
  });

  it("缺 seq 的条目按内容键去重", async () => {
    logs.clearLogs();
    const noSeq = { timestamp: "2026-08-26T00:00:01", level: "INFO", source: "backend", message: "legacy" };
    logs.appendLogs([noSeq as LogEntry, noSeq as LogEntry]);
    await flush();
    expect(logs.logs.length).toBe(1);
  });

  it("不在底部时累计新消息计数", async () => {
    logs.appendLogs([entry(401), entry(402), entry(403)], false);
    await flush();
    expect(logs.newLogCount.value).toBe(3);
    logs.newLogCount.value = 0;
  });

  it("超过 LOG_MAX_ENTRIES(100) 后裁剪最旧日志", async () => {
    logs.clearLogs();
    for (let i = 1; i <= 150; i++) logs.appendLogs([entry(i)]);
    await flush();
    expect(logs.logs.length).toBe(100);
    expect(logs.logs[0].seq).toBe(51);
  });
});

describe("过滤", () => {
  beforeAll(async () => {
    logs.clearLogs();
    logs.appendLogs([
      entry(1, "认证成功", "INFO", "backend"),
      entry(2, "connection timeout", "ERROR", "backend"),
      entry(3, "认证重试", "WARN", "frontend"),
    ]);
    await flush();
  });

  it("级别过滤取下限以上（INFO 含 INFO/WARN/ERROR）", () => {
    logs.logFilter.level = "INFO";
    expect(logs.filteredLogs.value.map((l) => l.seq)).toEqual([1, 2, 3]);

    logs.logFilter.level = "WARN";
    expect(logs.filteredLogs.value.map((l) => l.seq)).toEqual([2, 3]);

    logs.logFilter.level = "";
  });

  it("来源过滤精确匹配", () => {
    logs.logFilter.source = "frontend";
    expect(logs.filteredLogs.value.map((l) => l.seq)).toEqual([3]);
    logs.logFilter.source = "";
  });

  it("搜索不区分大小写（防抖 300ms 后生效）", async () => {
    logs.logFilter.search = "TIMEOUT";
    await new Promise((r) => setTimeout(r, 350));
    expect(logs.filteredLogs.value.map((l) => l.seq)).toEqual([2]);
    logs.logFilter.search = "";
    await new Promise((r) => setTimeout(r, 350));
  });
});

describe("clearLogs", () => {
  it("清空日志与计数", async () => {
    logs.appendLogs([entry(999)], false);
    await flush();
    logs.clearLogs();
    expect(logs.logs.length).toBe(0);
    expect(logs.newLogCount.value).toBe(0);
  });
});
