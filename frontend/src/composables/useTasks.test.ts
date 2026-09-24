/**
 * useTasks 自动保存路径的单元测试（此前该面板没有测试文件）。
 *
 * 浏览器任务面板的缺口判据与另外两个面板不同：它没有"字段级缺口"，而是
 * **JSON 解析失败即不落盘**（`draftToPayload` 返回 null 并写入 `jsonError`，
 * 编辑器据此标红）。因此这里重点覆盖：
 * 1. JSON 非法时不发请求，修正后恢复落盘；
 * 2. 不改就不发请求 / 改回原样也不发请求（载荷指纹判据）；
 * 3. 删除正在编辑的任务必须关掉编辑器。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const { confirmMock, toastOnlyMock, tasksApiMock } = vi.hoisted(() => ({
  confirmMock: vi.fn(async () => true),
  toastOnlyMock: vi.fn(),
  tasksApiMock: {
    // 返回类型显式写出：vi.fn(async () => []) 会把返回类型推断成 never[]，
    // 随后的 mockResolvedValue([task]) 会报 TS2322（与 save 的参数声明同一个坑）
    list: vi.fn(async (): Promise<unknown[]> => []),
    get: vi.fn(),
    // 显式声明参数：否则推断出零参签名，断言 `mock.calls[0][0]` 触发 TS2493
    save: vi.fn(async (_id: string, _payload: unknown) => ({ message: "保存成功" })),
    delete: vi.fn(async (_id: string) => ({ message: "删除成功" })),
    execute: vi.fn(),
    export: vi.fn(),
    import: vi.fn(),
  },
}));

vi.mock("../api", () => ({ tasksApi: tasksApiMock }));
vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: toastOnlyMock }) }));
vi.mock("./useConfirm", () => ({ useConfirm: () => ({ confirm: confirmMock }) }));

const { useTasks } = await import("./useTasks");
const { useTaskDirectory } = await import("./useTaskDirectory");

/** 已落盘浏览器任务的服务端形态（tasksApi.get 的返回：TaskDetail） */
function serverTask(config: Record<string, unknown> = {}) {
  const merged = {
    type: "browser",
    name: "宿舍登录",
    description: "",
    url: "http://10.0.0.1/login",
    steps: [{ type: "goto", url: "{{LOGIN_URL}}" }],
    ...config,
  };
  return {
    summary: { id: "dorm", name: "宿舍登录", description: "", task_type: "browser" },
    config: merged,
  };
}

beforeEach(() => {
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  toastOnlyMock.mockReset();
  tasksApiMock.save.mockClear();
  tasksApiMock.delete.mockClear();
  tasksApiMock.get.mockReset();
  tasksApiMock.list.mockClear();
});

afterEach(() => {
  useTasks().clearTaskDraft();
});

