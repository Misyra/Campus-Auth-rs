/**
 * useRepoImport 点选与截图字段的单元测试。
 * 覆盖：列表点选写入 selected（不触发导入）、打开/加载索引时复位 selected、
 * 任务刷新后点选失效自动清空。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { RepoTask } from "../api/types";

vi.mock("../api", () => ({
  repoApi: {
    fetchIndex: vi.fn(),
    fetchTask: vi.fn(),
  },
}));

vi.mock("./useTasks", () => ({
  useTasks: () => ({
    confirmDiscardTaskIfDirty: async () => true,
    setTaskDraft: vi.fn(),
    jsonError: { value: "" },
  }),
}));

vi.mock("./useToast", () => ({
  useToast: () => ({ toastOnly: vi.fn() }),
}));

const { useRepoImport } = await import("./useRepoImport");
const { repoApi } = await import("../api");

const repo = useRepoImport();

function makeTask(id: string, screenshot?: string): RepoTask {
  return { id, name: `任务${id}`, url: `https://example.com/${id}.json`, screenshot };
}

beforeEach(() => {
  vi.mocked(repoApi.fetchIndex).mockReset();
  repo.repoImport.value.tasks = [];
  repo.repoImport.value.selected = null;
  repo.repoImport.value.disclaimer = null;
  repo.repoImport.value.searchQuery = "";
  repo.repoImport.value.error = "";
});

describe("仓库导入点选预览", () => {
  it("点选任务写入 selected 且不触发免责声明", () => {
    const task = makeTask("gd-telecom-qs", "https://example.com/a.png");
    repo.selectRepoTask(task);
    // ref 写入后为响应式代理（引用不等），按值断言
    expect(repo.repoImport.value.selected).toStrictEqual(task);
    expect(repo.repoImport.value.disclaimer).toBeNull();
  });

  it("打开弹窗时复位上次点选", () => {
    repo.repoImport.value.selected = makeTask("old");
    repo.showRepoImport();
    expect(repo.repoImport.value.selected).toBeNull();
  });

  it("加载索引时复位点选", async () => {
    repo.repoImport.value.selected = makeTask("old");
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("new")]);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.selected).toBeNull();
    expect(repo.repoImport.value.tasks).toHaveLength(1);
  });
});
