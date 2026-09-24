/**
 * useScripts 自动保存路径的单元测试（此前该面板没有测试文件）。
 *
 * 覆盖自动保存模式下最容易悄悄坏掉的四点：
 * 1. 缺口未补齐时**不发请求**（脚本内容为空 / ID 不合法），补齐后恢复落盘；
 * 2. 不改就不发请求（判据是载荷指纹，不是"进过编辑器"）；
 * 3. 改回原样也不发请求（指纹相等即视为与磁盘一致）；
 * 4. 删除正在编辑的脚本必须关掉编辑器——否则自动保存会写回一个已删除的 id。
 *
 * 依赖全部 mock；debounce 用 fake timers 推进，不等真实 500ms。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { nextTick } from "vue";

const { confirmMock, toastOnlyMock, scriptsApiMock, tasksApiMock } = vi.hoisted(() => ({
  confirmMock: vi.fn(async () => true),
  toastOnlyMock: vi.fn(),
  scriptsApiMock: {
    list: vi.fn(async () => []),
    binaries: vi.fn(async () => [{ path: "/usr/bin/python3", label: "Python 3" }]),
    get: vi.fn(),
    // 显式声明参数：`vi.fn(async () => ...)` 会推断出零参签名，
    // 断言 `mock.calls[0][0]` 时触发 TS2493（useConfig.test.ts 也踩过同一个坑）
    save: vi.fn(async (_id: string, _payload: unknown) => ({ message: "保存成功" })),
    delete: vi.fn(async (_id: string) => ({ message: "删除成功" })),
    run: vi.fn(),
    export: vi.fn(),
    import: vi.fn(),
  },
  tasksApiMock: {
    list: vi.fn(async () => []),
    get: vi.fn(),
    save: vi.fn(),
    delete: vi.fn(),
  },
}));

vi.mock("../api", () => ({ scriptsApi: scriptsApiMock, tasksApi: tasksApiMock }));
vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: toastOnlyMock }) }));
vi.mock("./useConfirm", () => ({ useConfirm: () => ({ confirm: confirmMock }) }));

const { useScripts } = await import("./useScripts");

/** 已落盘脚本的服务端形态（scriptsApi.get 的返回） */
function serverScript(over: Record<string, unknown> = {}) {
  return {
    id: "checkin",
    name: "签到",
    description: "",
    content: "print(1)",
    binary_path: "/usr/bin/python3",
    ...over,
  };
}

beforeEach(() => {
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  toastOnlyMock.mockReset();
  scriptsApiMock.save.mockClear();
  scriptsApiMock.delete.mockClear();
  scriptsApiMock.get.mockReset();
  tasksApiMock.list.mockClear();
});

afterEach(() => {
  useScripts().clearScriptDraft();
});

