/**
 * useRepoImport 的条目类型分流、点选、截图字段与来源切换的单元测试。
 * 覆盖：列表只列当前类型（缺省/空串视作浏览器任务，script 两类都不进）、
 * 搜索不越过类型边界、点选写入 selected（不触发导入）、打开/加载索引时复位 selected、
 * 任务刷新后点选失效自动清空、来源切换回填预设索引地址与仓库主页地址、
 * 确认导入时按 repoKind 写进对应的编辑器草稿（浏览器任务 / 直连任务）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { RepoTask } from "../api/types";
import {
  TASK_REPO_INDEX_URL,
  TASK_REPO_INDEX_URL_GITEE,
  TASK_REPO_URL,
  TASK_REPO_URL_GITEE,
} from "../utils/constants";

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

// 直连任务草稿的写入需要被断言，故用 vi.hoisted 暴露 mock 句柄
// （vi.mock 工厂会被提升，直接引用普通 const 会在初始化前被访问）
const { setHttpTaskDraftMock } = vi.hoisted(() => ({ setHttpTaskDraftMock: vi.fn() }));

vi.mock("./useHttpTasks", () => ({
  useHttpTasks: () => ({
    confirmDiscardHttpTaskIfDirty: async () => true,
    setHttpTaskDraft: setHttpTaskDraftMock,
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

/** 带类型字段的条目（`type` 缺省即老条目，视为浏览器任务） */
function makeTypedTask(id: string, type?: string): RepoTask {
  return { id, name: `任务${id}`, url: `https://example.com/${id}.json`, type };
}

beforeEach(() => {
  vi.mocked(repoApi.fetchIndex).mockReset();
  vi.mocked(repoApi.fetchTask).mockReset();
  setHttpTaskDraftMock.mockReset();
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
    repo.showRepoImport("browser");
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

describe("条目按类型分流", () => {
  it("只列当前类型：缺省/空串/browser 归浏览器任务，http 归直连任务，script 两类都不进", () => {
    const mixed = [
      makeTypedTask("b1"),
      makeTypedTask("b2", ""),
      makeTypedTask("b3", "browser"),
      makeTypedTask("h1", "http"),
      makeTypedTask("s1", "script"),
    ];

    // 注意顺序：showRepoImport 会清空列表（打开弹窗的复位语义），故列表在它之后填
    repo.showRepoImport("browser");
    expect(repo.repoImport.value.repoKind).toBe("browser");
    repo.repoImport.value.tasks = [...mixed];
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["b1", "b2", "b3"]);

    repo.showRepoImport("http");
    expect(repo.repoImport.value.repoKind).toBe("http");
    repo.repoImport.value.tasks = [...mixed];
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["h1"]);

    // script 不属于任务页这两类，任何一边都不该出现（导进去会落到看不见的地方）
    expect(repo.filteredRepoTasks.value.some((t) => t.id === "s1")).toBe(false);
  });

  it("搜索不越过类型边界（否则会把另一类条目导进错的编辑器）", () => {
    repo.showRepoImport("http");
    // 两类条目的名称都含"任务"，纯关键词搜索会把 b1 也带出来
    repo.repoImport.value.tasks = [makeTypedTask("b1"), makeTypedTask("h1", "http")];
    repo.repoImport.value.searchQuery = "任务";
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["h1"]);
  });
});

describe("确认导入的去向由 repoKind 决定", () => {
  it("http：下载到的任务填进直连任务的新建草稿（ID 可改、来源名称优先）", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("dorm", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http",
      name: "宿舍直连登录",
      description: "门户直连",
      method: "GET",
      url: "http://10.0.0.1/login?username={username}&password={password}",
    });

    await repo.acceptRepoDisclaimer();

    expect(setHttpTaskDraftMock).toHaveBeenCalledTimes(1);
    const draft = setHttpTaskDraftMock.mock.calls[0][0];
    expect(draft.id).toBe("dorm");
    expect(draft.name).toBe("宿舍直连登录");
    expect(draft.description).toBe("门户直连");
    expect(draft.url).toContain("{username}");
    // 新建语义：允许用户改 ID 后再保存，避免与他人已导入的同名任务撞车
    expect(draft._isNew).toBe(true);
    expect(repo.repoImport.value.visible).toBe(false);
  });

  it("http：文件实际不是直连任务时拒绝（索引与文件不一致）", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("bad", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", name: "浏览器任务", steps: [] });

    await repo.acceptRepoDisclaimer();

    expect(setHttpTaskDraftMock).not.toHaveBeenCalled();
    // 弹窗不自动关闭，用户能看到列表并换一条重试
    expect(repo.repoImport.value.visible).toBe(true);
  });
});

