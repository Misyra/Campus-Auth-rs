/**
 * useRepoImport 的统一列表、按条目类型导入、全部索引加载与错误恢复测试。
 * 覆盖：三类任务一起展示、旧条目兼容、跨类别同 ID 隔离、搜索与自动路由、
 * 镜像和缓存隔离、部分索引失败回退、迟到响应守卫、格式错误与未知类型、
 * 点选写入 selected（不触发导入）、打开/加载索引时复位 selected、任务刷新后点选失效自动清空、
 * 仓库统一展示三类任务，导入后按条目类型打开对应编辑器。
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

describe("统一任务库列表", () => {
  it("所有入口展示浏览器、HTTP 与脚本，旧条目兼容且未知类型跳过", () => {
    repo.showRepoImport();
    repo.repoImport.value.tasks = [makeTask("old"), makeTypedTask("browser", "browser"), makeTypedTask("http", "http"), makeTypedTask("script", "script"), makeTypedTask("unknown", "other")];
    expect(repo.filteredRepoTasks.value.map((task) => task.id)).toEqual(["old", "browser", "http", "script"]);
    expect(repo.repoTaskCount.value).toBe(4);
    expect(repo.foreignRepoTaskCount.value).toBe(1);
  });
  it("搜索覆盖所有类别，跨类别同 ID 选中不会串到另一条", () => {
    repo.showRepoImport();
    const browser = { ...makeTypedTask("same", "browser"), name: "学校浏览器" };
    const http = { ...makeTypedTask("same", "http"), name: "学校直连" };
    repo.repoImport.value.tasks = [browser, http];
    repo.repoImport.value.searchQuery = "学校";
    expect(repo.filteredRepoTasks.value).toHaveLength(2);
    repo.selectRepoTask(browser);
    repo.repoImport.value.searchQuery = "直连";
    expect(repo.repoImport.value.selected).toBeNull();
  });
});

describe("确认导入的去向由条目类型决定", () => {
  it("script：先展示下载到的正文，再保存同一份内容并打开脚本编辑器", async () => {
    repo.showRepoImport();
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
    repo.showRepoImport();
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
    repo.showRepoImport();
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
    repo.showRepoImport();
    repo.repoImport.value.disclaimer = makeTypedTask("portal", "http");
    tasksApiMock.list.mockResolvedValue([{ id: "portal" }]);
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http", name: "Portal", method: "GET", url: "http://10.0.0.1/login",
    });
    await repo.acceptRepoDisclaimer();
    expect(tasksApiMock.save).toHaveBeenCalledWith("portal_2", expect.anything());
  });

  it("http：下载到的任务直接落盘并打开其编辑器（自动保存模式无草稿态）", async () => {
    repo.showRepoImport();
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
    repo.showRepoImport();
    repo.repoImport.value.disclaimer = makeTypedTask("bad", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", name: "浏览器任务", steps: [] });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).not.toHaveBeenCalled();
    // 弹窗不自动关闭，用户能看到列表并换一条重试
    expect(repo.repoImport.value.visible).toBe(true);
  });

  it("http：导出包装层与实际配置的类型冲突时拒绝导入", async () => {
    repo.showRepoImport();
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

  it("自定义源的手输地址关闭重开后保留（它按约定指向某一类索引，与当前列表无关）", () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "https://example.com/my-index.json";
    repo.showRepoImport();
    expect(repo.repoImport.value.url).toBe("https://example.com/my-index.json");
    repo.showRepoImport();
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
  beforeEach(() => {
    repo.repoImport.value.source = "custom";
    repo.repoImport.value.url = "https://example.com/index.json";
  });
  it("关闭后重新打开时，旧弹窗的响应不能填入新列表", async () => {
    let resolveOld: (v: RepoTask[]) => void = () => {};
    vi.mocked(repoApi.fetchIndex)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce([makeTypedTask("new", "http")]);
    repo.showRepoImport();
    repo.closeRepoImport();
    repo.showRepoImport();
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

  it("拉取失败时在列表显示任务库错误与对应索引", async () => {
    vi.mocked(repoApi.fetchIndex).mockRejectedValue(new Error("连接超时"));
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(false);
    expect(repo.repoImport.value.error).toContain("任务库加载失败");
    expect(repo.repoImport.value.error).toContain("HTTP 任务列表");
    expect(repo.repoImport.value.error).toContain("连接超时");
  });

  it("打开弹窗复位 loaded：上一次的「该源暂无条目」不得残留到新索引", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.loaded).toBe(true);
    repo.showRepoImport();
    expect(repo.repoImport.value.loaded).toBe(false);
  });

  it("已知类别全部展示，未知类型才计数", () => {
    repo.showRepoImport();
    repo.repoImport.value.tasks = [
      makeTypedTask("b1"),
      makeTypedTask("h1", "http"),
      makeTypedTask("s1", "script"),
      makeTypedTask("x1", "unknown"),
    ];
    expect(repo.filteredRepoTasks.value.map((t) => t.id)).toEqual(["b1", "h1", "s1"]);
    expect(repo.foreignRepoTaskCount.value).toBe(1);
  });

  it("本类索引里全为本类条目时计数为 0（避免提示常驻）", () => {
    repo.showRepoImport();
    repo.repoImport.value.tasks = [makeTypedTask("b1"), makeTypedTask("b2", "browser")];
    expect(repo.foreignRepoTaskCount.value).toBe(0);
  });
});

describe("全部索引加载与两小时缓存", () => {
  const browserUrl = presetRepoIndexUrl("browser", "github");
  const httpUrl = presetRepoIndexUrl("http", "github");
  const browser = makeTask("browser");
  const http = makeTypedTask("http", "http");
  const script = makeTypedTask("script", "script");
  const settled = () => vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));

  it("打开一次并行读取全部索引，共用的脚本地址不重复请求", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => url === browserUrl ? [browser, script] : [http]);
    repo.showRepoImport();
    await settled();
    expect(repoApi.fetchIndex).toHaveBeenCalledTimes(2);
    expect(repoApi.fetchIndex).toHaveBeenCalledWith(browserUrl);
    expect(repoApi.fetchIndex).toHaveBeenCalledWith(httpUrl);
    expect(repo.filteredRepoTasks.value).toEqual([browser, script, http]);
  });

  it("两小时内重开复用每份索引缓存", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => url === browserUrl ? [browser] : [http]);
    repo.showRepoImport();
    await settled();
    repo.closeRepoImport();
    vi.mocked(repoApi.fetchIndex).mockClear();
    repo.showRepoImport();
    await settled();
    expect(repo.repoImport.value.fromCache).toBe(true);
    expect(repo.repoImport.value.tasks).toEqual([browser, http]);
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
  });

  it("两小时到期后两份索引重新请求", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1000);
    try {
      vi.mocked(repoApi.fetchIndex).mockResolvedValue([browser]);
      repo.showRepoImport();
      await settled();
      repo.closeRepoImport();
      now.mockReturnValue(1000 + REPO_INDEX_CACHE_TTL_MS);
      vi.mocked(repoApi.fetchIndex).mockResolvedValue([http]);
      repo.showRepoImport();
      await settled();
      expect(repo.repoImport.value.tasks).toEqual([http]);
      expect(repoApi.fetchIndex).toHaveBeenCalledTimes(4);
    } finally { now.mockRestore(); }
  });

  it("手动刷新绕过缓存，等待期间保留列表，刷新后按类型和 ID 恢复选中", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => url === browserUrl ? [browser] : [http]);
    repo.showRepoImport();
    await settled();
    repo.selectRepoTask(http);
    let resolveBrowser!: (tasks: RepoTask[]) => void;
    vi.mocked(repoApi.fetchIndex).mockImplementation((url) => url === browserUrl
      ? new Promise((resolve) => { resolveBrowser = resolve; }) : Promise.resolve([http]));
    const pending = repo.fetchRepoIndex();
    expect(repo.repoImport.value.tasks).toEqual([browser, http]);
    expect(repo.repoImport.value.loading).toBe(true);
    resolveBrowser([script]);
    await pending;
    expect(repo.repoImport.value.tasks).toEqual([script, http]);
    expect(repo.repoImport.value.selected).toEqual(http);
  });

  it("切源读取该来源全部索引，切回使用缓存", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => [makeTask(url.includes('gitee') ? 'gitee' : 'github')]);
    repo.showRepoImport();
    await settled();
    repo.selectRepoSource("gitee");
    await settled();
    expect(repo.repoImport.value.tasks[0]?.id).toBe("gitee");
    repo.selectRepoSource("github");
    await settled();
    expect(repo.repoImport.value.tasks[0]?.id).toBe("github");
    expect(repo.repoImport.value.fromCache).toBe(true);
    expect(repoApi.fetchIndex).toHaveBeenCalledTimes(4);
  });

  it("部分索引失败时显示成功列表，并保留失败索引的上次任务", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => url === browserUrl ? [browser] : [http]);
    repo.showRepoImport();
    await settled();
    vi.mocked(repoApi.fetchIndex).mockImplementation((url) => url === browserUrl ? Promise.resolve([script]) : Promise.reject(new Error('HTTP 索引离线')));
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.tasks).toEqual([script, http]);
    expect(repo.repoImport.value.error).toContain("部分任务加载失败");
    expect(repo.repoImport.value.error).toContain("HTTP 索引离线");
  });

  it("首次部分失败仍能导入可用类别，切到空自定义来源不误请求旧地址", async () => {
    vi.mocked(repoApi.fetchIndex).mockImplementation((url) => url === browserUrl ? Promise.resolve([browser]) : Promise.reject(new Error('离线')));
    repo.showRepoImport();
    await settled();
    expect(repo.repoImport.value.tasks).toEqual([browser]);
    expect(repo.repoImport.value.loaded).toBe(true);
    vi.mocked(repoApi.fetchIndex).mockClear();
    repo.selectRepoSource("custom");
    expect(repo.repoImport.value.url).toBe("");
    expect(repo.repoImport.value.tasks).toEqual([]);
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
  });

  it("合并时同类去重，跨类别同 ID 均保留", async () => {
    const b = makeTypedTask("same", "browser");
    const h = makeTypedTask("same", "http");
    vi.mocked(repoApi.fetchIndex).mockImplementation(async (url) => url === browserUrl ? [b] : [h]);
    repo.showRepoImport();
    await settled();
    expect(repo.filteredRepoTasks.value).toEqual([b, h]);
    repo.selectRepoTask(h);
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.selected?.type).toBe("http");
  });
});

describe("向导预置打开（showRepoImport opts）", () => {
  it("脚本条目经向导导入时回传实际类型，仍先展示正文", async () => {
    const afterImport = vi.fn(async () => {});
    repo.showRepoImport({ afterImport });
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "script", content: "print('reviewed')" });
    await repo.confirmRepoImport(makeTypedTask("script", "script"));
    expect(repo.repoImport.value.scriptPreview).toBe("print('reviewed')");
    await repo.acceptRepoDisclaimer();
    expect(afterImport).toHaveBeenCalledWith("script", "script");
    expect(routerPushMock).not.toHaveBeenCalled();
  });
  it("预置源与关键词后自动加载，并保留关键词", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("a"), makeTask("b")]);
    repo.showRepoImport({
      source: "gitee",
      keyword: "电子科大",
    });
    // 源与索引地址按 (类别, 源) 回填，关键词已预填
    expect(repo.repoImport.value.source).toBe("gitee");
    expect(repo.repoImport.value.url).toBe(presetRepoIndexUrl("browser", "gitee"));
    expect(repo.repoImport.value.searchQuery).toBe("电子科大");
    // 所有入口打开后都会在后台读取索引
    expect(repo.repoImport.value.loading).toBe(true);
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    // 拉取完成后仍保留向导预填的关键词
    expect(repo.repoImport.value.loaded).toBe(true);
    expect(repo.repoImport.value.searchQuery).toBe("电子科大");
  });

  it("不带 opts 的普通打开复位关键词并自动读取全部索引", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([]);
    repo.showRepoImport({ source: "gitee", keyword: "某学校" });
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));

    repo.showRepoImport();
    expect(repo.repoImport.value.searchQuery).toBe("");
    expect(repo.repoImport.value.loading).toBe(true);
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    expect(repoApi.fetchIndex).toHaveBeenCalledWith(presetRepoIndexUrl("browser", "gitee"));
    expect(repoApi.fetchIndex).toHaveBeenCalledWith(presetRepoIndexUrl("http", "gitee"));
    expect(repo.repoImport.value.tasks).toEqual([]);
  });

  it("afterImport 提供时接管收尾：不打开编辑器、不跳路由，回调收到最终 id", async () => {
    const afterImport = vi.fn(async () => {});
    repo.showRepoImport({ afterImport });
    repo.repoImport.value.disclaimer = makeTypedTask("dorm", "http");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({
      type: "http",
      name: "宿舍HTTP 登录",
      method: "GET",
      url: "http://10.0.0.1/login",
    });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(afterImport).toHaveBeenCalledWith("dorm", "http");
    expect(showHttpTaskEditorMock).not.toHaveBeenCalled();
    expect(routerPushMock).not.toHaveBeenCalled();
    expect(repo.repoImport.value.visible).toBe(false);
  });

  it("afterImport 抛错时提示已导入并关闭弹窗，避免再次确认重复保存", async () => {
    const afterImport = vi.fn(async () => {
      throw new Error("绑定失败");
    });
    repo.showRepoImport({ afterImport });
    repo.repoImport.value.disclaimer = makeTask("campus");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", name: "校园登录", steps: [] });

    await repo.acceptRepoDisclaimer();

    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(repo.repoImport.value.visible).toBe(false);
    const [ok, message] = toastOnlyMock.mock.calls.at(-1) as [boolean, string];
    expect(ok).toBe(false);
    expect(message).toContain("已导入");
    expect(message).toContain("绑定失败");
  });

  it("首次下载失败后重试成功仍回调向导", async () => {
    const afterImport = vi.fn(async () => {});
    repo.showRepoImport({ afterImport });
    const task = makeTask("campus");
    vi.mocked(repoApi.fetchTask)
      .mockRejectedValueOnce(new Error("网络中断"))
      .mockResolvedValueOnce({ type: "browser", name: "校园登录", steps: [] });
    repo.repoImport.value.disclaimer = task;
    await repo.acceptRepoDisclaimer();
    expect(afterImport).not.toHaveBeenCalled();
    repo.repoImport.value.disclaimer = task;
    await repo.acceptRepoDisclaimer();
    expect(afterImport).toHaveBeenCalledWith("campus", "browser");
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
  it("多个关键词可分别命中名称、作者和标签，且需全部命中", () => {
    const tasks = [{ ...makeTask("x"), name: "校园登录", author: "Misyra", tags: ["电信"] }];
    expect(filterRepoTasks(tasks, "browser", "校园  MISYRA\t电信")).toHaveLength(1);
    expect(filterRepoTasks(tasks, "browser", "校园 移动")).toHaveLength(0);
  });
});

describe("仓库导入状态与错误恢复", () => {
  it("搜索隐藏当前任务时清空选中项", () => {
    repo.showRepoImport();
    const task = makeTask("campus");
    repo.repoImport.value.tasks = [task];
    repo.selectRepoTask(task);
    repo.repoImport.value.searchQuery = "不存在的学校";
    expect(repo.repoImport.value.selected).toBeNull();
  });

  it("脚本下载中切源时忽略旧源结果", async () => {
    repo.showRepoImport();
    let resolveTask!: (data: Record<string, unknown>) => void;
    vi.mocked(repoApi.fetchTask).mockImplementationOnce(() => new Promise((resolve) => { resolveTask = resolve; }));
    const pending = repo.confirmRepoImport(makeTypedTask("old", "script"));
    repo.selectRepoSource("gitee");
    resolveTask({ type: "script", content: "print('old')" });
    await pending;
    expect(repo.repoImport.value.disclaimer).toBeNull();
    expect(repo.repoImport.value.scriptPreview).toBe("");
    expect(repo.repoImport.value.previewLoading).toBe(false);
  });

  it("重复点击只保存一次，导入中不能关闭、切源或重新打开", async () => {
    repo.showRepoImport();
    repo.repoImport.value.disclaimer = makeTask("campus");
    let resolveTask!: (data: Record<string, unknown>) => void;
    vi.mocked(repoApi.fetchTask).mockImplementationOnce(() => new Promise((resolve) => { resolveTask = resolve; }));
    const pending = repo.acceptRepoDisclaimer();
    await repo.acceptRepoDisclaimer();
    repo.closeRepoImport();
    repo.cancelRepoDisclaimer();
    repo.selectRepoSource("gitee");
    repo.showRepoImport();
    expect(repo.repoImport.value.importing).toBe(true);
    expect(repo.repoImport.value.visible).toBe(true);
    expect(repo.repoImport.value.source).toBe("github");
    expect(repo.repoImport.value.disclaimer?.id).toBe("campus");
    resolveTask({ type: "browser", steps: [] });
    await pending;
    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(repo.repoImport.value.importing).toBe(false);
    expect(repo.repoImport.value.visible).toBe(false);
    expect(repo.repoImport.value.disclaimer).toBeNull();
  });

  it("下载失败保留确认内容，重试可完成导入", async () => {
    repo.showRepoImport();
    repo.repoImport.value.disclaimer = makeTask("campus");
    vi.mocked(repoApi.fetchTask).mockRejectedValueOnce(new Error("连接中断"))
      .mockResolvedValueOnce({ type: "browser", steps: [] });
    await repo.acceptRepoDisclaimer();
    expect(repo.repoImport.value.importError).toContain("连接中断");
    expect(repo.repoImport.value.disclaimer?.id).toBe("campus");
    expect(repo.repoImport.value.importing).toBe(false);
    await repo.acceptRepoDisclaimer();
    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(repo.repoImport.value.visible).toBe(false);
  });

  it("脚本保存失败重试仍使用已确认正文，不重新下载", async () => {
    repo.showRepoImport();
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "script", content: "print('reviewed')" });
    await repo.confirmRepoImport(makeTypedTask("script", "script"));
    tasksApiMock.save.mockRejectedValueOnce(new Error("写入失败"));
    await repo.acceptRepoDisclaimer();
    expect(repo.repoImport.value.scriptPreview).toBe("print('reviewed')");
    await repo.acceptRepoDisclaimer();
    expect(repoApi.fetchTask).toHaveBeenCalledTimes(1);
    expect(tasksApiMock.save).toHaveBeenLastCalledWith("script", expect.objectContaining({ content: "print('reviewed')" }));
  });

  it("已保存但编辑器打开失败时说明实际结果，不保留再次保存入口", async () => {
    repo.showRepoImport();
    repo.repoImport.value.disclaimer = makeTask("campus");
    vi.mocked(repoApi.fetchTask).mockResolvedValue({ type: "browser", steps: [] });
    showTaskEditorMock.mockRejectedValueOnce(new Error("详情加载失败"));
    await repo.acceptRepoDisclaimer();
    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    expect(repo.repoImport.value.disclaimer).toBeNull();
    expect(toastOnlyMock).toHaveBeenLastCalledWith(false, expect.stringContaining("任务已导入（campus）"));
  });

  it("索引已过缓存有效期时刷新失败仍保留列表和选中项", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("old")]);
    repo.showRepoImport();
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    repo.selectRepoTask(repo.repoImport.value.tasks[0]!);
    repo.repoImport.value.fetchedAt = Date.now() - REPO_INDEX_CACHE_TTL_MS - 1;
    vi.mocked(repoApi.fetchIndex).mockRejectedValueOnce(new Error("离线"));
    await repo.fetchRepoIndex();
    expect(repo.repoImport.value.tasks[0]?.id).toBe("old");
    expect(repo.repoImport.value.selected?.id).toBe("old");
    expect(repo.repoImport.value.loaded).toBe(true);
  });

  it("条目字段损坏或 ID 重复时拒绝整份索引，不污染有效列表", async () => {
    vi.mocked(repoApi.fetchIndex).mockResolvedValue([makeTask("old")]);
    repo.showRepoImport();
    await vi.waitFor(() => expect(repo.repoImport.value.loading).toBe(false));
    for (const data of [[null], [{ ...makeTask("bad"), tags: "错误字段" }], [makeTask("dup"), makeTask("dup")]]) {
      vi.mocked(repoApi.fetchIndex).mockResolvedValueOnce(data as never);
      await repo.fetchRepoIndex();
      expect(repo.repoImport.value.error).toContain("索引条目无效");
      expect(repo.repoImport.value.tasks[0]?.id).toBe("old");
    }
  });

  it("自定义地址协议无效时在请求前显示错误，隐藏仓库链接", async () => {
    repo.selectRepoSource("custom");
    repo.repoImport.value.url = "javascript:alert(1)";
    await repo.fetchRepoIndex();
    expect(repoApi.fetchIndex).not.toHaveBeenCalled();
    expect(repo.repoImport.value.error).toContain("HTTP 或 HTTPS");
    expect(repo.sourceHomeUrl.value).toBe("");
  });
});
