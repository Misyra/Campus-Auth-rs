/**
 * useRepoImport 的条目类型分流、点选、截图字段、索引地址（类别 × 源）与索引状态的单元测试。
 * 覆盖：统一索引按三类筛选（旧条目缺省/空串视作浏览器任务）、搜索不越过类别边界、
 * 切类别保持同源索引、切源换镜像、空索引与格式错的区分、未知类型计数、
 * 点选写入 selected（不触发导入）、打开/加载索引时复位 selected、任务刷新后点选失效自动清空、
 * 「直接查看仓库」用仓库主页地址、确认导入时按 repoKind 写进对应的编辑器草稿。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { RepoTask } from "../api/types";
import { TASK_REPO_URL, TASK_REPO_URL_GITEE, presetRepoIndexUrl } from "../utils/constants";
import { REPO_INDEX_CACHE_TTL_MS } from "../utils/repoIndexCache";

// 自动保存模式下导入直接落盘：tasksApi.save 与编辑器跳转都要被断言。
// useTasks/useHttpTasks 不再有草稿写入接口（setTaskDraft 等），mock 收窄到
// 新契约：列表 + 拉取 + 打开编辑器。
const { tasksApiMock, showHttpTaskEditorMock, showTaskEditorMock, toastOnlyMock } = vi.hoisted(() => ({
  tasksApiMock: {
    list: vi.fn(async () => [] as { id: string }[]),
    save: vi.fn(async () => ({ message: "保存成功" })),
  },
  showHttpTaskEditorMock: vi.fn(async () => {}),
  showTaskEditorMock: vi.fn(async () => {}),
  toastOnlyMock: vi.fn(),
}));

vi.mock("../api", () => ({
  repoApi: {
    fetchIndex: vi.fn(),
    fetchTask: vi.fn(),
  },
  tasksApi: tasksApiMock,
}));

vi.mock("./useTasks", () => ({
  useTasks: () => ({
    tasks: { value: [] },
    fetchTasks: vi.fn(async () => {}),
    showTaskEditor: showTaskEditorMock,
  }),
}));

vi.mock("./useHttpTasks", () => ({
  useHttpTasks: () => ({
    httpTasks: { value: [] },
    fetchHttpTasks: vi.fn(async () => {}),
    showHttpTaskEditor: showHttpTaskEditorMock,
  }),
}));

const { showScriptEditorMock } = vi.hoisted(() => ({
  showScriptEditorMock: vi.fn(async () => {}),
}));
vi.mock("./useScripts", () => ({
  useScripts: () => ({
    fetchScripts: vi.fn(async () => {}),
    showScriptEditor: showScriptEditorMock,
  }),
}));

vi.mock("./useToast", () => ({
  useToast: () => ({ toastOnly: toastOnlyMock }),
}));

// 导入跳编辑页经动态 import("../router")：router.push 打桩
const { routerPushMock } = vi.hoisted(() => ({ routerPushMock: vi.fn(async () => {}) }));

vi.mock("../router", () => ({
  router: { push: routerPushMock },
}));

const { useRepoImport, filterRepoTasks } = await import("./useRepoImport");
const { repoApi } = await import("../api");

const repo = useRepoImport();
const cacheStorage = new Map<string, string>();

function makeTask(id: string, screenshot?: string): RepoTask {
  return { id, name: `任务${id}`, url: `https://example.com/${id}.json`, screenshot };
}

/** 带类型字段的条目（`type` 缺省即老条目，视为浏览器任务） */
function makeTypedTask(id: string, type?: string): RepoTask {
  return { id, name: `任务${id}`, url: `https://example.com/${id}.json`, type };
}

