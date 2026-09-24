/**
 * useScheduledTasks 的自动保存路径与列表行操作测试。
 *
 * 定时任务从「弹窗 + 显式保存」改成「二级编辑页 + 自动保存」后，最容易被写坏的是
 * 那套共享状态机（与 useScripts / useHttpTasks 同构）：
 * 1. 缺口未补齐时必须**跳过落盘**（名称/目标空着就发请求只会换回一句莫名的报错）；
 * 2. 首次落盘走 POST、之后同 id 走 PUT（搞错就会 409 或凭空多出一个任务）；
 * 3. 换编辑对象要在途改动补发（detached 语义：不回头改写新草稿的共享状态）；
 * 4. 删除正在编辑的任务必须关掉编辑器——否则自动保存可能写回一个已删除的 id。
 *
 * 依赖全部 mock（api / toast / confirm / 任务目录），只关心状态机与请求参数；
 * debounce 用假定时器推进，不真等 500ms。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { ScheduledTask } from "../api/types";

const { apiMock, toastOnlyMock, confirmMock, directory } = vi.hoisted(() => ({
  // 参数类型显式写出来：vi.fn 的参数为空时 mock.calls 是 `[]` 元组，取 [0] 会被 TS 拦下，
  // 而"断言发出去的载荷长什么样"正是本文件的主要目的。
  apiMock: {
    list: vi.fn(async () => [] as unknown[]),
    create: vi.fn(async (_payload: Record<string, unknown>) => ({ message: "保存成功" })),
    update: vi.fn(async (_id: string, _payload: Record<string, unknown>) => ({ message: "保存成功" })),
    delete: vi.fn(async (_id: string) => ({ message: "删除成功" })),
    toggle: vi.fn(async (_id: string) => ({ message: "操作成功" })),
    run: vi.fn(async (_id: string) => ({ message: "ok" })),
    history: vi.fn(async (_id: string) => ({ runs: [] })),
  },
  toastOnlyMock: vi.fn((_ok: boolean, _message?: string) => {}),
  confirmMock: vi.fn(async (_opts?: unknown) => true),
  directory: {
    browserTasks: { value: [] as Array<{ id: string; name: string }> },
    scripts: { value: [] as Array<{ id: string; name: string }> },
    loaded: { value: true },
  },
}));

vi.mock("../api", () => ({ scheduledTasksApi: apiMock }));
vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: toastOnlyMock }) }));
vi.mock("./useConfirm", () => ({ useConfirm: () => ({ confirm: confirmMock }) }));
vi.mock("./useTaskDirectory", () => ({ useTaskDirectory: () => directory }));

const { useScheduledTasks } = await import("./useScheduledTasks");

const st = useScheduledTasks();

/** 列表里的一条任务（编辑页的字段全部来自列表响应，故测试也从列表进入编辑） */
function listTask(overrides: Partial<ScheduledTask> = {}): ScheduledTask {
  return {
    id: "sched_a",
    name: "早八签到",
    description: "",
    task_type: "browser",
    target_id: "browser1",
    cron: "0 8 * * *",
    enabled: true,
    trigger: "cron",
    timeout: 60,
    ...overrides,
  };
}

beforeEach(() => {
  for (const fn of Object.values(apiMock)) vi.mocked(fn).mockClear();
  toastOnlyMock.mockClear();
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  directory.browserTasks.value = [{ id: "browser1", name: "通用登录" }];
  directory.scripts.value = [];
  directory.loaded.value = true;
  st.clearScheduledDraft();
  st.scheduledLoaded.value = false;
  st.scheduledTasks.value.splice(0, st.scheduledTasks.value.length);
  apiMock.list.mockResolvedValue([]);
});

afterEach(() => {
  st.clearScheduledDraft();
});

