/**
 * 直连任务列表派生逻辑的单元测试（A 版式的左列）。
 *
 * 锁定三类会让用户看错事实的地方：
 * 1. 绑定索引：`profiles` 为空表（尚未拉取）与「所有任务都没人用」是两回事，
 *    前者必须让渲染方整行不显示绑定区，不能渲染成「未绑定」；
 * 2. 行摘要的取值：方法/地址都取任务摘要自带字段（`TaskSummary.url` /
 *    `http_method`），缺数据时宁可不显示 chip/地址，也不能编造一个 GET 让用户
 *    以为任务配错了；
 * 3. 搜索是跨字段 OR、但不得跨字段拼接误命中。
 */
import { describe, expect, it } from "vitest";
import type { ProfileSummary, TaskItem } from "@/api/types";
import {
  buildHttpTaskBindingIndex,
  buildHttpTaskRows,
  filterHttpTaskRows,
  isBindingIndexReady,
} from "./httpTaskList";

function makeProfile(overrides: Partial<ProfileSummary> = {}): ProfileSummary {
  return {
    id: "dorm",
    name: "宿舍移动",
    username: "2024001",
    isp: "",
    active_task: "default",
    active_http_task: "",
    login_channel: "http",
    gateway_ip: "",
    wifi_ssid: "",
    ...overrides,
  };
}

function profileMap(...profiles: ProfileSummary[]): Record<string, ProfileSummary> {
  return Object.fromEntries(profiles.map((p) => [p.id, p]));
}

function makeTask(overrides: Partial<TaskItem> = {}): TaskItem {
  return { id: "dorm", name: "宿舍移动 直连", task_type: "http", ...overrides };
}

describe("buildHttpTaskBindingIndex", () => {
  it("按 active_http_task 反查引用它的方案名", () => {
    const index = buildHttpTaskBindingIndex(
      profileMap(
        makeProfile({ id: "dorm", name: "宿舍移动", active_http_task: "dorm-http" }),
        makeProfile({ id: "teach", name: "教学区网络", active_http_task: "dorm-http" }),
        makeProfile({ id: "lib", name: "图书馆", active_http_task: "" }),
      ),
    );

    // 同一任务被两个方案引用时都要出现，且顺序稳定（对象键序不该影响 pill 顺序）
    expect(index.get("dorm-http")).toEqual(["宿舍移动", "教学区网络"]);
    expect(index.has("")).toBe(false);
    expect(index.size).toBe(1);
  });

  it("未绑定（空串 / 纯空白）不建条目", () => {
    const index = buildHttpTaskBindingIndex(
      profileMap(
        makeProfile({ id: "a", active_http_task: "" }),
        makeProfile({ id: "b", active_http_task: "   " }),
      ),
    );
    expect(index.size).toBe(0);
  });

  it("方案名为空时回退用方案 ID（pill 不能是空胶囊）", () => {
    const index = buildHttpTaskBindingIndex(
      profileMap(makeProfile({ id: "dorm", name: "", active_http_task: "dorm-http" })),
    );
    expect(index.get("dorm-http")).toEqual(["dorm"]);
  });

  it("任务 ID 两端空白按 trim 后的值归并（后端存的是用户选中的 id，不该被空格拆成两个任务）", () => {
    const index = buildHttpTaskBindingIndex(
      profileMap(makeProfile({ id: "a", name: "A", active_http_task: " dorm-http " })),
    );
    expect(index.get("dorm-http")).toEqual(["A"]);
  });
});

describe("isBindingIndexReady", () => {
  it("空表（方案列表尚未拉取）视为不可用", () => {
    expect(isBindingIndexReady({})).toBe(false);
  });

  it("非空表即可用", () => {
    expect(isBindingIndexReady(profileMap(makeProfile()))).toBe(true);
  });
});

describe("buildHttpTaskRows", () => {
  it("方法/地址取任务摘要自带字段，名称与绑定一并带出", () => {
    const rows = buildHttpTaskRows(
      [makeTask({ url: " http://10.0.0.1/login ", http_method: "POST" })],
      new Map([["dorm", ["宿舍移动"]]]),
    );

    expect(rows).toEqual([
      {
        id: "dorm",
        name: "宿舍移动 直连",
        method: "POST",
        url: "http://10.0.0.1/login",
        boundProfiles: ["宿舍移动"],
      },
    ]);
  });

  it("摘要缺方法/地址时为空串（不编造 GET，也不显示空地址）", () => {
    const rows = buildHttpTaskRows([makeTask()], new Map());
    expect(rows[0].method).toBe("");
    expect(rows[0].url).toBe("");
    expect(rows[0].boundProfiles).toEqual([]);
  });

  it("绑定名数组是副本（行内不会因为后续改动索引数组而被改）", () => {
    const names = ["宿舍移动"];
    const rows = buildHttpTaskRows([makeTask()], new Map([["dorm", names]]));
    names.push("教学区网络");
    expect(rows[0].boundProfiles).toEqual(["宿舍移动"]);
  });
});

describe("filterHttpTaskRows", () => {
  const rows = buildHttpTaskRows(
    [
      makeTask({ id: "dorm", name: "宿舍移动 直连", url: "http://10.0.0.1/eportal/portal/login", http_method: "POST" }),
      makeTask({ id: "TeachEportal", name: "教学区 eportal", url: "http://10.20.1.1/eportal/portal/login", http_method: "GET" }),
      makeTask({ id: "library", name: "图书馆 Dr.COM", url: "http://10.30.7.9/drcom/login", http_method: "POST" }),
    ],
    new Map(),
  );

  it("空关键字返回全部（含纯空白）", () => {
    expect(filterHttpTaskRows(rows, "")).toHaveLength(3);
    expect(filterHttpTaskRows(rows, "   ")).toHaveLength(3);
  });

  it("按名称匹配（中文子串）", () => {
    expect(filterHttpTaskRows(rows, "教学区").map((r) => r.id)).toEqual(["TeachEportal"]);
  });

  it("按任务 ID 匹配且大小写不敏感", () => {
    expect(filterHttpTaskRows(rows, "teacheportal").map((r) => r.id)).toEqual(["TeachEportal"]);
  });

  it("按请求地址匹配（改网关 IP 后按 IP 找任务）", () => {
    expect(filterHttpTaskRows(rows, "10.30.7.9").map((r) => r.id)).toEqual(["library"]);
  });

  it("不跨字段拼接误命中", () => {
    // 名称以「宿舍移动」结尾、ID 以 dorm 开头，拼起来不该算一条匹配
    expect(filterHttpTaskRows(rows, "宿舍移动dorm")).toEqual([]);
  });

  it("无匹配时返回空数组", () => {
    expect(filterHttpTaskRows(rows, "不存在的任务")).toEqual([]);
  });
});