beforeEach(() => {
  cacheStorage.clear();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => cacheStorage.get(key) ?? null,
    setItem: (key: string, value: string) => { cacheStorage.set(key, value); },
    removeItem: (key: string) => { cacheStorage.delete(key); },
  });
  vi.mocked(repoApi.fetchIndex).mockReset();
  vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
  vi.mocked(repoApi.fetchTask).mockReset();
  tasksApiMock.save.mockClear();
  tasksApiMock.list.mockReset();
  tasksApiMock.list.mockResolvedValue([]);
  showScriptEditorMock.mockClear();
  showHttpTaskEditorMock.mockClear();
  showTaskEditorMock.mockClear();
  toastOnlyMock.mockClear();
  routerPushMock.mockClear();
  // 索引地址与列表状态显式复位：这些用例会改 source / repoKind，不复位就会互相串味
  repo.repoImport.value.source = "github";
  repo.repoImport.value.repoKind = "browser";
  repo.repoImport.value.url = presetRepoIndexUrl("browser", "github");
  repo.repoImport.value.customUrl = "";
  repo.repoImport.value.loaded = false;
  repo.repoImport.value.tasks = [];
  repo.repoImport.value.selected = null;
  repo.repoImport.value.disclaimer = null;
  repo.repoImport.value.searchQuery = "";
  repo.repoImport.value.error = "";
  repo.repoImport.value.visible = true;
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
  it("只列当前类型：缺省/空串/browser 归浏览器，http 和 script 各归自己的子页", () => {
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

    expect(repo.filteredRepoTasks.value.some((t) => t.id === "s1")).toBe(false);
    repo.showRepoImport("script");
    repo.repoImport.value.tasks = [...mixed];
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["s1"]);
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
  it("script：先展示下载到的正文，再保存同一份内容并打开脚本编辑器", async () => {
    repo.showRepoImport("script");
    const entry = makeTypedTask("campus_script", "script");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "script", name: "校园网脚本", content: "print('ready')",
    });
    await repo.confirmRepoImport(entry);
    expect(repo.repoImport.value.disclaimer?.id).toBe("campus_script");
    expect(repo.repoImport.value.scriptPreview).toBe("print('ready')");
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    await repo.acceptRepoDisclaimer();
    expect(repoApi.fetchTask).toHaveBeenCalledTimes(1);
    expect(tasksApiMock.save).toHaveBeenCalledWith("campus_script", expect.objectContaining({
      type: "script", content: "print('ready')", binary_path: "",
    }));
    expect(showScriptEditorMock).toHaveBeenCalledWith("campus_script");
    expect(routerPushMock).toHaveBeenCalledWith({ name: "tasks-scripts", query: { task: "campus_script" } });
  });

  it("script：拒绝需要本地路径的远程配置，取消预览后不保存", async () => {
    repo.showRepoImport("script");
    const entry = makeTypedTask("unsafe", "script");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "script", content: "print('ready')", binary_path: "C:\\Other\\runner.exe",
    });
    await repo.confirmRepoImport(entry);
    expect(repo.repoImport.value.disclaimer).toBeNull();
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "script", content: "print('ready')",
    });
    await repo.confirmRepoImport(entry);
    repo.cancelRepoDisclaimer();
    expect(repo.repoImport.value.scriptPreview).toBe("");
    expect(tasksApiMock.save).not.toHaveBeenCalled();
  });

  it("script：关闭弹窗后迟到的下载结果不得重新弹出代码确认", async () => {
    repo.showRepoImport("script");
    let resolveTask: (task: Record<string, unknown>) => void = () => {};
    vi.mocked(repoApi.fetchTask).mockImplementationOnce(() => new Promise((resolve) => { resolveTask = resolve; }));
    const pending = repo.confirmRepoImport(makeTypedTask("late", "script"));
    expect(repo.repoImport.value.previewLoading).toBe(true);
    repo.closeRepoImport();
    resolveTask({ type: "script", content: "print('late')" });
    await pending;
    expect(repo.repoImport.value.disclaimer).toBeNull();
    expect(repo.repoImport.value.scriptPreview).toBe("");
    expect(repo.repoImport.value.previewLoading).toBe(false);
  });

  it("跨类型同 ID 时自动改名，不覆盖已存在的脚本", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("portal", "http");
    tasksApiMock.list.mockResolvedValue([{ id: "portal" }]);
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http", name: "Portal", method: "GET", url: "http://10.0.0.1/login",
    });
    await repo.acceptRepoDisclaimer();
    expect(tasksApiMock.save).toHaveBeenCalledWith("portal_2", expect.anything());
  });

  it("http：下载到的任务直接落盘并打开其编辑器（自动保存模式无草稿态）", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("dorm", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http",
      name: "宿舍HTTP 登录",
      description: "门户直连",
      method: "GET",
      url: "http://10.0.0.1/login?username={username}&password={password}",
    });

    await repo.acceptRepoDisclaimer();

    // 直接落盘：save 一次，载荷带 type=http 与来源名称
    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    const [id, payload] = tasksApiMock.save.mock.calls[0] as unknown as [string, Record<string, unknown>];
    expect(id).toBe("dorm");
    expect(payload).toMatchObject({
      type: "http",
      task_id: "dorm",
      name: "宿舍HTTP 登录",
      url: "http://10.0.0.1/login?username={username}&password={password}",
    });
    // 落盘后打开该任务的编辑器
    expect(showHttpTaskEditorMock).toHaveBeenCalledWith("dorm");
    expect(routerPushMock).toHaveBeenCalledWith({ name: "tasks-http", query: { task: "dorm" } });
    expect(repo.repoImport.value.visible).toBe(false);
  });

  it("http：文件实际不是HTTP 登录任务时拒绝（索引与文件不一致）", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("bad", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", name: "浏览器任务", steps: [] });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).not.toHaveBeenCalled();
    // 弹窗不自动关闭，用户能看到列表并换一条重试
    expect(repo.repoImport.value.visible).toBe(true);
  });

  it("http：导出包装层与实际配置的类型冲突时拒绝导入", async () => {
    repo.showRepoImport("http");
    repo.repoImport.value.disclaimer = makeTypedTask("conflict", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http",
      config: { type: "script", content: "print('unexpected')" },
    });
    await repo.acceptRepoDisclaimer();
    expect(tasksApiMock.save).not.toHaveBeenCalled();
  });
});