describe("来源切换", () => {
  it("切到 Gitee 回填镜像索引地址，切回 GitHub 回填原地址", () => {
    repo.selectRepoSource("gitee");
    expect(repo.repoImport.value.url).toBe(TASK_REPO_INDEX_URL_GITEE);
    repo.selectRepoSource("github");
    expect(repo.repoImport.value.url).toBe(TASK_REPO_INDEX_URL);
  });

  it("切到自定义保留已填的 URL，不被清空或覆盖", () => {
    repo.repoImport.value.url = "https://example.com/my-index.json";
    repo.selectRepoSource("custom");
    expect(repo.repoImport.value.url).toBe("https://example.com/my-index.json");
  });

  it("自定义源切走再切回，手输地址仍保留（不因误点一下就丢）", () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "https://example.com/mine.json";
    repo.selectRepoSource("gitee");
    expect(repo.repoImport.value.url).toBe(TASK_REPO_INDEX_URL_GITEE);
    repo.selectRepoSource("custom");
    expect(repo.repoImport.value.url).toBe("https://example.com/mine.json");
  });

  it("「直接查看仓库」在预设源下给仓库主页，而非 raw 索引地址", () => {
    // 真实缺陷：此前直接用索引地址，点开是一屏 raw JSON 而不是仓库页面
    repo.selectRepoSource("github");
    expect(repo.sourceHomeUrl.value).toBe(TASK_REPO_URL);
    repo.selectRepoSource("gitee");
    expect(repo.sourceHomeUrl.value).toBe(TASK_REPO_URL_GITEE);
    expect(repo.sourceHomeUrl.value).not.toContain("raw.");
  });

  it("自定义源下「直接查看仓库」回退为用户自填的地址", () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "https://example.com/idx.json";
    expect(repo.sourceHomeUrl.value).toBe("https://example.com/idx.json");
  });

  it("当前源的说明文案随来源变化，自定义源无说明", () => {
    repo.selectRepoSource("gitee");
    expect(repo.currentSource.value.hint).toContain("国内");
    repo.selectRepoSource("custom");
    expect(repo.currentSource.value.hint).toBe("");
  });

  it("未知源回退到自定义项，不产生 undefined", () => {
    // 防御：类型收窄只在前端生效，localStorage 里的旧值等仍可能给出意外字符串
    repo.repoImport.value.source = "svn" as never;
    expect(repo.currentSource.value?.id).toBe("custom");
    expect(repo.sourceHomeUrl.value).toBe(repo.repoImport.value.url);
  });
});

describe("索引拉取的 epoch 守卫", () => {
  it("迟到的旧响应不得覆盖新请求的列表", async () => {
    // A 慢、B 快：先发 A（挂起），再发 B（立即返回）
    let resolveA: (v: RepoTask[]) => void = () => {};
    const slowA = new Promise<RepoTask[]>((r) => {
      resolveA = r;
    });
    vi.mocked(repoApi.fetchIndex)
      .mockImplementationOnce(() => slowA)
      .mockImplementationOnce(async () => [makeTask("B")]);

    repo.repoImport.value.url = "https://example.com/a.json";
    const pendingA = repo.fetchRepoIndex();
    repo.repoImport.value.url = "https://example.com/b.json";
    await repo.fetchRepoIndex();

    expect(repo.repoImport.value.tasks.map((t) => t.id)).toEqual(["B"]);

    // A 迟到：不得覆盖 B 的列表
    resolveA([makeTask("A")]);
    await pendingA;
    expect(repo.repoImport.value.tasks.map((t) => t.id)).toEqual(["B"]);
  });

  it("旧请求完成时不得提前复位新请求的 loading", async () => {
    let resolveA: (v: RepoTask[]) => void = () => {};
    const slowA = new Promise<RepoTask[]>((r) => {
      resolveA = r;
    });
    let resolveB: (v: RepoTask[]) => void = () => {};
    const slowB = new Promise<RepoTask[]>((r) => {
      resolveB = r;
    });
    vi.mocked(repoApi.fetchIndex)
      .mockImplementationOnce(() => slowA)
      .mockImplementationOnce(() => slowB);

    const pendingA = repo.fetchRepoIndex();
    const pendingB = repo.fetchRepoIndex();
    expect(repo.repoImport.value.loading).toBe(true);

    // A 先回来：B 仍在途，loading 必须保持 true
    resolveA([makeTask("A")]);
    await pendingA;
    expect(repo.repoImport.value.loading).toBe(true);

    // B 回来才复位
    resolveB([makeTask("B")]);
    await pendingB;
    expect(repo.repoImport.value.loading).toBe(false);
    expect(repo.repoImport.value.tasks.map((t) => t.id)).toEqual(["B"]);
  });

  it("被取代的旧请求失败不得覆盖新请求的错误状态", async () => {
    let rejectA: (e: unknown) => void = () => {};
    const failA = new Promise<RepoTask[]>((_r, rej) => {
      rejectA = rej;
    });
    vi.mocked(repoApi.fetchIndex)
      .mockImplementationOnce(() => failA)
      .mockImplementationOnce(async () => [makeTask("B")]);

    const pendingA = repo.fetchRepoIndex();
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.error).toBe("");

    rejectA(new Error("A 超时"));
    await pendingA;
    // 新请求已成功，旧请求的失败不得写入 error
    expect(repo.repoImport.value.error).toBe("");
    expect(repo.repoImport.value.tasks.map((t) => t.id)).toEqual(["B"]);
  });
});
