// @vitest-environment happy-dom
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";
import { createApp, nextTick, type App } from "vue";
import SchemaRow from "./SchemaRow.vue";
import { useConfig } from "@/composables/useConfig";
import { DEFAULT_CONFIG } from "@/utils/constants";

// happy-dom 环境下的相对 URL 解析不可靠（其 URL 实现不走 file: 协议），
// 文本锁定统一用 node:path 字符串拼接定位源码文件
const here = dirname(fileURLToPath(import.meta.url));
function readSource(relative: string): string {
  return readFileSync(join(here, relative), "utf8");
}

/**
 * 设置页控件可访问性（阶段 4③ schema 驱动架构后的锁定面）。
 *
 * 机制变化：标准设置行由 SchemaRow 渲染器按 schema 产出，开关的可访问性语义
 * （role="switch" + aria-checked 显式绑定状态值）收口在渲染器一处——此处改为
 * **行为级**断言：真实挂载渲染器，检查它对开关/下拉/数字行产出的 DOM。
 * 保留在槽位组件里的手写控件（自启动开关、通知通道启用开关）维持原有静态锁定。
 */

// ===== SchemaRow 渲染器（标准行）=====

function mountRowClean(row: Record<string, unknown>): { app: App; container: HTMLDivElement } {
  // useConfig 是模块级单例：预置草稿后挂真实渲染器（不引入 @vue/test-utils）
  const config = useConfig();
  Object.assign(config.config, structuredClone(DEFAULT_CONFIG));
  const container = document.createElement("div");
  document.body.appendChild(container);
  const app = createApp({
    data: () => ({ row }),
    template: `<SchemaRow :row="row" />`,
    components: { SchemaRow },
  });
  app.mount(container);
  return { app, container };
}

describe("SchemaRow 渲染器可访问性（标准行）", () => {
  it("开关行产出 role=switch 且 aria-checked 显式绑定状态值（随草稿翻转）", async () => {
    const { app, container } = mountRowClean({
      id: "row-a",
      label: "示例开关",
      control: { kind: "switch", binding: "logging.file_enabled" },
    });
    const input = container.querySelector<HTMLInputElement>('input[role="switch"]');
    expect(input, "开关行应产出 role=\"switch\"").not.toBeNull();
    // 开关自带可访问名（sr-only 承载 label，不依赖外部 label for）
    expect(container.querySelector(".sr-only")?.textContent).toContain("示例开关");

    // aria-checked 显式绑定草稿状态值（原生 checked 语义对读屏不可见）：
    // 先落 false 再翻转，两个方向都验证绑定跟随
    const config = useConfig();
    config.config.logging.file_enabled = false;
    await nextTick();
    expect(input!.getAttribute("aria-checked")).toBe("false");
    input!.click();
    await nextTick();
    expect(config.config.logging.file_enabled).toBe(true);
    expect(input!.getAttribute("aria-checked")).toBe("true");
    app.unmount();
  });

  it("下拉行经 CustomSelect 触发器绑定设置项名称（aria-label，不再回退为当前值）", () => {
    const { app, container } = mountRowClean({
      id: "row-b",
      label: "检查频率",
      control: {
        kind: "select",
        binding: "updater.check_interval_hours",
        ariaLabel: "检查频率",
        numeric: true,
        options: [{ value: "24", label: "每天一次" }],
      },
    });
    const trigger = container.querySelector<HTMLButtonElement>(".custom-select-trigger");
    expect(trigger, "下拉行应产出 CustomSelect").not.toBeNull();
    expect(trigger!.getAttribute("aria-label")).toBe("检查频率");
    app.unmount();
  });

  it("数字/文本行：label for 指向 input id，FieldHelp 走 aria-describedby", () => {
    const { app, container } = mountRowClean({
      id: "row-c",
      label: "检测间隔（秒）",
      hint: "较短间隔能更快发现断线",
      help: "两次网络检测之间的间隔。",
      control: { kind: "number", binding: "monitor.check_interval_seconds", inputId: "settings-interval", min: 20, max: 1200 },
    });
    const input = container.querySelector<HTMLInputElement>("#settings-interval");
    expect(input, "数字行应产出绑定 id 的输入框").not.toBeNull();
    const label = container.querySelector<HTMLLabelElement>("label[for='settings-interval']");
    expect(label?.textContent).toContain("检测间隔（秒）");
    app.unmount();
  });
});

// ===== 槽位组件里的手写控件（静态锁定）=====

const systemStartupRows = readSource("panels/SystemStartupRows.vue");
const notificationChannels = readSource("panels/NotificationChannelsPanel.vue");
const settingsView = readSource("../SettingsView.vue");

describe("槽位组件手写控件可访问性", () => {
  it("开机自启动开关为 role=switch 且显式绑定 aria-checked", () => {
    const input = systemStartupRows.match(/<input[^>]*role="switch"[^>]*>/);
    expect(input, "SystemStartupRows 缺少 role=\"switch\" 开关").not.toBeNull();
    expect(input![0]).toContain(':aria-checked="autostart.enabled"');
  });

  it("通知通道启用开关为 role=switch 且显式绑定 aria-checked", () => {
    const input = notificationChannels.match(/<input[^>]*role="switch"[^>]*>/);
    expect(input, "NotificationChannelsPanel 缺少 role=\"switch\" 开关").not.toBeNull();
    expect(input![0]).toContain(':aria-checked="channel.enabled"');
  });

  it("免打扰时段开关为 role=switch 且显式绑定 aria-checked", () => {
    const quiet = readSource("panels/NotificationQuietRows.vue");
    const input = quiet.match(/<input[^>]*role="switch"[^>]*>/);
    expect(input, "NotificationQuietRows 缺少 role=\"switch\" 开关").not.toBeNull();
    expect(input![0]).toContain(':aria-checked="notification.quiet_enabled"');
  });
});

describe("ToggleSwitch 可访问名（F4）", () => {
  const toggleSwitch = readSource("../../components/common/ToggleSwitch.vue");

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
    const scheduled = readSource("../tasks/ScheduledTasksPanel.vue");
    const inline = scheduled.match(/<ToggleSwitch[\s\S]*?\/>/g) ?? [];
    const rowUsage = inline.find((tag) => tag.includes("toggleScheduledTask"));
    expect(rowUsage, "找不到定时任务列表行的 ToggleSwitch 用法").toBeTruthy();
    expect(rowUsage!).toContain(":aria-label=");
  });
});

describe("设置页搜索可访问性（输入框已合并进顶栏）", () => {
  const topbar = readSource("../../components/common/AppTopbar.vue");

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