describe("索引地址：类别 × 源", () => {
  it("切到 Gitee 回填该类别下的镜像索引，切回 GitHub 回填主索引", () => {
    repo.selectRepoSource("gitee");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("browser", "gitee"));
    repo.selectRepoSource("github");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("browser", "github"));
  });

  it("切类别和镜像都取对应索引地址", () => {
    repo.showRepoImport("http");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("http", "github"));
    expect(repo.repoImport.value.url).not.toBe(presetRepoIndexUrl("browser", "github"));
    // 类别内部再切源：仍留在 http 那一份
    repo.selectRepoSource("gitee");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("http", "gitee"));
    expect(repo.repoImport.value.url).not.toBe(presetRepoIndexUrl("browser", "gitee"));
    // 切回浏览器类别：跟着回到浏览器索引，且保留当前源（Gitee）
    repo.showRepoImport("browser");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("browser", "gitee"));
  });

  it("自定义源的手输地址跨类别保留（它按约定指向某一类索引，与当前列表无关）", () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "https://example.com/my-index.json";
    repo.showRepoImport("http");
    expect(repo.repoImport.value.url).toBe("https://example.com/my-index.json");
    repo.showRepoImport("browser");
    expect(repo.repoImport.value.url).toBe("https://example.com/my-index.json");
  });

  it("首次切到自定义源清空预设地址，避免误把 GitHub 索引当自定义源加载", () => {
    repo.selectRepoSource("custom");
    expect(repo.repoImport.value.url).toBe("");
    expect(repo.repoImport.value.loaded).toBe(false);
  });

  it("自定义源切走再切回，手输地址仍保留（不因误点一下就丢）", () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "https://example.com/mine.json";
    repo.selectRepoSource("gitee");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("browser", "gitee"));
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
  it("关闭后重新打开另一类别时，旧弹窗的响应不能填入新列表", async () => {
    let resolveOld: (v: RepoTask[]) => void = () => {};
    vi.mocked(repoApi.fetchIndex)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce([makeTypedTask("new", "http")]);
    repo.showRepoImport("browser");
    repo.closeRepoImport();
    repo.showRepoImport("http");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    resolveOld([makeTask("old")]);
    await Promise.resolve();
    expect(repo.repoImport.value.tasks.map((task) => task.id)).toEqual(["new"]);
    expect(repo.repoImport.value.loaded).toBe(true);
  });

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