describe("useTasks 自动保存", () => {
  it("刚载入的任务不动它就不发请求", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue(serverTask());
      const t = useTasks();
      await t.showTaskEditor("dorm");
      expect(t.editingTask.value?.id).toBe("dorm");

      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save).not.toHaveBeenCalled();
      expect(t.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("JSON 非法时不落盘并标红，修正后恢复落盘", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue(serverTask());
      const t = useTasks();
      await t.showTaskEditor("dorm");

      // 语法错：draftToPayload 返回 null，自动保存必须拦下（否则会把坏 JSON 推给后端）
      t.editingTask.value!.json = "{ not json";
      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save).not.toHaveBeenCalled();
      expect(t.jsonError.value).not.toBe("");

      // 修正成合法且与磁盘不同的内容：恢复落盘
      t.editingTask.value!.json = JSON.stringify(
        { type: "browser", name: "宿舍登录", description: "", url: "http://10.0.0.1/login", steps: [] },
        null,
        2,
      );
      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
      expect(tasksApiMock.save.mock.calls[0][0]).toBe("dorm");
      expect(t.jsonError.value).toBe("");
      expect(t.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("改动后再改回原样：仍然不发请求（判据是载荷指纹）", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue(serverTask());
      const t = useTasks();
      await t.showTaskEditor("dorm");

      const draft = t.editingTask.value!;
      const original = draft.json;
      draft.json = JSON.stringify({ type: "browser", name: "改了", steps: [] }, null, 2);
      await vi.advanceTimersByTimeAsync(100); // 仍在 debounce 窗口内
      draft.json = original;
      await vi.advanceTimersByTimeAsync(600);

      expect(tasksApiMock.save).not.toHaveBeenCalled();
      expect(t.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("删除正在编辑的任务会关掉编辑器（自动保存不再写回已删除的 id）", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue(serverTask());
      const t = useTasks();
      await t.showTaskEditor("dorm");
      expect(t.editingTask.value).not.toBeNull();

      await t.deleteTask("dorm");
      expect(tasksApiMock.delete).toHaveBeenCalledWith("dorm");
      expect(t.editingTask.value).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("用户取消确认时不删除，草稿保留", async () => {
    vi.useFakeTimers();
    try {
      confirmMock.mockResolvedValue(false);
      tasksApiMock.get.mockResolvedValue(serverTask());
      const t = useTasks();
      await t.showTaskEditor("dorm");

      await t.deleteTask("dorm");
      expect(tasksApiMock.delete).not.toHaveBeenCalled();
      expect(t.editingTask.value?.id).toBe("dorm");
    } finally {
      vi.useRealTimers();
    }
  });

  it("新建/副本的 id 与另外两类任务不撞车（后端会删掉别桶同 id 的文件）", async () => {
    vi.useFakeTimers();
    const dir = useTaskDirectory();
    try {
      // 目录里已有直连任务的 untitled_1 与脚本的 untitled_2：浏览器任务新建时必须跳过它们。
      // 只看本类列表取号的话，这里会得到 untitled_1，随后 PUT 到 /api/tasks/untitled_1
      // （type=browser）会把 http 桶里那份文件删掉——静默丢一条任务。
      tasksApiMock.list.mockResolvedValue([
        { id: "untitled_1", name: "直连", task_type: "http", type: "http", url: "http://10.0.0.1/" },
        { id: "untitled_2", name: "脚本", task_type: "script", url: "" },
      ]);
      await dir.fetchDirectory(true);

      const t = useTasks();
      t.createTask();
      expect(t.editingTask.value?.id).toBe("untitled_3");

      // 副本同样按三类并集去重：baseId 撞上已存在的 http 任务时继续递增
      tasksApiMock.get.mockResolvedValue({
        summary: { id: "untitled_1", name: "直连", description: "", task_type: "http" },
        config: { type: "http", name: "直连", url: "http://10.0.0.1/" },
      });
      await t.duplicateTask("untitled_1");
      // baseId 规范化成 "untitled"，候选 untitled_copy → 不在并集里，故直接可用
      expect(tasksApiMock.save.mock.calls.at(-1)?.[0]).toBe("untitled_copy");
    } finally {
      // 目录是模块级单例：把这份测试数据清掉，免得影响同文件里其他用例
      tasksApiMock.list.mockResolvedValue([]);
      await dir.fetchDirectory(true);
      vi.useRealTimers();
    }
  });

  it("危险步骤能被认出来（编辑器据此常驻提示，旧版是保存前确认）", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue(
        serverTask({
          steps: [
            { type: "goto", url: "{{LOGIN_URL}}" },
            { type: "evaluate", script: "return 1" },
          ],
        }),
      );
      const t = useTasks();
      await t.showTaskEditor("dorm");

      // `evaluate` 能在页面上下文跑任意 JS：自动保存模式下没有"保存前"，提示必须在编辑页常驻
      expect(t.dangerousSteps.value).toHaveLength(1);
      expect(t.dangerousSteps.value[0].stepIndex).toBe(2);
      expect(t.dangerousSteps.value[0].stepType).toBe("evaluate");

      // JSON 语法错误时不误报（那种内容根本不会落盘，另有 JSON 错误提示）
      t.editingTask.value!.json = "{ not json";
      expect(t.dangerousSteps.value).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("乱序到达的旧列表快照会被丢弃（否则编辑器会被误判成「任务已删除」）", async () => {
    const dir = useTaskDirectory();
    try {
      // 第一发慢（挂起）、第二发快：两发都写列表的话，旧快照后到会让列表倒退，
      // 于是"刚刚还在编辑的这个任务"从不存在的列表里被判为已删除 → 编辑器被关掉、
      // 未落盘的改动丢掉。世代号（fetchEpoch）只允许最新那一发落盘。
      let releaseSlow: (v: unknown[]) => void = () => {};
      tasksApiMock.list.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseSlow = resolve;
          }),
      );
      const slow = dir.fetchDirectory(true);

      tasksApiMock.list.mockResolvedValueOnce([
        { id: "keep", name: "新列表", task_type: "browser", type: "browser", url: "" },
      ]);
      await dir.fetchDirectory(true);
      expect(dir.browserTasks.value.map((t) => t.id)).toEqual(["keep"]);

      // 旧快照（不含 keep）现在才回来：不得写进列表
      releaseSlow([{ id: "stale", name: "旧列表", task_type: "browser", url: "" }]);
      await slow;
      expect(dir.browserTasks.value.map((t) => t.id), "过期快照不该覆盖新列表").toEqual(["keep"]);
    } finally {
      tasksApiMock.list.mockResolvedValue([]);
      await dir.fetchDirectory(true);
    }
  });
});
