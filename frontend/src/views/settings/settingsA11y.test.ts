import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * 系统设置页控件可访问性锁定（docs/plan-next.md 2026-09-24 登记：8 个控件不在
 * 可访问性树里）。逐项补齐后用静态断言锁定，防止回退：
 *
 * - 开关（开机自启动 / 启用文件日志 / 启动时打开控制台 / 任务通知 / 显示系统托盘
 *   图标）：原生 checkbox 被 .toggle 样式视觉隐藏，补 role="switch" + aria-checked
 *   （原生 checked 状态不随 role 改变语义，显式绑定状态值读屏才可读）；
 * - 下拉（启动后执行 / 服务模式 / 全局日志级别）：CustomSelect 触发器是 combobox，
 *   可访问名缺省回退为「当前值」，补 aria-label 绑定设置项名称；
 * - 配置预设（原「运行模式」分区的分段按钮组）：按钮已带 aria-pressed，补
 *   role="group" + aria-label 让整组有可访问名。
 */

const systemSettings = readFileSync(new URL("./SystemSettings.vue", import.meta.url), "utf8");
const settingsView = readFileSync(new URL("../SettingsView.vue", import.meta.url), "utf8");

describe("系统设置页控件可访问性", () => {
  it("五个开关均为 role=switch 且显式绑定 aria-checked", () => {
    const switchInputs = systemSettings.match(/<input[^>]*role="switch"[^>]*>/g) ?? [];
    expect(switchInputs, "role=\"switch\" 的开关应为 5 个").toHaveLength(5);
    for (const state of [
      "autostart.enabled",
      "config.config.app_settings.auto_start_browser",
      "config.config.app_settings.task_notification",
      "config.config.app_settings.show_tray",
      "config.config.logging.file_enabled",
    ]) {
      const input = switchInputs.find((tag) => tag.includes(`:aria-checked="${state}"`));
      expect(input, `开关缺少 :aria-checked="${state}" 绑定`).toBeTruthy();
    }
  });

  it("三个下拉的 CustomSelect 触发器绑定设置项名称（不再回退为当前值）", () => {
    for (const label of ["启动后执行", "服务模式", "全局日志级别"]) {
      const usage = systemSettings.match(new RegExp(`<CustomSelect[^>]*aria-label="${label}"[^>]*>`));
      expect(usage, `CustomSelect 缺少 aria-label="${label}"`).not.toBeNull();
    }
  });

  it("配置预设分段按钮组有组语义与可访问名", () => {
    const group = systemSettings.match(/<div class="run-mode-options"[^>]*>/);
    expect(group, "run-mode-options 缺少组语义").not.toBeNull();
    expect(group![0]).toContain('role="group"');
    expect(group![0]).toContain('aria-label="配置预设"');
  });
});

describe("ToggleSwitch 可访问名（F4）", () => {
  const toggleSwitch = readFileSync(new URL("../../components/common/ToggleSwitch.vue", import.meta.url), "utf8");

  it("role=switch 的可访问名落在按钮上：有 label 走 aria-labelledby，无 label 透传 aria-label", () => {
    const button = toggleSwitch.match(/<button[\s\S]*?role="switch"[\s\S]*?<\/button>/);
    expect(button, "ToggleSwitch.vue 缺少 role=\"switch\" 按钮").not.toBeNull();
    expect(button![0]).toContain(":aria-labelledby");
    expect(button![0]).toContain(":aria-label");
    // label span 必须挂 id，aria-labelledby 才有指向目标
    expect(toggleSwitch).toContain(':id="labelId"');
    // useId 保证同页多实例 id 唯一
    expect(toggleSwitch).toContain("useId()");
  });

  it("行内用法（无 label）必须显式传 aria-label（定时任务列表启停列）", () => {
    const scheduled = readFileSync(new URL("../tasks/ScheduledTasksPanel.vue", import.meta.url), "utf8");
    const inline = scheduled.match(/<ToggleSwitch[\s\S]*?\/>/g) ?? [];
    const rowUsage = inline.find((tag) => tag.includes("toggleScheduledTask"));
    expect(rowUsage, "找不到定时任务列表行的 ToggleSwitch 用法").toBeTruthy();
    expect(rowUsage!).toContain(":aria-label=");
  });
});

describe("设置页搜索可访问性（输入框已合并进顶栏）", () => {
  const topbar = readFileSync(new URL("../../components/common/AppTopbar.vue", import.meta.url), "utf8");

  it("设置路由下顶栏输入框带 aria-label，不能只靠 placeholder", () => {
    const input = topbar.match(/<input[^>]*class="topbar-search-input"[^>]*>/);
    expect(input, "AppTopbar.vue 缺少 topbar-search-input 输入框").not.toBeNull();
    expect(input![0]).toContain('aria-label="搜索设置"');
    expect(topbar).toContain("isSettingsRoute");
  });

  it("清空按钮可达：type=button + aria-label（有查询词时才渲染）", () => {
    const clear = topbar.match(/<button[^>]*class="topbar-search-clear"[^>]*>/);
    expect(clear, "AppTopbar.vue 缺少搜索清空按钮").not.toBeNull();
    expect(clear![0]).toContain('type="button"');
    expect(clear![0]).toContain('aria-label="清空搜索"');
  });

  it("SettingsView 不再自绘第二个搜索框（入口唯一）", () => {
    expect(settingsView).not.toContain("settings-search-input");
    expect(settingsView).not.toContain("settings-search-clear");
  });
});
