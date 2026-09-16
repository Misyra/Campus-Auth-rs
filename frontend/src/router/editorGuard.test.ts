/**
 * 编辑器离开守卫的单元测试（FE2-9）。
 * 覆盖：非编辑页导航直放、任务/脚本两个 Tab 间切换直放（草稿各自保留）、
 * 脏草稿确认抢占阻断且不清草稿、确认放行且显式清空、两块草稿同时脏时逐个判定。
 * useConfirm/useTasks/useScripts 全部打桩，不触真实单例。
 *
 * 直连登录 Tab 相关用例已随该 Tab 一并移除：直连参数归回「配置方案」页编辑，
 * 其草稿由 useProfiles 的 dirty 快照与编辑器同生命周期负责，不再跨路由存活。
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

  it("任务页两个 Tab 间切换不拦截：两块草稿各自持有，切回仍在", async () => {
    isTaskDirty.mockReturnValue(true);
    isScriptDirty.mockReturnValue(true);
    // 浏览器任务 Tab → 脚本 Tab
    expect(await guardEditorLeave({ path: "/tasks/scripts" }, { path: "/tasks" })).toBe(true);
    // 脚本 Tab → 浏览器任务 Tab
    expect(await guardEditorLeave({ path: "/tasks" }, { path: "/tasks/scripts" })).toBe(true);
    expect(confirmMock).not.toHaveBeenCalled();
    // 关键：Tab 切换绝不能顺手清草稿（否则切回来内容没了）
    expect(clearTaskDraft).not.toHaveBeenCalled();
    expect(clearScriptDraft).not.toHaveBeenCalled();
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

  it("离开脚本 Tab 也判定脚本草稿（两 Tab 同处 /tasks 区域）", async () => {
    isScriptDirty.mockReturnValue(true);
    confirmMock.mockResolvedValueOnce(true);
    const result = await guardEditorLeave({ path: "/about" }, { path: "/tasks/scripts" });
    expect(result).toBe(true);
    expect(clearScriptDraft).toHaveBeenCalledTimes(1);
    // 隔离性：任务草稿清理必须一次都没被调用（not.toHaveBeenCalledTimes(2) 会放过
    // “错误清空 1 次”这种漏检）
    expect(clearTaskDraft).not.toHaveBeenCalled();
  });

  it("两块草稿同时脏：逐块确认，前一块被拒即阻断且不清理任何草稿", async () => {
    isTaskDirty.mockReturnValue(true);
    isScriptDirty.mockReturnValue(true);
    // 第一块（任务）被拒 → 立即阻断，脚本块不再询问、两块草稿都保留
    confirmMock.mockResolvedValueOnce(false);
    expect(await guardEditorLeave({ path: "/about" }, { path: "/tasks" })).toBe(false);
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(clearTaskDraft).not.toHaveBeenCalled();
    expect(clearScriptDraft).not.toHaveBeenCalled();
  });

  it("两块草稿同时脏且都确认：两块都清理后放行", async () => {
    isTaskDirty.mockReturnValue(true);
    isScriptDirty.mockReturnValue(true);
    confirmMock.mockResolvedValueOnce(true);
    confirmMock.mockResolvedValueOnce(true);
    expect(await guardEditorLeave({ path: "/about" }, { path: "/tasks" })).toBe(true);
    expect(clearTaskDraft).toHaveBeenCalledTimes(1);
    expect(clearScriptDraft).toHaveBeenCalledTimes(1);
  });

  it("定时任务 Tab 同处 /tasks 区域：内部切换不拦截，离开才判定", async () => {
    isTaskDirty.mockReturnValue(true);
    // 直链进入定时任务 Tab（/tasks/scheduled）后切回浏览器任务 Tab
    expect(await guardEditorLeave({ path: "/tasks" }, { path: "/tasks/scheduled" })).toBe(true);
    expect(await guardEditorLeave({ path: "/tasks/scheduled" }, { path: "/tasks" })).toBe(true);
    expect(confirmMock).not.toHaveBeenCalled();
    // 离开整个 /tasks 区域才判定
    confirmMock.mockResolvedValueOnce(true);
    expect(await guardEditorLeave({ path: "/about" }, { path: "/tasks/scheduled" })).toBe(true);
    expect(clearTaskDraft).toHaveBeenCalledTimes(1);
  });
});