describe("自动保存：缺口与首次落盘", () => {
  it("名称/目标补齐前不发请求，补齐后以 POST 建任务，之后同 id 走 PUT", async () => {
    vi.useFakeTimers();
    try {
      st.createScheduledDraft();
      const draft = st.scheduledTaskDraft.value!;
      expect(draft._isNew).toBe(true);

      draft.name = "早八签到";
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.create, "还缺目标任务，不该发请求").not.toHaveBeenCalled();

      draft.target_id = "browser1";
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.create).toHaveBeenCalledTimes(1);
      const created = apiMock.create.mock.calls[0][0];
      expect(String(created.id)).toMatch(/^sched_/);
      expect(created).toMatchObject({
        name: "早八签到",
        target_id: "browser1",
        cron: "0 8 * * *",
        enabled: true,
        trigger: "cron",
      });
      // 落盘后 _isNew 翻转：同一 id 不再重复创建
      expect(st.scheduledTaskDraft.value!._isNew).toBe(false);

      st.scheduledTaskDraft.value!.name = "早八签到 v2";
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.create).toHaveBeenCalledTimes(1);
      expect(apiMock.update).toHaveBeenCalledTimes(1);
      expect(apiMock.update.mock.calls[0][0]).toBe(String(created.id));
      expect(apiMock.update.mock.calls[0][1].name).toBe("早八签到 v2");
    } finally {
      vi.useRealTimers();
    }
  });

  it("首发撞上「已被另一发建好」（409）时降级为 PUT，不当成失败", async () => {
    vi.useFakeTimers();
    try {
      const { ApiError } = await import("../api/client");
      st.createScheduledDraft();
      const draft = st.scheduledTaskDraft.value!;
      draft.name = "早八签到";
      draft.target_id = "browser1";

      // 自动保存的两发重叠时，两边都以为自己是第一次：后发那发会拿到 409。
      // 它不是失败（任务就在那儿），必须降级成 PUT 走完，否则用户看到一句
      // 「定时任务 X 已存在」的红字，而任务其实建成功了。
      apiMock.create.mockRejectedValueOnce(
        new ApiError("定时任务 sched_x 已存在", 409, undefined, "CONFLICT"),
      );
      await vi.advanceTimersByTimeAsync(600);

      expect(apiMock.update, "409 应降级为 PUT 继续").toHaveBeenCalledTimes(1);
      expect(apiMock.update.mock.calls[0][0]).toBe(draft.id);
      expect(toastOnlyMock, "不该弹「已存在」的红字").not.toHaveBeenCalled();
      expect(st.scheduledTaskDraft.value!._isNew, "任务已在磁盘上，草稿要翻成非新建").toBe(false);
      expect(st.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("首发因别的原因失败（非 409）照旧报错，不降级", async () => {
    vi.useFakeTimers();
    try {
      const { ApiError } = await import("../api/client");
      st.createScheduledDraft();
      const draft = st.scheduledTaskDraft.value!;
      draft.name = "早八签到";
      draft.target_id = "browser1";

      apiMock.create.mockRejectedValueOnce(new ApiError("目标任务不存在", 400, undefined, "BAD_REQUEST"));
      await vi.advanceTimersByTimeAsync(600);

      expect(apiMock.update, "非冲突不该偷偷改成 PUT").not.toHaveBeenCalled();
      expect(toastOnlyMock).toHaveBeenCalledWith(false, "目标任务不存在");
      expect(st.autosaveState.value).toBe("error");
    } finally {
      vi.useRealTimers();
    }
  });

  it("刚载入的草稿与磁盘一致：不动它就不发请求", async () => {
    vi.useFakeTimers();
    try {
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.update).not.toHaveBeenCalled();
      expect(apiMock.create).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("改回原样也不发请求（判据是载荷指纹，不是「改过就写」）", async () => {
    vi.useFakeTimers();
    try {
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      st.scheduledTaskDraft.value!.name = "改了";
      st.scheduledTaskDraft.value!.name = "早八签到";
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.update).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("目标已不存在时按缺口拦下（否则要等触发时才在日志里看到失败）", async () => {
    vi.useFakeTimers();
    try {
      directory.browserTasks.value = [];
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      expect(st.draftGapsNow.value).toEqual(["有效的目标任务（原目标已不存在）"]);
      st.scheduledTaskDraft.value!.description = "改点别的";
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.update).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("换编辑对象与关闭", () => {
  it("改动还在 debounce 窗口内就切走：仍然落盘，且不污染新草稿", async () => {
    vi.useFakeTimers();
    try {
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      st.scheduledTaskDraft.value!.name = "早八签到改了";
      // 不等 debounce 到点就切到新建草稿
      await st.showScheduledTaskEditor();

      const call = apiMock.update.mock.calls.find((c) => c[0] === "sched_a");
      expect(call, "切走时应补发上一份草稿的改动").toBeTruthy();
      expect(call![1].name).toBe("早八签到改了");

      // 新草稿什么都没填：不该凭空写一次
      await vi.advanceTimersByTimeAsync(600);
      expect(apiMock.create).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("关闭编辑器会补发在途改动（「退出即生效」）", async () => {
    vi.useFakeTimers();
    try {
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      st.scheduledTaskDraft.value!.timeout = 120;
      await st.closeScheduledTaskEditor();
      expect(st.scheduledTaskDraft.value).toBeNull();
      const call = apiMock.update.mock.calls.find((c) => c[0] === "sched_a");
      expect(call, "关闭前应补发在途改动").toBeTruthy();
      expect(call![1].timeout).toBe(120);
    } finally {
      vi.useRealTimers();
    }
  });

  it("缺口态下关闭编辑器会出声（不能静默丢改动）", async () => {
    vi.useFakeTimers();
    try {
      st.scheduledTasks.value.splice(0, 0, listTask());
      await st.showScheduledTaskEditor("sched_a");
      st.scheduledTaskDraft.value!.name = "";
      await st.closeScheduledTaskEditor();
      expect(apiMock.update).not.toHaveBeenCalled();
      const [ok, message] = toastOnlyMock.mock.calls.at(-1) as [boolean, string];
      expect(ok).toBe(false);
      expect(message).toContain("改动未保存");
      expect(message).toContain("任务名称");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("列表行操作", () => {
  it("打开不存在的任务给出提示且不建草稿", async () => {
    await st.showScheduledTaskEditor("ghost");
    expect(st.scheduledTaskDraft.value).toBeNull();
    const [ok, message] = toastOnlyMock.mock.calls.at(-1) as [boolean, string];
    expect(ok).toBe(false);
    expect(message).toContain("ghost");
  });

  it("删除正在编辑的任务会关掉编辑器（自动保存不再写回已删除的 id）", async () => {
    st.scheduledTasks.value.splice(0, 0, listTask());
    await st.showScheduledTaskEditor("sched_a");
    await st.deleteScheduledTask("sched_a");
    expect(apiMock.delete).toHaveBeenCalledWith("sched_a");
    expect(st.scheduledTaskDraft.value).toBeNull();
  });

  it("新建草稿上的删除是「放弃」：不发请求、清空草稿", async () => {
    st.createScheduledDraft();
    const id = st.scheduledTaskDraft.value!.id;
    await st.deleteScheduledTask(id);
    expect(apiMock.delete).not.toHaveBeenCalled();
    expect(st.scheduledTaskDraft.value).toBeNull();
  });

  it("用户取消确认时不删除", async () => {
    confirmMock.mockResolvedValue(false);
    st.scheduledTasks.value.splice(0, 0, listTask());
    await st.deleteScheduledTask("sched_a");
    expect(apiMock.delete).not.toHaveBeenCalled();
  });

  it("立即运行的提示是「已排入执行」（后端只回排入，成败要看历史）", async () => {
    st.scheduledTasks.value.splice(0, 0, listTask());
    await st.runScheduledTask("sched_a");
    expect(apiMock.run).toHaveBeenCalledWith("sched_a");
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "已触发执行，结果见「执行历史」");
  });

  it("运行中连点只发一次请求", async () => {
    let release: (v: { message: string }) => void = () => {};
    apiMock.run.mockImplementationOnce(
      () =>
        new Promise((r) => {
          release = r;
        }),
    );
    st.scheduledTasks.value.splice(0, 0, listTask());
    const first = st.runScheduledTask("sched_a");
    await st.runScheduledTask("sched_a");
    expect(apiMock.run).toHaveBeenCalledTimes(1);
    release({ message: "ok" });
    await first;
  });
});

describe("列表加载", () => {
  it("成功拉取后标记就绪（?task= 的解析据此才敢判定「任务不存在」）", async () => {
    apiMock.list.mockResolvedValue([listTask()]);
    await st.loadScheduledTasks(true);
    expect(st.scheduledLoaded.value).toBe(true);
    expect(st.scheduledTasks.value.map((t) => t.id)).toEqual(["sched_a"]);
  });

  it("拉取失败不标记就绪并提示一次", async () => {
    apiMock.list.mockRejectedValue(new Error("boom"));
    await st.loadScheduledTasks(true);
    expect(st.scheduledLoaded.value).toBe(false);
    const [ok, message] = toastOnlyMock.mock.calls.at(-1) as [boolean, string];
    expect(ok).toBe(false);
    expect(message).toContain("boom");
  });
});
