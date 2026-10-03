import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  vi.unstubAllEnvs();
  vi.resetModules();
});

describe("OpenWrt 功能入口", () => {
  it("只保留 HTTP、脚本与定时任务，移除浏览器和环境安装设置", async () => {
    vi.stubEnv("VITE_OPENWRT", "true");
    vi.resetModules();
    const nav = await import("./navTree");
    expect(nav.TASK_NAV_CHILDREN.map((child) => child.id)).toEqual(["http", "scripts", "scheduled"]);
    expect(nav.SETTINGS_NAV_CHILDREN.map((child) => child.id)).toEqual(["monitor", "system", "appearance"]);
  });

  it("桌面构建继续提供浏览器与环境设置", async () => {
    vi.stubEnv("VITE_OPENWRT", "false");
    vi.resetModules();
    const nav = await import("./navTree");
    expect(nav.TASK_NAV_CHILDREN.map((child) => child.id)).toContain("browser");
    expect(nav.TASK_NAV_CHILDREN.map((child) => child.id)).toContain("ai");
    expect(nav.SETTINGS_NAV_CHILDREN.map((child) => child.id)).toContain("browser");
    expect(nav.SETTINGS_NAV_CHILDREN.map((child) => child.id)).toContain("tasks");
  });
});
