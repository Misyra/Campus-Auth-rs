/**
 * useHttpTasks 自动保存路径的单元测试。
 *
 * 自动保存模式（方案 G）下最容易悄悄坏掉的地方：
 * 1. 缺口校验（ID 形态、请求地址）——缺口未补齐时自动保存**静默跳过**（不发请求、
 *    不打扰编辑），补齐后恢复落盘；
 * 2. 落盘载荷正确（type=http、trim 后的地址与 id）；
 * 3. 删除正在编辑的任务必须关掉编辑器——否则自动保存可能写回一个已删除的 id。
 *
 * 依赖全部 mock：本测试只关心状态机与请求参数，不碰真实 HTTP。
 * debounce 用 vi.useFakeTimers 推进，避免等待真实 500ms。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { emptyHttpTaskDraft } from "../utils/httpTask";

const { confirmMock, toastOnlyMock, tasksApiMock } = vi.hoisted(() => ({
  confirmMock: vi.fn(async () => true),
  toastOnlyMock: vi.fn(),
  tasksApiMock: {
    list: vi.fn(async () => []),
    get: vi.fn(),
    save: vi.fn(async (..._args: unknown[]) => ({ message: "保存成功" })),
    delete: vi.fn(async (..._args: unknown[]) => ({ message: "删除成功" })),
    export: vi.fn(),
    import: vi.fn(),
  },
}));

vi.mock("../api", () => ({
  tasksApi: tasksApiMock,
  httpTasksApi: { test: vi.fn() },
}));

vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: toastOnlyMock }) }));
vi.mock("./useConfirm", () => ({ useConfirm: () => ({ confirm: confirmMock }) }));

const { useHttpTasks } = await import("./useHttpTasks");

beforeEach(() => {
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  toastOnlyMock.mockReset();
  tasksApiMock.save.mockClear();
  tasksApiMock.delete.mockClear();
  tasksApiMock.list.mockClear();
  tasksApiMock.get.mockReset();
});

afterEach(() => {
  useHttpTasks().clearHttpTaskDraft();
});

describe("自动保存的缺口校验（经 showHttpTaskEditor + debounce）", () => {
  it("请求地址补齐前不发请求，补齐后恢复落盘", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockResolvedValue({
        summary: { id: "dorm", name: "宿舍直连", task_type: "http" },
        config: { type: "http", task_id: "dorm", name: "宿舍直连", url: "" },
      });
      const http = useHttpTasks();
      await http.showHttpTaskEditor("dorm");
      expect(http.httpTaskDraft.value?.id).toBe("dorm");

      // 地址为空：debounce 到点也不发请求（缺口未补齐，发了必 400）
      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save).not.toHaveBeenCalled();

      // 补上地址：debounce 到点后落盘
      if (http.httpTaskDraft.value) http.httpTaskDraft.value.url = "http://10.0.0.1/login";
      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("换编辑对象时补发在途改动", () => {
  it("改动还在 debounce 窗口内就切走：仍然落盘，且不污染新草稿", async () => {
    vi.useFakeTimers();
    try {
      tasksApiMock.get.mockImplementation(async (id: string) => ({
        summary: { id, name: id, task_type: "http" },
        config: { type: "http", task_id: id, name: id, url: "http://10.0.0.1/login" },
      }));
      const http = useHttpTasks();
      await http.showHttpTaskEditor("dorm");
      // 改一个字段后**不等** debounce 到点就切到另一条：原来 watcher 只会把定时器
      // 清掉（换 id 直接 return），这半秒内的编辑既没落盘也没提示地消失
      if (http.httpTaskDraft.value) http.httpTaskDraft.value.name = "宿舍直连改";
      await http.showHttpTaskEditor("office");

      const dormCall = tasksApiMock.save.mock.calls.find((c) => c[0] === "dorm");
      expect(dormCall, "切走时应补发上一份草稿的改动").toBeTruthy();
      expect((dormCall?.[1] as Record<string, unknown>).name).toBe("宿舍直连改");
      expect(http.httpTaskDraft.value?.id).toBe("office");

      // 新草稿刚载入、用户还没碰过它：不该凭空写一次
      // （旧草稿那一发的响应若回头改写 lastSavedFingerprint 就会在这里露出来）
      await vi.advanceTimersByTimeAsync(600);
      expect(tasksApiMock.save.mock.calls.map((c) => c[0])).not.toContain("office");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("删除路径", () => {
  it("删除的正是当前编辑对象时关掉编辑器（自动保存不再写回已删除的 id）", async () => {
    tasksApiMock.get.mockResolvedValue({
      summary: { id: "dorm", name: "宿舍直连", task_type: "http" },
      config: { type: "http", task_id: "dorm", name: "宿舍直连", url: "http://10.0.0.1/login" },
    });
    const http = useHttpTasks();
    await http.showHttpTaskEditor("dorm");
    expect(http.httpTaskDraft.value?.id).toBe("dorm");

    await http.deleteHttpTask("dorm");
    expect(tasksApiMock.delete).toHaveBeenCalledWith("dorm");
    expect(http.httpTaskDraft.value).toBeNull();
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "直连任务已删除");
  });

  it("用户取消确认时不删除", async () => {
    confirmMock.mockResolvedValue(false);
    await useHttpTasks().deleteHttpTask("dorm");
    expect(tasksApiMock.delete).not.toHaveBeenCalled();
  });

  it("删除的是别的任务时不动当前草稿", async () => {
    tasksApiMock.get.mockResolvedValue({
      summary: { id: "dorm", name: "宿舍直连", task_type: "http" },
      config: { type: "http", task_id: "dorm", name: "宿舍直连", url: "http://10.0.0.1/login" },
    });
    const http = useHttpTasks();
    await http.showHttpTaskEditor("dorm");

    await http.deleteHttpTask("other");
    expect(http.httpTaskDraft.value?.id).toBe("dorm");
  });
});

describe("草稿 ⇄ 载荷互转（经 emptyHttpTaskDraft 基准）", () => {
  it("空草稿的缺口是请求地址；_isNew 不再参与判定（新建先落盘）", () => {
    const draft = emptyHttpTaskDraft();
    expect(draft.url).toBe("");
    // 自动保存模式下新建即落盘，_isNew 只是展示语义
    expect(draft._isNew).toBe(true);
  });
});