describe("索引状态与异类条目", () => {
  it("空索引是合法状态：loaded 为真且不报错", async () => {
    // 拆分后「这一类暂时没有条目」是正常状态（新仓库、镜像源尚未收录），
    // 不能与「拉取失败 / 格式错」共用一句提示
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(true);
    expect(repo.repoImport.value.error).toBe("");
    expect(repo.repoImport.value.tasks).toEqual([]);
    expect(toastOnlyMock).not.toHaveBeenCalled();
  });

  it("非数组按格式错处理，且不计入加载成功", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue({ tasks: [] } as never);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.error).toContain("JSON 数组");
    expect(repo.repoImport.value.loaded).toBe(false);
  });

  it("拉取失败提示带上当前筛选类别名", async () => {
    vi.mocked(repoApi.fetchIndex).mockRejectedValue(new Error("连接超时"));
    repo.showRepoImport("http");
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(false);
    const [ok, message] = toastOnlyMock.mock.calls[0] as [boolean, string];
    expect(ok).toBe(false);
    expect(message).toContain("HTTP 登录任务索引");
  });

  it("打开弹窗复位 loaded：上一次的「该源暂无条目」不得残留到新索引", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(true);
    repo.showRepoImport("http");
    expect(repo.repoImport.value.loaded).toBe(false);
  });

  it("其他已知类别只被过滤，未知类型才计数", () => {
    repo.showRepoImport("http");
    repo.repoImport.value.tasks = [
      makeTypedTask("b1"),
      makeTypedTask("h1", "http"),
      makeTypedTask("s1", "script"),
      makeTypedTask("x1", "unknown"),
    ];
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["h1"]);
    expect(repo.foreignRepoTaskCount.value).toBe(1);
  });

  it("本类索引里全为本类条目时计数为 0（避免提示常驻）", () => {
    repo.showRepoImport("browser");
    repo.repoImport.value.tasks = [makeTypedTask("b1"), makeTypedTask("b2", "browser")];
    expect(repo.foreignRepoTaskCount.value).toBe(0);
  });
});

