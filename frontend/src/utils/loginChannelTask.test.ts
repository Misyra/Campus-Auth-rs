/**
 * 任务下拉的选项构造与绑定显示（回归）。
 *
 * 背景：下拉曾有一个空值项「使用内置默认任务」，它指向的其实就是任务列表里的
 * `default`（播种名「通用登录」）——同一件事两个条目，用户无从选择；而 `default`
 * 那条又不带说明，看不出它就是"内置默认"。
 *
 * 直连任务的下拉口径**相反**：它必须有一个显式空值项（没有可内置的兜底任务，
 * 未绑定又会让直连登录直接失败），两者放在同一个文件里一并锁定，避免"顺手把两处
 * 改成一样"式回归。
 *
 * 测试直接调用生产函数（`browserTaskOptions` / `httpTaskOptions` / `taskBindingDisplay`），
 * 与组件共用同一实现，避免"测试自己抄一份逻辑"式的假覆盖。
 */
import { describe, expect, it } from "vitest";
import { browserTaskOptions, httpTaskOptions, taskBindingDisplay } from "./loginChannel";

const DEFAULT_TASK_ID = "default";

describe("browserTaskOptions", () => {
  it("选项只来自任务列表，不再凭空多出空值项", () => {
    const opts = browserTaskOptions(
      [
        { id: "default", name: "通用登录" },
        { id: "dorm", name: "宿舍登录" },
      ],
      DEFAULT_TASK_ID,
    );
    expect(opts).toHaveLength(2);
    // 空值项已移除：它和 default 指向同一任务，并列出现只会让人不知选哪个
    expect(opts.some((o) => o.value === "")).toBe(false);
  });

  it("内置默认任务带「（内置默认）」标注，其余不带", () => {
    const opts = browserTaskOptions(
      [
        { id: "default", name: "通用登录" },
        { id: "dorm", name: "宿舍登录" },
      ],
      DEFAULT_TASK_ID,
    );
    expect(opts[0].label).toBe("通用登录（内置默认）");
    expect(opts[1].label).toBe("宿舍登录");
  });

  it("任务无名称时回退显示 id", () => {
    expect(browserTaskOptions([{ id: "default" }], DEFAULT_TASK_ID)[0].label).toBe(
      "default（内置默认）",
    );
    const noName = browserTaskOptions([{ id: "x" }], DEFAULT_TASK_ID);
    expect(noName[0].label).toBe("x");
    // 非默认任务不得被误加标注
    expect(noName[0].label).not.toContain("内置默认");
  });

  it("任务为空时返回空选项（不注入占位项）", () => {
    expect(browserTaskOptions([], DEFAULT_TASK_ID)).toEqual([]);
  });
});

describe("httpTaskOptions", () => {
  it("首项是空值「未绑定（直连登录不可用）」", () => {
    const opts = httpTaskOptions([{ id: "dorm", name: "宿舍直连" }]);
    expect(opts[0]).toEqual({ value: "", label: "未绑定（直连登录不可用）" });
  });

  it("空值项只出现一次（其余条目都带任务 id）", () => {
    const opts = httpTaskOptions([
      { id: "dorm", name: "宿舍直连" },
      { id: "lab", name: "机房直连" },
    ]);
    expect(opts.filter((o) => o.value === "")).toHaveLength(1);
    expect(opts.slice(1).every((o) => o.value !== "")).toBe(true);
  });

  it("任务顺序保持传入顺序（下拉顺序 = 任务列表顺序，便于按记忆找）", () => {
    const opts = httpTaskOptions([
      { id: "b", name: "第二个" },
      { id: "a", name: "第一个" },
      { id: "c", name: "第三个" },
    ]);
    expect(opts.map((o) => o.value)).toEqual(["", "b", "a", "c"]);
    expect(opts.map((o) => o.label)).toEqual([
      "未绑定（直连登录不可用）",
      "第二个",
      "第一个",
      "第三个",
    ]);
  });

  it("任务无名称时回落显示 id", () => {
    const opts = httpTaskOptions([{ id: "dorm" }]);
    expect(opts[1]).toEqual({ value: "dorm", label: "dorm" });
  });

  it("任务为空时仍保留「未绑定」项（否则用户没法把方案从直连登录上摘下来）", () => {
    expect(httpTaskOptions([])).toEqual([{ value: "", label: "未绑定（直连登录不可用）" }]);
  });
});

describe("taskBindingDisplay", () => {
  it("未绑定显示为内置默认任务", () => {
    expect(taskBindingDisplay("", DEFAULT_TASK_ID)).toBe(DEFAULT_TASK_ID);
  });

  it("已绑定任务照常显示，不被默认值覆盖", () => {
    expect(taskBindingDisplay("dorm", DEFAULT_TASK_ID)).toBe("dorm");
  });

  it("显式绑定 default 与未绑定显示一致（两者对登录等价）", () => {
    expect(taskBindingDisplay(DEFAULT_TASK_ID, DEFAULT_TASK_ID)).toBe(
      taskBindingDisplay("", DEFAULT_TASK_ID),
    );
  });
});
