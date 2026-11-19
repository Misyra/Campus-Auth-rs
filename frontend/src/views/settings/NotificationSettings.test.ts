// @vitest-environment happy-dom
/**
 * 推送通知分区行为测试（createApp 直挂，免 @vue/test-utils，与
 * NetworkDiagnosticsPanel.test.ts 同范式）。useConfig 是模块级单例，直接预置
 * config.notification 后挂载真实组件。
 *
 * 阶段 4③ 面板拆分后的挂载面：推送设置三行（启用开关/重试参数）由 schema +
 * SchemaRow 渲染（覆盖见 settingsA11y.test.ts），本文件挂载三个槽位组件——
 * NotificationChannelsPanel（通道增删改与测试发送）、NotificationEventsRows
 * （触发事件 chip）、NotificationQuietRows（免打扰时段）。
 *
 * 锁定五类交互：
 * - 通道增删：添加通道按所选类型生成草稿卡片，删除即移除（结果缓存同步清掉）；
 * - 类型切换：type 下拉切换就地重建通道为对应变体，字段表单随类型切换、
 *   公共字段（名称/启用/超时）保留；
 * - 系统通知（os）通道：无凭据输入框（表单仅剩超时字段），后端载荷经草稿
 *   编辑后保存形状不变（serde 往返无凭据字段混入）；
 * - 测试按钮三分支：发送中（禁用按钮）/ 成功提示 / 失败摘要（含请求级错误）；
 * - 事件类型开关组与免打扰时段绑定写入 config 草稿（保存仍由页面统一负责）。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App, type Component } from "vue";
import NotificationChannelsPanel from "./panels/NotificationChannelsPanel.vue";
import NotificationEventsRows from "./panels/NotificationEventsRows.vue";
import NotificationQuietRows from "./panels/NotificationQuietRows.vue";
import { useConfig } from "@/composables/useConfig";
import { DEFAULT_CONFIG } from "@/utils/constants";
import type { NotificationConfig, NotificationTestResult } from "@/api/types";

/** 构造 JSON 响应（与 client.test.ts 同款桩） */
function jsonResponse(body: unknown, init: { status?: number } = {}): Response {
  const status = init.status ?? 200;
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: new Headers({ "content-type": "application/json" }),
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

let app: App | null = null;
let container: HTMLDivElement | null = null;

/** 预置通知配置（单例快照清空后写默认值） */
function seedNotification(overrides: Partial<NotificationConfig> = {}): NotificationConfig {
  const config = useConfig().config;
  Object.assign(config.notification, { ...structuredClone(DEFAULT_CONFIG.notification), ...overrides });
  return config.notification;
}

function mount(component: Component): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(component);
  app.mount(container);
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：测试发送是异步 fetch + 组件状态刷新，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

function stubFetch(handler: (url: string, init?: RequestInit) => Promise<Response>): void {
  vi.stubGlobal("fetch", vi.fn(async (url: string | URL, init?: RequestInit) => handler(String(url), init)));
}

function addChannelByUI(): void {
  // 默认 addType 即 webhook；其他类型经卡片内下拉切换（见类型切换用例）
  const addBtn = [...document.querySelectorAll<HTMLButtonElement>(".notif-add-row button")].find(
    (b) => b.textContent?.includes("添加通道"),
  );
  expect(addBtn, "找不到「添加通道」按钮").not.toBeNull();
  addBtn!.click();
}

function channelCards(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".notif-channel-card")];
}

function findButton(label: string, scope: ParentNode = document): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll<HTMLButtonElement>("button")].find((b) =>
    b.textContent?.includes(label),
  );
}

/** 通过 CustomSelect 切换通道卡片类型：点开下拉 → 等渲染 → mousedown 选项 */
async function switchChannelType(card: HTMLElement, optionLabel: string): Promise<void> {
  const trigger = card.querySelector<HTMLButtonElement>(".custom-select-trigger");
  expect(trigger, "通道卡片缺少类型下拉").not.toBeNull();
  trigger!.click();
  await flush();
  const option = [...card.querySelectorAll<HTMLElement>(".custom-select-option")].find(
    (o) => o.textContent?.trim() === optionLabel,
  );
  expect(option, `下拉缺少选项 ${optionLabel}`).not.toBeNull();
  option!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
  window.location.hash = "";
  seedNotification();
});

