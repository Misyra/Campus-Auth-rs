/** 自启动诊断文案须区分系统禁用、路径异常、未知与真正关闭。 */
import { describe, expect, it } from "vitest";
import type { AutostartStatus } from "../api/types";
import { autostartDescription } from "./autostart";

const base: AutostartStatus = {
  platform: "windows", enabled: false, method: "Registry", location: "", runtime_mode: "monitor",
};

describe("自启动系统状态", () => {
  it.each([
    ["blocked", "Windows 已禁用"],
    ["path_mismatch", "注册路径"],
    ["command_mismatch", "数据目录"],
    ["unknown", "无法确认"],
    ["not_registered", "未注册"],
    ["enabled", "检查通过"],
  ] as const)("%s 有独立说明", (state, text) => {
    expect(autostartDescription({ ...base, state })).toContain(text);
  });

  it("读取失败优先于过期的成功状态", () => {
    expect(autostartDescription({ ...base, enabled: true, state: "enabled", error: "读取失败" })).toBe("读取失败");
  });
});
