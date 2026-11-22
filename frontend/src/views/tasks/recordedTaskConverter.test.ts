// @vitest-environment node
/**
 * recordedTaskConverter 纯函数测试：四类动作映射、占位符原样透传、navigate 保留
 * （含无 URL 的纯标记形态）、iframe frame 链透传、词表外动作丢弃计数、空序列
 * 拒绝、产物字段极小集。
 */
import { describe, it, expect } from "vitest";
import { convertRecordedActions, type RecordedTaskStep } from "./recordedTaskConverter";
import type { RecordedAction } from "@/api/types";

/** 按录制契约构造动作（与 Worker 返回形状一致：仅带该类型应有的键） */
function makeActions(): RecordedAction[] {
  return [
    { type: "navigate", ts: 1, value: "http://portal.example.com/login" },
    { type: "fill", ts: 2, selector: "#username", strategy: "id", value: "{{USERNAME}}" },
    { type: "fill", ts: 3, selector: "#password", strategy: "id", value: "{{PASSWORD}}" },
    { type: "change", ts: 4, selector: "select[name=isp]", strategy: "name", value: "cmcc" },
    { type: "click", ts: 5, selector: "#login-submit", strategy: "id" },
  ];
}

describe("convertRecordedActions", () => {
  it("四类动作逐字映射：click→click、fill→input、change→select、navigate→navigate", () => {
    const result = convertRecordedActions(makeActions());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.steps.map((s) => s.type)).toEqual(["navigate", "input", "input", "select", "click"]);
    // 保留步骤连续编号（被丢弃的动作不占号）
    expect(result.steps.map((s) => s.id)).toEqual([
      "recorded_1",
      "recorded_2",
      "recorded_3",
      "recorded_4",
      "recorded_5",
    ]);
    expect(result.skippedUnknown).toBe(0);
  });

  it("fill/change 值原样透传：{{USERNAME}} / {{PASSWORD}} 占位符不展开、不改写", () => {
    const result = convertRecordedActions(makeActions());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.steps[1]).toMatchObject({ type: "input", selector: "#username", value: "{{USERNAME}}" });
    expect(result.steps[2]).toMatchObject({ type: "input", selector: "#password", value: "{{PASSWORD}}" });
    expect(result.steps[3]).toMatchObject({ type: "select", selector: "select[name=isp]", value: "cmcc" });
  });

  it("navigate 保留：带 URL 时 value 原样透传；纯标记（无 URL）也保留为步骤", () => {
    const withUrl = convertRecordedActions([{ type: "navigate", ts: 1, value: "http://p.example/success" }]);
    expect(withUrl).toEqual({
      ok: true,
      skippedUnknown: 0,
      steps: [{ id: "recorded_1", type: "navigate", description: "跳转 http://p.example/success", value: "http://p.example/success" }],
    });

    const markerOnly = convertRecordedActions([{ type: "navigate", ts: 1 }]);
    expect(markerOnly).toEqual({
      ok: true,
      skippedUnknown: 0,
      steps: [{ id: "recorded_1", type: "navigate", description: "页面跳转" }],
    });
  });

  it("产物为编辑器种子步骤的最小字段集：不留多余键（后端反序列化补默认值）", () => {
    const result = convertRecordedActions(makeActions());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    const keySets = result.steps.map((s: RecordedTaskStep) => Object.keys(s).sort().join(","));
    expect(keySets).toEqual([
      "description,id,type,value", // navigate（带 URL）
      "description,id,selector,type,value", // fill
      "description,id,selector,type,value", // fill
      "description,id,selector,type,value", // change
      "description,id,selector,type", // click（无 value）
    ]);
  });

  it("iframe 动作 frame 链透传：非顶层录制的 click/input/select 步骤携带 frame，顶层动作不产出该键", () => {
    const result = convertRecordedActions([
      { type: "fill", ts: 1, selector: "#user", strategy: "id", value: "u", frame: "iframe#outer >> iframe#inner" },
      { type: "click", ts: 2, selector: "#go", strategy: "id", frame: "iframe#outer" },
      { type: "fill", ts: 3, selector: "#user", strategy: "id", value: "top" },
    ]);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    // frame 逐字透传（回放侧按 ' >> ' 链逐层下钻）；顶层动作保持最小字段集
    expect(result.steps[0]).toMatchObject({ type: "input", frame: "iframe#outer >> iframe#inner" });
    expect(result.steps[1]).toMatchObject({ type: "click", frame: "iframe#outer" });
    expect(result.steps[2]).not.toHaveProperty("frame");
  });

  it("空序列拒绝：ok=false（后端 validate_task 拒绝空 steps，不产出必失败的任务）", () => {
    expect(convertRecordedActions([])).toEqual({ ok: false, reason: "empty" });
  });

  it("词表外动作丢弃并计数，不中断整段转换；全词表外等价空序列", () => {
    const mixed = convertRecordedActions([
      { type: "hover", ts: 1, selector: "#x", strategy: "id" } as unknown as RecordedAction,
      { type: "click", ts: 2, selector: "#ok", strategy: "id" },
    ]);
    expect(mixed).toEqual({
      ok: true,
      skippedUnknown: 1,
      steps: [{ id: "recorded_1", type: "click", description: "点击 #ok", selector: "#ok" }],
    });

    expect(
      convertRecordedActions([
        { type: "hover", ts: 1, selector: "#x", strategy: "id" } as unknown as RecordedAction,
      ]),
    ).toEqual({ ok: false, reason: "empty" });
  });
});
