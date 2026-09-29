/** 索引缓存的来源隔离、两小时失效、坏数据回退与存储不可用处理。 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { readRepoIndexCache, writeRepoIndexCache, REPO_INDEX_CACHE_TTL_MS } from "./repoIndexCache";

const saved = new Map<string, string>();
const github = "https://example.com/index.json";
const gitee = "https://mirror.example.com/index.json";
const task = { id: "portal", name: "校园网", url: "https://example.com/portal.json" };

beforeEach(() => {
  saved.clear();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => { saved.set(key, value); },
  });
});

describe("任务索引本地缓存", () => {
  it("按完整 URL 区分来源，空索引也能缓存", () => {
    writeRepoIndexCache(github, [task], 1000);
    writeRepoIndexCache(gitee, [], 1000);
    expect(readRepoIndexCache(github, 1001)?.tasks).toEqual([task]);
    expect(readRepoIndexCache(gitee, 1001)?.tasks).toEqual([]);
  });

  it("两小时整过期，时钟回拨也不复用未来的记录", () => {
    writeRepoIndexCache(github, [task], 1000);
    expect(readRepoIndexCache(github, 1000 + REPO_INDEX_CACHE_TTL_MS - 1)).not.toBeNull();
    expect(readRepoIndexCache(github, 1000 + REPO_INDEX_CACHE_TTL_MS)).toBeNull();
    expect(readRepoIndexCache(github, 999)).toBeNull();
  });

  it("本地缓存损坏时直接视为未命中", () => {
    saved.set("campus-auth.repo-index-cache.v1", "{坏数据");
    expect(readRepoIndexCache(github)).toBeNull();
    writeRepoIndexCache(github, [task], 1000);
    expect(readRepoIndexCache(github, 1001)?.tasks).toEqual([task]);
  });

  it("浏览器禁用本地存储时不阻断索引加载", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => { throw new Error("disabled"); },
      setItem: () => { throw new Error("disabled"); },
    });
    expect(readRepoIndexCache(github)).toBeNull();
    expect(() => writeRepoIndexCache(github, [task])).not.toThrow();
  });
});