describe("打开自动加载与两小时缓存", () => {
  it("首次打开自动获取索引，关闭重开立即使用同 URL 缓存", async () => {
    const entry = makeTypedTask("h1", "http");
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([entry]);
    repo.showRepoImport("http");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    expect(repoApi.fetchIndex).toHaveBeenCalledWith(presetRepoIndexUrl("http", "github"));
    expect(repo.repoImport.value.tasks).toEqual([entry]);
    expect(repo.repoImport.value.fromCache).toBe(false);

    repo.closeRepoImport();
    vi.mocked(repoApi.fetchIndex).mockClear();
    repo.showRepoImport("http");
    expect(repo.repoImport.value.loading).toBe(false);
    expect(repo.repoImport.value.fromCache).toBe(true);
    expect(repo.repoImport.value.tasks).toEqual([entry]);
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
  });

  it("两小时到期后重开会重新获取索引", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1000);
    try {
      vi.mocked(repoApi.fetchIndex)
        .mockResolvedValueOnce([makeTask("before")])
        .mockResolvedValueOnce([makeTask("after")]);
      repo.showRepoImport("browser");
      await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
      repo.closeRepoImport();

      now.mockReturnValue(1000 + REPO_INDEX_CACHE_TTL_MS);
      repo.showRepoImport("browser");
      expect(repo.repoImport.value.loading).toBe(true);
      await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
      expect(repo.repoImport.value.tasks[0]?.id).toBe("after");
      expect(repoApi.fetchIndex).toHaveBeenCalledTimes(2);
    } finally {
      now.mockRestore();
    }
  });

  it("手动刷新绕过有效缓存，等待期间保留旧列表，成功后替换缓存", async () => {
    const oldEntry = makeTask("old");
    const newEntry = makeTask("new");
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([oldEntry]);
    repo.showRepoImport("browser");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    repo.selectRepoTask(oldEntry);

    let resolveRefresh: (entries: RepoTask[]) => void = () => {};
    vi.mocked(repoApi.fetchIndex).mockImplementationOnce(() => new Promise((resolve) => { resolveRefresh = resolve; }));
    const refreshing = repo.fetchRepoIndex();
    expect(repo.repoImport.value.loading).toBe(true);
    expect(repo.repoImport.value.tasks).toEqual([oldEntry]);
    expect(repo.repoImport.value.selected?.id).toBe("old");
    resolveRefresh([newEntry]);
    await refreshing;
    expect(repo.repoImport.value.tasks).toEqual([newEntry]);
    expect(repo.repoImport.value.selected).toBeNull();

    repo.closeRepoImport();
    vi.mocked(repoApi.fetchIndex).mockClear();
    repo.showRepoImport("browser");
    expect(repo.repoImport.value.tasks).toEqual([newEntry]);
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
  });

  it("切换镜像自动读取对应索引，切回已缓存来源不再请求", async () => {
    vi.mocked(repoApi.fetchIndex)
      .mockResolvedValueOnce([makeTask("github")])
      .mockResolvedValueOnce([makeTask("gitee")]);
    repo.showRepoImport("browser");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    repo.selectRepoSource("gitee");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    expect(repo.repoImport.value.tasks[0]?.id).toBe("gitee");
    repo.selectRepoSource("github");
    expect(repo.repoImport.value.tasks[0]?.id).toBe("github");
    expect(repo.repoImport.value.fromCache).toBe(true);
    expect(repoApi.fetchIndex).toHaveBeenCalledTimes(2);
  });

  it("刷新失败保留有效列表，首次切到空自定义源不误请求旧地址", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("current")]);
    repo.showRepoImport("browser");
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    vi.mocked(repoApi.fetchIndex).mockRejectedValueOnce(new Error("网络不可用"));
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(true);
    expect(repo.repoImport.value.tasks[0]?.id).toBe("current");
    expect(repo.repoImport.value.error).toContain("网络不可用");

    vi.mocked(repoApi.fetchIndex).mockClear();
    repo.selectRepoSource("custom");
    expect(repo.repoImport.value.url).toBe("");
    expect(repo.repoImport.value.tasks).toEqual([]);
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
  });
});

