/**
 * 拖拽排序载荷的单元测试。
 *
 * 盯住的是"漏传一组"这个静默故障：后端 `order_tasks` 整体替换 `.order.json`
 * （先 clear 再 extend 三组），少传一组不会报错——那一组顺序被清空、回落成目录
 * 扫描顺序，刷新后才看出来。三个面板共用这一份载荷构造，故在此钉死：
 * 三组都在、组内顺序原样、空组以空数组出现（而不是缺字段）。
 */
import { describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import { orderPayload, useDragSort } from "./drag";
import { tasksApi } from "../api";

// moveBy 的持久化走 tasksApi.order；mock 掉真实 api client，单测不发起任何网络语义
vi.mock("../api", () => ({ tasksApi: { order: vi.fn(async () => {}) } }));

const t = (id: string) => ({ id });

/** 构造被测列表；类型按 useDragSort 签名放宽（moveBy / indexOf 用例只关心 id 序列） */
function setup(ids: string[]) {
  const list = ref(ids.map((id) => ({ id })));
  const drag = useDragSort(
    list as Parameters<typeof useDragSort>[0],
    { tasks: list as Parameters<typeof useDragSort>[1]["tasks"], scripts: ref([]), http: ref([]) } as Parameters<typeof useDragSort>[1],
  );
  return { list, drag };
}

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

describe("moveBy（F11 行内上移/下移）", () => {
  it("下移一格：与拖拽同一份全量列表语义，移动后按三组全量载荷持久化", async () => {
    vi.mocked(tasksApi.order).mockClear();
    const { list, drag } = setup(["a", "b", "c"]);
    await drag.moveBy("a", 1);
    expect(list.value.map((t) => t.id)).toEqual(["b", "a", "c"]);
    expect(vi.mocked(tasksApi.order)).toHaveBeenCalledWith({ all: ["b", "a", "c"], scripts: [], http: [] });
  });

  it("首行上移 / 末行下移 / 未知 id 均为空操作（按钮 disabled 之外的第二道边界）", async () => {
    const { list, drag } = setup(["a", "b"]);
    await drag.moveBy("a", -1);
    await drag.moveBy("b", 1);
    await drag.moveBy("不存在", 1);
    expect(list.value.map((t) => t.id)).toEqual(["a", "b"]);
  });
});

describe("indexOf（行 id → 全量下标换算，三面板共用）", () => {
  it("按 id 返回全量列表下标：模板过滤视图的行号不得直接喂给拖拽 / 上移下移", () => {
    const { drag } = setup(["a", "b", "c"]);
    expect(drag.indexOf("a")).toBe(0);
    expect(drag.indexOf("c")).toBe(2);
  });

  it("未知 id 返回 -1（findIndex 口径，模板 disabled 判断之外的第二道边界）", () => {
    const { drag } = setup(["a"]);
    expect(drag.indexOf("不存在")).toBe(-1);
  });
});
