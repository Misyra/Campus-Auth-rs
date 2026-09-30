/** 外观存储受限或损坏时继续显示界面，不让偏好阻断启动。 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { loadStored, saveStored } from "./storage";

afterEach(() => vi.unstubAllGlobals());
describe("本地显示偏好存储", () => {
  it("无存储权限时读取回落且保存返回失败", () => {
    vi.stubGlobal("localStorage", { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); }, removeItem: () => { throw new Error("denied"); } });
    expect(loadStored("appearance", { theme: "light" })).toEqual({ theme: "light" });
    expect(saveStored("appearance", {})).toBe(false);
  });
  it("坏 JSON 即使不能清理也不会打断加载", () => {
    vi.stubGlobal("localStorage", { getItem: () => "{broken", removeItem: () => { throw new Error("denied"); } });
    expect(loadStored("appearance", {})).toEqual({});
  });
  it.each(["[]", "null", '"text"', "123"])("拒绝错误的配置顶层类型 %s", (saved) => {
    vi.stubGlobal("localStorage", { getItem: () => saved });
    expect(loadStored("appearance", { theme: "light" })).toEqual({ theme: "light" });
  });
  it("读取旧配置保留默认新字段", () => {
    const setItem = vi.fn();
    vi.stubGlobal("localStorage", { getItem: () => '{"theme":"dark"}', setItem });
    expect(loadStored("appearance", { theme: "light", font_scale: 1 })).toEqual({ theme: "dark", font_scale: 1 });
    expect(saveStored("appearance", { theme: "dark" })).toBe(true);
    expect(setItem).toHaveBeenCalledWith("appearance", '{"theme":"dark"}');
  });
});
