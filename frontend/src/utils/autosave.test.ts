/**
 * 自动保存状态字与共享控制器的单元测试。
 *
 * 状态字盯住的核心只有一条：**缺口未补齐时不许说「已保存」/「改动自动保存」**。
 * 那是修掉的那类静默丢改动问题——状态字说谎时用户不会去数缺口。
 *
 * 控制器（`createAutosaveController`）是两个面板以上共用的一处状态机，四条判据都在
 * 关键路径上，坏掉的表现都是"静默丢改动"或"凭空多写一次"，界面上看不出异常：
 * 1. debounce：改动要等停手才落盘（不是每个按键一发）；
 * 2. 载荷指纹：刚载入 / 新建后没改过 / 改了又改回原样都不发请求；
 * 3. 闸口：发不出去的改动不发请求、不打扰，只在**关闭编辑器**时出声；
 * 4. detached：换编辑对象补发的那一发不得回头改写共享状态（基线被旧草稿覆盖会让
 *    新草稿被误判成"有改动"，凭空写一次刚打开的对象）。
 *
 * 另有三条"同一份草稿有两发请求在飞"的判据（2026-09-24 补，坏掉的后果更隐蔽）：
 * 5. 在途载荷相同就不再发一遍（`flush` 与 debounce 叠在一起时）；
 * 6. 被顶掉的那一发落盘成功也要调 `onSaved`（否则 `_isNew` 永远翻不过来）；
 * 7. 失败出声不受序号影响：detached 那一发被顶掉后失败同样要提示。
 */
import { describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import { AUTOSAVE_DEBOUNCE_MS, autosaveLabel, createAutosaveController, gapBlocker } from "./autosave";

describe("autosaveLabel", () => {
  it("无缺口时按状态机给出四态文案", () => {
    expect(autosaveLabel("idle").text).toBe("改动自动保存");
    expect(autosaveLabel("saving").text).toBe("保存中…");
    expect(autosaveLabel("saved").text).toBe("已保存 · 刚刚");
    expect(autosaveLabel("error").text).toBe("保存失败，修改后自动重试");
  });

  it("缺口非空时改口「暂未保存」，并带缺口数量", () => {
    const label = autosaveLabel("idle", ["请求地址"]);
    expect(label.text).toBe("有 1 处待补全，改动暂未保存");
    expect(label.cls).toBe("tsk-autosave blocked");
    expect(autosaveLabel("saved", ["请求地址", "前置请求的取值方式"]).text).toBe(
      "有 2 处待补全，改动暂未保存",
    );
  });

  it("请求在途时如实说保存中（缺口等落盘后再说）", () => {
    expect(autosaveLabel("saving", ["请求地址"]).text).toBe("保存中…");
  });

  it("状态类始终带 tsk-autosave 基类（面板只绑一个类名）", () => {
    for (const state of ["idle", "saving", "saved", "error"] as const) {
      expect(autosaveLabel(state).cls.startsWith("tsk-autosave")).toBe(true);
    }
  });

  it("新建未落盘时改口「尚未创建」（空闲态才这么说）", () => {
    expect(autosaveLabel("idle", [], true).text).toBe("尚未创建 · 改动后自动保存");
    // 已经存下去过 / 正在存 / 存失败：磁盘上那份的存在性有别的说法，不再提"尚未创建"
    expect(autosaveLabel("saved", [], true).text).toBe("已保存 · 刚刚");
    expect(autosaveLabel("saving", [], true).text).toBe("保存中…");
    expect(autosaveLabel("error", [], true).text).toBe("保存失败，修改后自动重试");
  });

  it("缺口优先于新建态（缺 ID 的新脚本先说要补什么）", () => {
    expect(autosaveLabel("idle", ["脚本 ID"], true).text).toBe("有 1 处待补全，改动暂未保存");
  });
});

/** 测试用草稿：字段少到能一眼看出指纹怎么变 */
interface TestDraft {
  id: string;
  text: string;
  _isNew?: boolean;
}

/**
 * 一个可观察的控制器宿主。
 *
 * `manual` = 落盘挂起不自动完成（由测试自己 resolve / reject）：验证"在途响应后到"与
 * detached 语义这两条，恰恰要靠"请求还没回来"才暴露得出来。
 */
function harness(options: { blocked?: (draft: TestDraft) => boolean; manual?: boolean } = {}) {
  const draft = ref<TestDraft | null>(null);
  const calls: TestDraft[] = [];
  const savedIds: string[] = [];
  const toasts: Array<[boolean, string]> = [];
  const gates: Array<{ resolve: () => void; reject: (error: unknown) => void }> = [];
  const controller = createAutosaveController<TestDraft>({
    draft,
    idOf: (d) => d.id,
    fingerprintOf: (d) => JSON.stringify({ id: d.id, text: d.text }),
    blockReasonOf: (d) => (options.blocked?.(d) ? "改动未保存：还缺 文本" : null),
    persist: (d) => {
      calls.push({ ...d });
      if (!options.manual) return Promise.resolve();
      return new Promise<void>((resolve, reject) => gates.push({ resolve, reject }));
    },
    onSaved: (d) => savedIds.push(d.id),
    toast: (success, message) => toasts.push([success, message]),
    logScope: "test",
    detachedSubject: "上一份",
  });
  /** 打开一份"已落盘"的对象：赋值 + 登记基线（各面板入口的两步） */
  function open(value: TestDraft): TestDraft {
    draft.value = value;
    controller.markBaseline(value);
    return value;
  }
  return { draft, controller, calls, savedIds, toasts, gates, open };
}

describe("createAutosaveController", () => {
  const DEBOUNCE = AUTOSAVE_DEBOUNCE_MS;

  it("改动经 debounce 落盘一次，停手前一个请求都不发", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE - 1);
      expect(h.calls, "debounce 未到点").toHaveLength(0);

      await vi.advanceTimersByTimeAsync(1);
      expect(h.calls).toEqual([{ id: "a", text: "2" }]);
      expect(h.controller.autosaveState.value).toBe("saved");
      expect(h.savedIds).toEqual(["a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("基线未变动就不发请求（刚载入、新建种子都靠这条）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1", _isNew: true });

      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls).toHaveLength(0);
      expect(h.controller.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("改了又改回原样：不发请求（判据是载荷指纹，不是「改过就写」）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE - 100);
      h.draft.value!.text = "1";
      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);

      expect(h.calls).toHaveLength(0);
      expect(h.controller.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("闸口拦下的改动不发请求也不出声；关闭编辑器时才出声", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ blocked: (d) => d.text === "" });
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "";
      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls, "发出去必被后端拒").toHaveLength(0);
      expect(h.toasts, "用户正在逐项填写，别拿 toast 打断").toHaveLength(0);
      expect(h.controller.autosaveState.value).toBe("idle");

      // 换编辑对象：用户只是在列表里点另一条，不该被弹一句
      await h.controller.flush("switch");
      expect(h.toasts).toHaveLength(0);

      // 关闭编辑器：明说，否则「退出即生效」在缺口态下变成静默丢弃
      await h.controller.flush("close");
      expect(h.toasts).toEqual([[false, "改动未保存：还缺 文本"]]);
      expect(h.calls).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("关闭编辑器会补发在途改动（「退出即生效」）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "2"; // 不等 debounce 到点就关
      await h.controller.flush("close");
      expect(h.calls).toEqual([{ id: "a", text: "2" }]);
      expect(h.controller.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("detached：换编辑对象补发的那一发不接管状态字、不登记基线", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });
      h.draft.value!.text = "2";

      const sent = h.controller.flush("switch");
      // 请求还没回来，用户已经切到另一份草稿：这一发不得回头改写它的基线
      h.open({ id: "b", text: "same" });
      h.gates[0].resolve();
      await sent;

      expect(h.controller.autosaveState.value, "detached 不该把状态字改成 saved").toBe("idle");
      expect(h.savedIds, "detached 不写界面态（_isNew 等）").toEqual([]);

      // 新草稿改一下再改回去（净零）：基线若被上一份草稿覆盖，这里会凭空写出 "b"
      h.draft.value!.text = "changed";
      await vi.advanceTimersByTimeAsync(DEBOUNCE - 100);
      h.draft.value!.text = "same";
      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls.map((c) => c.id)).toEqual(["a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("慢响应后到：旧一发不接管状态字，但它的成功仍要认下「这份草稿已落盘」", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);
      expect(h.controller.autosaveState.value, "请求在途").toBe("saving");

      h.draft.value!.text = "3";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);

      h.gates[0].resolve(); // 旧的那一发先回来
      await vi.advanceTimersByTimeAsync(0);
      expect(h.controller.autosaveState.value, "旧响应不得改写状态字").toBe("saving");
      // 但它确实写成功了：`_isNew` 必须翻（否则定时任务之后每次自动保存都 POST 一个
      // 已存在的 id → 409），只是基线/状态字留给接管的那一发
      expect(h.savedIds, "被顶掉的成功也要认").toEqual(["a"]);

      h.gates[1].resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.controller.autosaveState.value).toBe("saved");
      expect(h.savedIds).toEqual(["a", "a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("在途载荷相同就不再发一遍（flush 不重复落盘）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });
      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);
      expect(h.calls).toHaveLength(1);

      // debounce 到点会清掉 `pendingChanges`，而基线要等响应才更新——"在途"这段窗口里
      // 旧判据看不出有一发在路上，于是换编辑对象时会再发一次一模一样的内容。
      // 对 PUT 面板是白跑，对定时任务则是第二次 POST 一个刚建出来的 id（409 假报错）。
      await h.controller.flush("switch");
      expect(h.calls).toHaveLength(1);

      h.gates[0].resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.controller.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("clear() 之后迟到的响应不得写状态字，也不得顶掉新草稿的基线", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });
      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);
      expect(h.controller.autosaveState.value).toBe("saving");

      h.controller.clear(); // 删除 / 放弃新建：草稿已置空
      h.gates[0].resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.controller.autosaveState.value, "已结束编辑，迟到响应不该写状态字").toBe("idle");
      expect(h.savedIds, "草稿已不在编辑器里，界面态不再属于它").toEqual([]);

      // 新草稿改一下再改回去（净零）：基线若被上一份草稿的响应覆盖，这里会凭空写一次
      h.open({ id: "b", text: "same" });
      h.draft.value!.text = "changed";
      await vi.advanceTimersByTimeAsync(DEBOUNCE - 100);
      h.draft.value!.text = "same";
      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls.map((c) => c.id)).toEqual(["a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("落盘失败：状态转 error 并出声，不误记已保存", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "2";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);

      h.gates[0].reject(new Error("boom"));
      await vi.advanceTimersByTimeAsync(0);
      expect(h.controller.autosaveState.value).toBe("error");
      expect(h.toasts).toEqual([[false, "boom"]]);
      expect(h.savedIds).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("detached 那一发失败也照样出声（静默等于偷偷丢掉）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });
      h.draft.value!.text = "2";

      const sent = h.controller.flush("switch");
      h.gates[0].reject(new Error("boom"));
      await sent;

      expect(h.toasts).toEqual([[false, "上一份的改动未保存：boom"]]);
      // 不改状态字：那份草稿已经不归这块界面管了
      expect(h.controller.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("detached 那一发被后一发顶掉之后失败，同样要出声", async () => {
    vi.useFakeTimers();
    try {
      const h = harness({ manual: true });
      h.open({ id: "a", text: "1" });
      h.draft.value!.text = "2";
      const sent = h.controller.flush("switch"); // 换编辑对象补发的那一发

      // 新草稿的变更紧接着又发出去一发：序号前进，旧那一发"已被顶掉"
      h.open({ id: "b", text: "x" });
      h.draft.value!.text = "y";
      await vi.advanceTimersByTimeAsync(DEBOUNCE);
      expect(h.calls.map((c) => c.id)).toEqual(["a", "b"]);

      h.gates[0].reject(new Error("boom"));
      await sent;
      // 失败提示必须在序号判断之前发出：序号被顶掉只说明"界面态不归它管"，
      // 不代表那一笔改动不需要告诉用户（提示与日志都照发）
      expect(h.toasts, "被顶掉的失败不得静默").toEqual([
        [false, "上一份的改动未保存：boom"],
      ]);

      h.gates[1].resolve();
      await vi.advanceTimersByTimeAsync(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("saveNow：程序化改动不走 debounce，且撤掉在途定时器（不写两次）", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1" });

      h.draft.value!.text = "templated";
      await h.controller.saveNow(h.draft.value!);
      expect(h.calls).toEqual([{ id: "a", text: "templated" }]);

      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls, "在途 debounce 应已被撤掉").toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("markUnsaved：登记「磁盘上还没有」后，这一次内容必定落盘", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      // 导入覆盖的场景：内容与磁盘不同，基线留空才会被排期
      h.draft.value = { id: "a", text: "imported" };
      h.controller.markUnsaved();

      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls).toEqual([{ id: "a", text: "imported" }]);
      expect(h.controller.autosaveState.value).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("clear()：复位状态机、关掉草稿，迟到的那一轮 watcher 不得再排期落盘", async () => {
    vi.useFakeTimers();
    try {
      const h = harness();
      h.open({ id: "a", text: "1" });

      // 改完立刻 clear（删除 / 放弃新建正是这条路径）：赋值同步生效，但深度 watcher 的
      // 回调还在队列里——只复位状态机而不关草稿，那一轮会读到残留草稿再写一次
      h.draft.value!.text = "2";
      h.controller.clear();
      expect(h.draft.value).toBeNull();

      await vi.advanceTimersByTimeAsync(DEBOUNCE * 2);
      expect(h.calls, "已结束编辑的内容不该被写回").toHaveLength(0);
      expect(h.controller.autosaveState.value).toBe("idle");
    } finally {
      vi.useRealTimers();
    }
  });

  it("gapBlocker：缺口为空才放行，非空时带全部缺口名", () => {
    const block = gapBlocker<{ gaps: string[] }>((d) => d.gaps);
    expect(block({ gaps: [] })).toBeNull();
    expect(block({ gaps: ["脚本 ID", "脚本内容"] })).toBe(
      "改动未保存：还缺 脚本 ID、脚本内容",
    );
  });
});
