/**
 * 侧栏二级导航数据与激活判定的单元测试。
 *
 * 这五个子项是「任务」分组在侧栏与窄屏 pill 行两处的唯一事实源，
 * 路由名写错或 id 重复会让某一项永远激活不了、或两项同时点亮；
 * 而路由名在 AppSidebar、TasksView、editorGuard 三处被引用，属高风险字段。
 */
import { describe, expect, it } from "vitest";
import { activeChildId, TASK_NAV_CHILDREN } from "./navTree";

describe("TASK_NAV_CHILDREN 数据完整性", () => {
  it("id 与路由名各自唯一", () => {
    // 重复 id 会让 activeChildId 只命中第一个，另一个子项永远不亮
    const ids = TASK_NAV_CHILDREN.map((child) => child.id);
    const names = TASK_NAV_CHILDREN.map((child) => child.name);
    expect(new Set(ids).size).toBe(ids.length);
    expect(new Set(names).size).toBe(names.length);
  });

  it("路由名均为 tasks- 前缀且标签/说明非空", () => {
    for (const child of TASK_NAV_CHILDREN) {
      // 前缀是 AppSidebar 的 `startsWith("tasks")` 高亮判定与 router 的
      // editorGuard `/tasks` 区域判定的前提
      expect(child.name.startsWith("tasks-")).toBe(true);
      expect(child.label.length).toBeGreaterThan(0);
      // 全称说明靠 title 承载（侧栏放不下全称），空 title 等于信息丢失
      expect(child.title.length).toBeGreaterThan(0);
    }
  });

  it("首项为浏览器任务：/tasks 的 redirect 落在它身上", () => {
    expect(TASK_NAV_CHILDREN[0].name).toBe("tasks-browser");
  });
});

describe("activeChildId", () => {
  it("按路由名精确命中", () => {
    expect(activeChildId(TASK_NAV_CHILDREN, "tasks-http")).toBe("http");
    expect(activeChildId(TASK_NAV_CHILDREN, "tasks-ai")).toBe("ai");
  });

  it("/tasks 落地经 redirect 后是 tasks-browser，命中浏览器任务", () => {
    // 直接匹配父级路由名 `tasks` 会落空，导航看起来像"一个都没选中"
    expect(activeChildId(TASK_NAV_CHILDREN, "tasks-browser")).toBe("browser");
  });

  it("不属于本分组时返回 null（父级/其他页面/缺失）", () => {
    expect(activeChildId(TASK_NAV_CHILDREN, "tasks")).toBeNull();
    expect(activeChildId(TASK_NAV_CHILDREN, "settings-browser")).toBeNull();
    expect(activeChildId(TASK_NAV_CHILDREN, null)).toBeNull();
    expect(activeChildId(TASK_NAV_CHILDREN, undefined)).toBeNull();
    expect(activeChildId(TASK_NAV_CHILDREN, "")).toBeNull();
  });
});
