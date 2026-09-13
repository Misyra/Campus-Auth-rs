/**
 * 编辑器离开守卫的单元测试（FE2-9）。
 * 覆盖：非编辑页导航直放、脏草稿确认抢占阻断且不清草稿、确认放行且显式清空、
 * 干净草稿不弹确认。useConfirm/useTasks/useScripts 全部打桩，不触真实单例。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const confirmMock = vi.fn();
const isTaskDirty = vi.fn(() => false);
const clearTaskDraft = vi.fn();
const isScriptDirty = vi.fn(() => false);
const clearScriptDraft = vi.fn();

vi.mock("../composables/useConfirm", () => ({
  useConfirm: () => ({ confirm: confirmMock }),
}));
vi.mock("../composables/useTasks", () => ({
  useTasks: () => ({ isTaskDirty, clearTaskDraft }),
}));
vi.mock("../composables/useScripts", () => ({
  useScripts: () => ({ isScriptDirty, clearScriptDraft }),
}));

const { guardEditorLeave } = await import("./editorGuard");

beforeEach(() => {
  // 计数按用例隔离：确认/清空桩的调用次数断言不跨用例累积
  confirmMock.mockClear();
  clearTaskDraft.mockClear();
  clearScriptDraft.mockClear();
  isTaskDirty.mockReturnValue(false);
  isScriptDirty.mockReturnValue(false);
});

describe("guardEditorLeave", () => {
  it("非编辑页离开不拦截、不弹确认", async () => {
    const result = await guardEditorLeave({ path: "/about" }, { path: "/settings" });
    expect(result).toBe(true);
    expect(confirmMock).not.toHaveBeenCalled();
  });

  it("编辑页内部导航（/tasks → /tasks 深链变化）不拦截", async () => {
    const result = await guardEditorLeave({ path: "/tasks" }, { path: "/tasks" });
    expect(result).toBe(true);
    expect(confirmMock).not.toHaveBeenCalled();
  });

  it("脏任务草稿：确认被拒/被抢占均阻断且不清草稿", async () => {
    isTaskDirty.mockReturnValue(true);
    // 取消（false）
    confirmMock.mockResolvedValueOnce(false);
    expect(await guardEditorLeave({ path: "/about" }, { path: "/tasks" })).toBe(false);
    expect(clearTaskDraft).not.toHaveBeenCalled();
    // 被抢占（null）：同样阻断，不能按"确认"处理
    confirmMock.mockResolvedValueOnce(null);
    expect(await guardEditorLeave({ path: "/about" }, { path: "/tasks" })).toBe(false);
    expect(clearTaskDraft).not.toHaveBeenCalled();
  });

  it("脏任务草稿：确认后显式清空草稿再放行", async () => {
    isTaskDirty.mockReturnValue(true);
    confirmMock.mockResolvedValueOnce(true);
    const result = await guardEditorLeave({ path: "/about" }, { path: "/tasks" });
    expect(result).toBe(true);
    expect(clearTaskDraft).toHaveBeenCalledTimes(1);
    expect(confirmMock).toHaveBeenCalledWith(
      expect.objectContaining({ title: "放弃未保存的修改", danger: true }),
    );
  });

  it("干净任务草稿不弹确认直接放行", async () => {
    isTaskDirty.mockReturnValue(false);
    const result = await guardEditorLeave({ path: "/about" }, { path: "/tasks" });
    expect(result).toBe(true);
    expect(confirmMock).not.toHaveBeenCalled();
    expect(clearTaskDraft).not.toHaveBeenCalled();
  });

  it("脏脚本草稿独立判定与清空", async () => {
    isScriptDirty.mockReturnValue(true);
    confirmMock.mockResolvedValueOnce(true);
    const result = await guardEditorLeave({ path: "/tasks" }, { path: "/scripts" });
    expect(result).toBe(true);
    expect(clearScriptDraft).toHaveBeenCalledTimes(1);
    // 隔离性：任务草稿清理必须一次都没被调用（not.toHaveBeenCalledTimes(2) 会放过
    // “错误清空 1 次”这种漏检）
    expect(clearTaskDraft).not.toHaveBeenCalled();
  });
});