describe("向导预置打开（showRepoImport opts）", () => {
  it("预置源与关键词后自动加载，并保留关键词", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("a"), makeTask("b")]);
    repo.showRepoImport("http", {
      source: "gitee",
      keyword: "电子科大",
    });
    // 源与索引地址按 (类别, 源) 回填，关键词已预填
    expect(repo.repoImport.value.source).toBe("gitee");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("http", "gitee"));
    expect(repo.repoImport.value.searchQuery).toBe("电子科大");
    // 所有入口打开后都会在后台读取索引
    expect(repo.repoImport.value.loading).toBe(true);
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    // 拉取完成后仍保留向导预填的关键词
    expect(repo.repoImport.value.loaded).toBe(true);
    expect(repo.repoImport.value.searchQuery).toBe("电子科大");
  });

  it("不带 opts 的普通打开复位关键词并自动读取本类索引", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
    repo.showRepoImport("http", { source: "gitee", keyword: "某学校" });
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));

    repo.showRepoImport("browser");
    expect(repo.repoImport.value.searchQuery).toBe("");
    expect(repo.repoImport.value.loading).toBe(true);
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    expect(repoApi.fetchIndex).toHaveBeenLastCalledWith(presetRepoIndexUrl("browser", "gitee"));
    expect(repo.repoImport.value.tasks).toEqual([]);
  });

  it("afterImport 提供时接管收尾：不打开编辑器、不跳路由，回调收到最终 id", async () => {
    const afterImport = vi.fn(async () => {});
    repo.showRepoImport("http", { afterImport });
    repo.repoImport.value.disclaimer = makeTypedTask("dorm", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http",
      name: "宿舍HTTP 登录",
      method: "GET",
      url: "http://10.0.0.1/login",
    });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(afterImport).toHaveBeenCalledWith("dorm");
    expect(showHttpTaskEditorMock).not.toHaveBeenCalled();
    expect(routerPushMock).not.toHaveBeenCalled();
    expect(repo.repoImport.value.visible).toBe(false);
  });

  it("afterImport 抛错时保留向导回调和弹窗，供用户重试后续配置", async () => {
    const afterImport = vi.fn(async () => {
      throw new Error("绑定失败");
    });
    repo.showRepoImport("browser", { afterImport });
    repo.repoImport.value.disclaimer = makeTask("campus");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", name: "校园登录", steps: [] });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(repo.repoImport.value.visible).toBe(true);
    const [ok, message] = toastOnlyMock.mock.calls.at(-1) as [boolean, string];
    expect(ok).toBe(false);
    expect(message).toContain("已导入");
    expect(message).toContain("绑定失败");
  });

  it("首次下载失败后重试成功仍回调向导", async () => {
    const afterImport = vi.fn(async () => {});
    repo.showRepoImport("browser", { afterImport });
    const task = makeTask("campus");
    vi.mocked(repoApi.fetchTask)
      .mockRejectedValueOnce(new Error("网络中断"))
      .mockResolvedValueOnce({ type: "browser", name: "校园登录", steps: [] });
    repo.repoImport.value.disclaimer = task;
    await repo.acceptRepoDisclaimer();
    expect(afterImport).not.toHaveBeenCalled();
    repo.repoImport.value.disclaimer = task;
    await repo.acceptRepoDisclaimer();
    expect(afterImport).toHaveBeenCalledWith("campus");
    expect(routerPushMock).not.toHaveBeenCalled();
  });
});

describe("filterRepoTasks（向导与弹窗共用的匹配口径）", () => {
  const mixed = [
    { ...makeTask("b1"), description: "电子科技大学校园网登录" },
    { ...makeTypedTask("h1", "http"), tags: ["电子科大"] },
    { ...makeTypedTask("s1", "script"), name: "电子科大脚本" },
  ];

  it("先按类别收窄再做关键词匹配，大小写不敏感", () => {
    // 关键词命中的 script 条目不得越界出现；browser 条目按 description 命中。
    // 注意匹配是**连续子串** includes：缩写「电子科大」不是全名的子串（跳字），
    // 故这里用连续片段「科技大学」验证，向导空态文案也据此引导"换个说法再搜"。
    expect(filterRepoTasks(mixed, "browser", "科技大学").map((t) => t.id)).toEqual(["b1"]);
    expect(filterRepoTasks(mixed, "http", "电子科大").map((t) => t.id)).toEqual(["h1"]);
    expect(filterRepoTasks([{ ...makeTask("x"), author: "Misyra" }], "browser", "MISYRA").map((t) => t.id)).toEqual(["x"]);
  });

  it("空查询返回全部同类条目", () => {
    expect(filterRepoTasks(mixed, "browser", "  ").map((t) => t.id)).toEqual(["b1"]);
  });
});
