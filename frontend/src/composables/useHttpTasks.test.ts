/**
 * useHttpTasks 的保存 / 删除 / 复制路径单元测试。
 *
 * 这几条是最容易悄悄坏掉的地方：
 * 1. 保存前拦缺口（ID 形态、请求地址、名称）——放过去就会在 `<base>/tasks/http/`
 *    写出一个 ID 非法或没有地址、登录必然失败的任务；
 * 2. 非空 `crypto_script` 必须弹一次确认（登录时要执行其中的 JS），拒绝即不保存；
 * 3. 删除正在编辑的任务必须关掉编辑器——否则"删除"看起来没生效（再点保存会以同一
 *    ID 新建回来）。
 *
 * 依赖全部 mock：本测试只关心状态机与请求参数，不碰真实 HTTP。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
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
const { setHttpTaskDraft, clearHttpTaskDraft, saveHttpTask, deleteHttpTask, isDraftDirty } = useHttpTasks();

beforeEach(() => {
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  toastOnlyMock.mockReset();
  tasksApiMock.save.mockClear();
  tasksApiMock.delete.mockClear();
  tasksApiMock.list.mockClear();
  clearHttpTaskDraft();
});

describe("saveHttpTask 的前置校验", () => {
  it("ID 形态非法时不发请求", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "宿舍 直连", url: "http://10.0.0.1/login" });
    await saveHttpTask();
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, expect.stringContaining("任务ID需为"));
  });

  it("请求地址为空时不发请求（缺了必然登不上）", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm" });
    await saveHttpTask();
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, expect.stringContaining("请求地址"));
  });

  it("名称为空时不发请求（列表/下拉里不能出现无名条目）", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm", name: "   ", url: "http://10.0.0.1/login" });
    await saveHttpTask();
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "请填写任务名称");
  });
});

describe("saveHttpTask 的凭据变换脚本确认", () => {
  it("含脚本时先确认，拒绝则不保存且保留草稿", async () => {
    confirmMock.mockResolvedValue(false);
    setHttpTaskDraft({
      ...emptyHttpTaskDraft(),
      id: "dorm",
      url: "http://10.0.0.1/login",
      crypto_script: "function transform(ctx) { return ctx; }",
    });

    await saveHttpTask();

    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(tasksApiMock.save).not.toHaveBeenCalled();
    // 草稿保留：用户拒绝的是"保存"，不是"丢掉刚写的脚本"
    expect(useHttpTasks().httpTaskDraft.value?.id).toBe("dorm");
  });

  it("无脚本时不打扰用户（大多数门户不需要脚本）", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm", url: "http://10.0.0.1/login" });
    await saveHttpTask();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
  });
});

describe("saveHttpTask 的落盘载荷", () => {
  it("带上 type=http、trim 后的地址，保存成功即清草稿并刷新列表", async () => {
    setHttpTaskDraft({
      ...emptyHttpTaskDraft(),
      id: " dorm ",
      name: "  宿舍直连  ",
      url: "  http://10.0.0.1/login?username={username}  ",
    });

    await saveHttpTask();

    expect(tasksApiMock.save).toHaveBeenCalledTimes(1);
    const [id, payload] = tasksApiMock.save.mock.calls[0] as [string, Record<string, unknown>];
    expect(id).toBe("dorm");
    expect(payload).toMatchObject({
      type: "http",
      task_id: "dorm",
      name: "宿舍直连",
      url: "http://10.0.0.1/login?username={username}",
    });
    expect(confirmMock).not.toHaveBeenCalled();
    expect(tasksApiMock.list).toHaveBeenCalledTimes(1);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "保存成功");
  });

  it("保存失败时保留草稿并提示（用户不必重填）", async () => {
    tasksApiMock.save.mockRejectedValueOnce(new Error("磁盘错误"));
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm", url: "http://10.0.0.1/login" });

    await saveHttpTask();

    expect(toastOnlyMock).toHaveBeenCalledWith(false, expect.stringContaining("磁盘错误"));
    expect(useHttpTasks().httpTaskDraft.value?.id).toBe("dorm");
  });
});

describe("deleteHttpTask", () => {
  it("删除的正是当前编辑对象时关掉编辑器", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm", _isNew: false, url: "http://10.0.0.1/login" });
    await deleteHttpTask("dorm");
    expect(tasksApiMock.delete).toHaveBeenCalledWith("dorm");
    // 草稿清空 = 编辑器关闭；留着会让人以为"还能保存"，保存其实会以同一 ID 新建回来
    expect(isDraftDirty()).toBe(false);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "直连任务已删除");
  });

  it("用户取消确认时不删除", async () => {
    confirmMock.mockResolvedValue(false);
    await deleteHttpTask("dorm");
    expect(tasksApiMock.delete).not.toHaveBeenCalled();
  });

  it("删除的是别的任务时不动当前草稿", async () => {
    setHttpTaskDraft({ ...emptyHttpTaskDraft(), id: "dorm", _isNew: false, url: "http://10.0.0.1/login" });
    await deleteHttpTask("other");
    expect(isDraftDirty()).toBe(false);
    expect(useHttpTasks().httpTaskDraft.value?.id).toBe("dorm");
  });
});
