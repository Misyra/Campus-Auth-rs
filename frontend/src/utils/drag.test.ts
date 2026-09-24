/**
 * 拖拽排序载荷的单元测试。
 *
 * 盯住的是"漏传一组"这个静默故障：后端 `order_tasks` 整体替换 `.order.json`
 * （先 clear 再 extend 三组），少传一组不会报错——那一组顺序被清空、回落成目录
 * 扫描顺序，刷新后才看出来。三个面板共用这一份载荷构造，故在此钉死：
 * 三组都在、组内顺序原样、空组以空数组出现（而不是缺字段）。
 */
import { describe, expect, it } from "vitest";
import { orderPayload } from "./drag";

const t = (id: string) => ({ id });

describe("orderPayload", () => {
  it("三组全量带上，组内顺序原样", () => {
    expect(
      orderPayload([t("default"), t("dorm")], [t("checkin")], [t("dorm_http"), t("lib_http")]),
    ).toEqual({
      all: ["default", "dorm"],
      scripts: ["checkin"],
      http: ["dorm_http", "lib_http"],
    });
  });

  it("空组也以空数组出现（缺字段走 serde 默认值，语义上是「这类任务不存在」，不是「这类任务没顺序」）", () => {
    const payload = orderPayload([], [], []);
    expect(payload).toEqual({ all: [], scripts: [], http: [] });
    expect(Object.keys(payload).sort()).toEqual(["all", "http", "scripts"]);
  });

  it("只取 id，不把整条任务对象发出去（载荷要能被后端 OrderBody 反序列化）", () => {
    const payload = orderPayload([{ id: "a", name: "任务 A", url: "http://x" } as { id: string }], [], []);
    expect(payload.all).toEqual(["a"]);
  });
});