describe("useScripts 自动保存", () => {
  it("刚载入的脚本不动它就不发请求", async () => {
    vi.useFakeTimers();
    try {
      scriptsApiMock.get.mockResolvedValue(serverScript());
      const s = useScripts();
      await s.showScriptEditor("checkin");
      expect(s.editingTask.value?.id).toBe("checkin");

      // 无变更：deep watcher 直接判定指纹一致，连 debounce 都不排期
      await vi.advanceTimersByTimeAsync(600);
      expect(scriptsApiMock.save).not.toHaveBeenCalled();
      expect(s.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("内容为空（缺口）时不发请求，补上内容后落盘一次", async () => {
    vi.useFakeTimers();
    try {
      scriptsApiMock.get.mockResolvedValue(serverScript({ content: "" }));
      const s = useScripts();
      await s.showScriptEditor("checkin");

      // 缺口（脚本内容）拦下自动保存：这正是"状态字不能谎称已保存"的场景
      await vi.advanceTimersByTimeAsync(600);
      expect(scriptsApiMock.save).not.toHaveBeenCalled();
      expect(s.editingTask.value?.content).toBe("");
      expect(s.draftGapsNow.value).toContain("脚本内容");

      s.editingTask.value!.content = "print(42)";
      await vi.advanceTimersByTimeAsync(600);
      expect(scriptsApiMock.save).toHaveBeenCalledTimes(1);
      expect(scriptsApiMock.save.mock.calls[0][0]).toBe("checkin");
      expect(s.draftGapsNow.value).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("改动后再改回原样：仍然不发请求（判据是载荷指纹）", async () => {
    vi.useFakeTimers();
    try {
      scriptsApiMock.get.mockResolvedValue(serverScript({ content: "print(1)" }));
      const s = useScripts();
      await s.showScriptEditor("checkin");

      const draft = s.editingTask.value!;
      draft.content = "print(2)";
      await vi.advanceTimersByTimeAsync(100); // 还在 debounce 窗口内
      draft.content = "print(1)"; // 改回原样
      await vi.advanceTimersByTimeAsync(600);

      expect(scriptsApiMock.save).not.toHaveBeenCalled();
      expect(s.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("正常改动落盘后状态字转为已保存", async () => {
    vi.useFakeTimers();
    try {
      scriptsApiMock.get.mockResolvedValue(serverScript());
      const s = useScripts();
      await s.showScriptEditor("checkin");

      s.editingTask.value!.content = "print(99)";
      await vi.advanceTimersByTimeAsync(600);

      expect(scriptsApiMock.save).toHaveBeenCalledTimes(1);
      expect(s.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("删除正在编辑的脚本会关掉编辑器（自动保存不再写回已删除的 id）", async () => {
    vi.useFakeTimers();
    try {
      scriptsApiMock.get.mockResolvedValue(serverScript());
      const s = useScripts();
      await s.showScriptEditor("checkin");
      expect(s.editingTask.value).not.toBeNull();

      await s.deleteScript("checkin");
      expect(scriptsApiMock.delete).toHaveBeenCalledWith("checkin");
      expect(s.editingTask.value).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("用户取消确认时不删除，草稿保留", async () => {
    vi.useFakeTimers();
    try {
      confirmMock.mockResolvedValue(false);
      scriptsApiMock.get.mockResolvedValue(serverScript());
      const s = useScripts();
      await s.showScriptEditor("checkin");

      await s.deleteScript("checkin");
      expect(scriptsApiMock.delete).not.toHaveBeenCalled();
      expect(s.editingTask.value?.id).toBe("checkin");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("useScripts 新建脚本的 ID 确认", () => {
  it("名字打到一半的停顿不会落盘（否则 ID 会被锁成半截名字）", async () => {
    vi.useFakeTimers();
    try {
      const s = useScripts();
      s.createScriptDraft();
      const draft = s.editingTask.value!;
      expect(draft._isNew).toBe(true);

      // 打 "camp"（还没打完 "campus"）后停手一秒：`camp` 已经是合法 ID、stub 也非空，
      // 闸口若只看"当前值合法"就会落盘 —— 而脚本 ID 落盘后不可改（输入框禁用），
      // "campus" 再也打不完，只能删掉重来
      draft.id = "camp";
      await vi.advanceTimersByTimeAsync(1000);
      expect(scriptsApiMock.save, "ID 还在输入中，不该创建文件").not.toHaveBeenCalled();
      expect(draft._isNew).toBe(true);
      expect(s.draftGapsNow.value.join()).toContain("脚本 ID");
    } finally {
      vi.useRealTimers();
    }
  });

  it("回车 / 失焦确认后立即创建，之后改动走同一个 id", async () => {
    vi.useFakeTimers();
    try {
      const s = useScripts();
      s.createScriptDraft();
      const draft = s.editingTask.value!;
      draft.id = "campus";
      draft.name = "校园打卡";

      s.commitScriptId();
      await nextTick();
      await vi.advanceTimersByTimeAsync(0);
      expect(scriptsApiMock.save).toHaveBeenCalledTimes(1);
      expect(scriptsApiMock.save.mock.calls[0][0]).toBe("campus");
      expect(draft._isNew).toBe(false);
      expect(s.draftGapsNow.value).toEqual([]);

      draft.content = "print('hi')";
      await vi.advanceTimersByTimeAsync(600);
      expect(scriptsApiMock.save).toHaveBeenCalledTimes(2);
      expect(scriptsApiMock.save.mock.calls[1][0]).toBe("campus");
    } finally {
      vi.useRealTimers();
    }
  });

  it("确认与「放弃」落在同一轮时不会先创建再删（延后一拍的意义）", async () => {
    vi.useFakeTimers();
    try {
      const s = useScripts();
      s.createScriptDraft();
      s.editingTask.value!.id = "campus";

      // 失焦（确认）与点击「放弃」在同一轮事件里发生：确认必须让放弃先跑完
      s.commitScriptId();
      s.clearScriptDraft();
      await nextTick();
      await vi.advanceTimersByTimeAsync(600);

      expect(scriptsApiMock.save, "已放弃就不该创建").not.toHaveBeenCalled();
      expect(s.editingTask.value).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