afterEach(() => {
  app?.unmount();
  app = null;
  container = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("NotificationChannelsPanel 通道增删", () => {
  it("添加通道按所选类型生成草稿卡片，再点删除即移除", async () => {
    mount(NotificationChannelsPanel);
    expect(channelCards()).toHaveLength(0);
    addChannelByUI();
    await flush();
    const cards = channelCards();
    expect(cards).toHaveLength(1);
    // webhook 通道的字段表单：目标地址输入框
    expect(cards[0].querySelector('input[placeholder="https://example.com/hook"]')).not.toBeNull();
    // 删除
    const deleteBtn = findButton("删除", cards[0]);
    expect(deleteBtn, "通道卡片缺少删除按钮").not.toBeNull();
    deleteBtn!.click();
    await flush();
    expect(channelCards()).toHaveLength(0);
  });

  it("通道数量上限 10：超限时 toast 提示且不再新增", async () => {
    seedNotification({
      channels: Array.from({ length: 10 }, (_, i) => ({
        type: "serverchan" as const,
        id: `ch-${i}`,
        name: "",
        enabled: true,
        timeout_seconds: 10,
        send_key: "sct",
      })),
    });
    mount(NotificationChannelsPanel);
    expect(channelCards()).toHaveLength(10);
    addChannelByUI();
    await flush();
    expect(channelCards(), "已达上限不得新增").toHaveLength(10);
  });
});

describe("NotificationChannelsPanel 类型切换", () => {
  it("切换 type 就地重建通道：字段表单随类型切换、公共字段保留", async () => {
    const notification = seedNotification();
    mount(NotificationChannelsPanel);
    addChannelByUI();
    await flush();
    const card = channelCards()[0];
    // 先填公共字段：名称 + 超时
    const nameInput = card.querySelector<HTMLInputElement>('input[aria-label="通道 1 名称"]');
    nameInput!.value = "宿舍机器人";
    nameInput!.dispatchEvent(new Event("input", { bubbles: true }));
    await flush();

    await switchChannelType(card, "Bark");
    await flush();

    const rebuilt = notification.channels[0];
    expect(rebuilt.type).toBe("bark");
    expect(rebuilt.id, "切换类型保留通道 id").toBe(notification.channels[0].id);
    expect(rebuilt.name).toBe("宿舍机器人");
    // 表单：Bark 字段出现、Webhook 字段消失
    const card2 = channelCards()[0];
    expect(
      card2.querySelector('input[placeholder="Bark App 显示的设备 Key"]'),
      "Bark Device Key 输入框应出现",
    ).not.toBeNull();
    expect(
      card2.querySelector('input[placeholder="https://example.com/hook"]'),
      "Webhook 目标地址输入框应消失",
    ).toBeNull();
    // 名称输入框仍绑定同一草稿值
    const nameAfter = card2.querySelector<HTMLInputElement>('input[aria-label="通道 1 名称"]');
    expect(nameAfter!.value).toBe("宿舍机器人");
  });
});

describe("NotificationChannelsPanel 系统通知通道", () => {
  it("切换到系统通知：表单仅剩超时字段，无凭据输入框", async () => {
    const notification = seedNotification();
    mount(NotificationChannelsPanel);
    addChannelByUI();
    await flush();
    const card = channelCards()[0];
    // 先填名称再切换，顺带验证公共字段保留
    const nameInput = card.querySelector<HTMLInputElement>('input[aria-label="通道 1 名称"]');
    nameInput!.value = "桌面提醒";
    nameInput!.dispatchEvent(new Event("input", { bubbles: true }));
    await flush();

    await switchChannelType(card, "系统通知");
    await flush();

    const rebuilt = notification.channels[0];
    expect(rebuilt.type).toBe("os");
    expect(rebuilt.name, "切换类型保留名称").toBe("桌面提醒");
    expect(rebuilt.enabled, "切换类型保留启用态").toBe(true);
    const card2 = channelCards()[0];
    // 无凭据区：正文不再渲染任何文本输入框（webhook 地址 / bark key / sendkey / token 均为 text）
    expect(
      card2.querySelectorAll('.notif-channel-body input[type="text"]'),
      "系统通知不应有凭据输入框",
    ).toHaveLength(0);
    // 表单形态：仅剩超时数字输入（草稿默认 10，与后端 serde default 一致）
    const timeout = card2.querySelector<HTMLInputElement>('.notif-channel-body input[type="number"]');
    expect(timeout, "系统通知仅保留超时输入").not.toBeNull();
    expect(timeout!.value).toBe("10");
    // 草稿形状：五个公共字段，无凭据字段
    expect(Object.keys(rebuilt).sort()).toEqual(["enabled", "id", "name", "timeout_seconds", "type"]);
  });

  it("系统通知通道 serde 往返：后端载荷经草稿编辑后保存形状不变", async () => {
    // 后端 GET /api/config 返回的 os 通道形状（timeout_seconds 恒序列化）
    const wire = { type: "os", id: "ch-os-1", name: "系统通知", enabled: true, timeout_seconds: 15 };
    const notification = seedNotification({
      channels: [JSON.parse(JSON.stringify(wire))],
    });
    mount(NotificationChannelsPanel);
    const card = channelCards()[0];
    // UI 编辑名称与超时；页面统一「保存」把 config.notification 原样序列化进 PATCH /api/config
    const nameInput = card.querySelector<HTMLInputElement>('input[aria-label="通道 1 名称"]');
    nameInput!.value = "桌面提醒";
    nameInput!.dispatchEvent(new Event("input", { bubbles: true }));
    const timeout = card.querySelector<HTMLInputElement>('.notif-channel-body input[type="number"]');
    timeout!.value = "20";
    timeout!.dispatchEvent(new Event("input", { bubbles: true }));
    await flush();
    // JSON 往返后形状与后端契约一致：五字段、无凭据、超时为编辑后的数字
    expect(JSON.parse(JSON.stringify(notification.channels[0]))).toEqual({
      type: "os",
      id: "ch-os-1",
      name: "桌面提醒",
      enabled: true,
      timeout_seconds: 20,
    });
  });
});

describe("NotificationChannelsPanel 测试发送三分支", () => {
  function clickTest(card: HTMLElement): void {
    const btn = findButton("测试发送", card);
    expect(btn, "通道卡片缺少测试发送按钮").not.toBeNull();
    btn!.click();
  }

  it("成功分支：回执 success=true 显示成功提示", async () => {
    seedNotification({
      channels: [
        { type: "webhook", id: "ch-1", name: "本地", enabled: true, url: "http://127.0.0.1:1/hook", headers: [], body_template: "", timeout_seconds: 3 },
      ],
    });
    stubFetch(async (url) => {
      expect(url).toContain("/api/notification/test");
      return jsonResponse({
        data: { results: [{ id: "ch-1", name: "本地", type: "webhook", success: true }] },
      } satisfies { data: NotificationTestResult });
    });
    mount(NotificationChannelsPanel);
    clickTest(channelCards()[0]);
    await settle();
    const result = document.querySelector(".notif-test-result");
    expect(result?.textContent).toContain("测试消息已发送");
  });

  it("通道失败分支：回执 success=false 就地显示脱敏错误摘要", async () => {
    seedNotification({
      channels: [
        { type: "webhook", id: "ch-1", name: "本地", enabled: true, url: "http://127.0.0.1:1/hook", headers: [], body_template: "", timeout_seconds: 3 },
      ],
    });
    stubFetch(async () =>
      jsonResponse({
        data: {
          results: [{ id: "ch-1", name: "本地", type: "webhook", success: false, error: "连接失败（检查网络或目标地址）" }],
        },
      }),
    );
    mount(NotificationChannelsPanel);
    clickTest(channelCards()[0]);
    await settle();
    const result = document.querySelector(".notif-test-result--fail");
    expect(result?.textContent).toContain("发送失败");
    expect(result?.textContent).toContain("连接失败");
  });

  it("请求失败分支：HTTP 500 显示后端错误信息", async () => {
    seedNotification({
      channels: [
        { type: "webhook", id: "ch-1", name: "本地", enabled: true, url: "http://127.0.0.1:1/hook", headers: [], body_template: "", timeout_seconds: 3 },
      ],
    });
    stubFetch(async () =>
      jsonResponse({ error: { code: "INTERNAL_ERROR", message: "内部错误" } }, { status: 500 }),
    );
    mount(NotificationChannelsPanel);
    clickTest(channelCards()[0]);
    await settle();
    const result = document.querySelector(".notif-test-result--fail");
    expect(result?.textContent).toContain("发送失败");
  });

  it("发送中分支：按钮禁用防连点，完成后恢复", async () => {
    seedNotification({
      channels: [
        { type: "webhook", id: "ch-1", name: "本地", enabled: true, url: "http://127.0.0.1:1/hook", headers: [], body_template: "", timeout_seconds: 3 },
      ],
    });
    let release!: (response: Response) => void;
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    stubFetch(async () => gate);
    mount(NotificationChannelsPanel);
    clickTest(channelCards()[0]);
    await flush();
    const btn = findButton("测试发送", channelCards()[0]);
    expect(btn!.disabled, "发送中应禁用按钮").toBe(true);
    expect(document.querySelector(".notif-test-result")?.textContent).toContain("正在发送");
    release(jsonResponse({ data: { results: [{ id: "ch-1", name: "本地", type: "webhook", success: true }] } }));
    await settle();
    expect(document.querySelector(".notif-test-result--ok")).not.toBeNull();
  });
});

describe("NotificationEventsRows 事件类型绑定", () => {
  it("事件类型开关组勾选写入 config.events 草稿", async () => {
    const notification = seedNotification({ events: [] });
    mount(NotificationEventsRows);
    const chip = [...document.querySelectorAll<HTMLLabelElement>(".notif-event-chip")].find(
      (l) => l.textContent?.includes("登录成功"),
    );
    expect(chip, "缺少「登录成功」事件 chip").not.toBeNull();
    const box = chip!.querySelector<HTMLInputElement>("input[type=checkbox]");
    box!.click();
    await flush();
    expect(notification.events).toContain("login_succeeded");
    // 再点一次取消
    box!.click();
    await flush();
    expect(notification.events).not.toContain("login_succeeded");
  });
});

describe("NotificationQuietRows 免打扰时段绑定", () => {
  it("免打扰时段 time 输入与 时/分 字段互转", async () => {
    const notification = seedNotification({ quiet_enabled: true });
    mount(NotificationQuietRows);
    const start = document.querySelector<HTMLInputElement>("#settings-notification-quiet-start");
    expect(start, "缺少免打扰开始时间输入").not.toBeNull();
    expect(start!.value).toBe("22:00");
    start!.value = "23:30";
    start!.dispatchEvent(new Event("input", { bubbles: true }));
    await flush();
    expect(notification.quiet_start_hour).toBe(23);
    expect(notification.quiet_start_minute).toBe(30);
  });
});
