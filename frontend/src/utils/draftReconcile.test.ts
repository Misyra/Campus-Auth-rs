/**
 * 草稿—列表对账的单元测试。
 * 核心回归：判据必须是「曾经在列表里、现在不在了」，否则新建草稿会在
 * 首次落盘与列表刷新之间被误判为"已删除"。
 */
import { describe, it, expect } from "vitest";
import { initialReconcileState, reconcileDraft } from "./draftReconcile";

describe("reconcileDraft", () => {
  it("草稿仍在列表里 → 不关闭", () => {
    const r = reconcileDraft({
      state: initialReconcileState(["a", "b"]),
      draftId: "a",
      listIds: ["a", "b"],
    });
    expect(r.gone).toBe(false);
  });

  it("草稿曾在列表里、现在消失 → 关闭", () => {
    const r = reconcileDraft({
      state: initialReconcileState(["a", "b"]),
      draftId: "a",
      listIds: ["b"],
    });
    expect(r.gone).toBe(true);
  });

  it("新建草稿（id 从未在列表里出现过）→ 不关闭", () => {
    // 这正是不能写成"不在列表即关闭"的原因：首次落盘成功后、列表刷新前，
    // 新任务是"已存在但列表还不知道"的状态
    const r = reconcileDraft({
      state: initialReconcileState(["a"]),
      draftId: "brand-new",
      listIds: ["a"],
    });
    expect(r.gone).toBe(false);
  });

  it("无草稿（未在编辑）→ 不关闭", () => {
    const r = reconcileDraft({
      state: initialReconcileState(["a"]),
      draftId: null,
      listIds: [],
    });
    expect(r.gone).toBe(false);
  });

  it("列表为空（加载失败/首次拉取）→ 不关闭", () => {
    const r = reconcileDraft({
      state: initialReconcileState([]),
      draftId: "a",
      listIds: [],
    });
    expect(r.gone).toBe(false);
  });

  it("每轮都会替换基线，消失一次后不会反复触发", () => {
    const first = reconcileDraft({
      state: initialReconcileState(["a", "b"]),
      draftId: "a",
      listIds: ["b"],
    });
    expect(first.gone).toBe(true);

    // 第二轮列表没变：基线已是 ["b"]，a 不在其中 → 不再判定为 gone
    const second = reconcileDraft({ state: first.state, draftId: "a", listIds: ["b"] });
    expect(second.gone).toBe(false);
  });

  it("任务先消失又回来（例如列表抖动）→ 第二轮不误判", () => {
    const step1 = reconcileDraft({
      state: initialReconcileState(["a"]),
      draftId: "a",
      listIds: [],
    });
    expect(step1.gone).toBe(true);
    const step2 = reconcileDraft({ state: step1.state, draftId: "a", listIds: ["a"] });
    expect(step2.gone).toBe(false);
  });
});
