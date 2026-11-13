// @vitest-environment happy-dom
/**
 * ActivityOverview 行为测试（createApp 直挂，免 @vue/test-utils，
 * 与 NetworkDiagnosticsPanel.test.ts 同范式）。
 *
 * 锁定渲染分支与交互红线：
 * - 数据渲染：三块图表按事件数据落位（柱状图段数 / 时间线分段 / 类型分布行）；
 * - 空态：total=0 时展示空态占位；
 * - 错误态：请求失败就地报错 + 重试按钮可再次拉取；
 * - 加载态：请求挂起期间展示加载占位；
 * - days 切换：分段控件切换后按新窗口重新拉取；
 * - 主题切换：颜色全部走 token 类（SVG fill 引用 var(--xxx)），切主题无 JS
 *   重绘路径——直接翻转 data-theme 后 DOM 原样保留且仍无内联颜色。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import ActivityOverview from "./ActivityOverview.vue";
import type { EventEnvelope, EventsResponse } from "@/api/types";

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

function ev(type: EventEnvelope["type"], timestamp: string): EventEnvelope {
  return { id: `${type}-${timestamp}-${Math.random()}`, timestamp, type };
}

/** N 小时前的 RFC3339 时间戳（相对真实时钟，避免断言随运行日期漂移） */
function hoursAgo(h: number): string {
  return new Date(Date.now() - h * 3_600_000).toISOString();
}

/** 有登录成败 + 网络翻转 + 任务的混合样本（时间戳相对现在，全部落在 7 天窗口内） */
function sampleResponse(): EventsResponse {
  return {
    events: [
      ev("login_succeeded", hoursAgo(9)),
      ev("login_failed", hoursAgo(8)),
      ev("network_down", hoursAgo(6)),
      ev("network_up", hoursAgo(4)),
      ev("task_failed", hoursAgo(2)),
    ],
    days: 7,
    total: 5,
    truncated: false,
  };
}

let app: App | null = null;

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(ActivityOverview);
  app.mount(container);
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：异步 fetch + 组件状态刷新，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

/** 安装 fetch 桩，返回 mock 供断言调用参数 */
function stubFetch(handler: (url: string) => Promise<Response>): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (url: string | URL) => handler(String(url)));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** 安装挂起中的 fetch 桩（返回未决 Promise，用于加载态断言） */
function stubPendingFetch(): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn((_url: string | URL) => new Promise<Response>(() => {}));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
});

afterEach(() => {
  app?.unmount();
  app = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ActivityOverview", () => {
  it("渲染三块图表：柱状图 / 时间线分段 / 类型分布行", async () => {
    stubFetch((url) => {
      expect(url).toContain("/api/events?days=7");
      return Promise.resolve(jsonResponse({ data: sampleResponse() }));
    });
    mount();
    await settle();

    // 柱状图：7 个日期桶，今日桶有一根成功柱与一根失败柱
    const svg = document.querySelector('[data-testid="activity-bars"]');
    expect(svg).not.toBeNull();
    const successBars = svg!.querySelectorAll(".activity-bar--success");
    const failedBars = svg!.querySelectorAll(".activity-bar--failed");
    expect(successBars.length).toBe(1);
    expect(failedBars.length).toBe(1);
    // 日期标签行与桶数一致（等宽占位）
    expect(document.querySelectorAll(".activity-day-labels span")).toHaveLength(7);

    // 时间线：unknown 头段 + down 段 + up 末段（末段延续到窗口末尾）
    const segs = document.querySelectorAll('[data-testid="activity-timeline"] .activity-seg');
    expect(segs.length).toBe(3);
    expect(segs[0].className).toContain("activity-seg--unknown");
    expect(segs[1].className).toContain("activity-seg--down");
    expect(segs[2].className).toContain("activity-seg--up");
    expect(segs[0].getAttribute("style")).toContain("width");

    // 类型分布：五种类型各 1 次，计数并列按类型名升序
    const rows = document.querySelectorAll('[data-testid="activity-types"] .activity-type-row');
    expect(rows).toHaveLength(5);
    const labels = Array.from(rows).map((r) => r.querySelector(".activity-type-label")?.textContent);
    expect(labels).toEqual(["登录失败", "登录成功", "网络断开", "网络恢复", "任务失败"]);
    const counts = Array.from(rows).map((r) => r.querySelector(".activity-type-count")?.textContent);
    expect(counts).toEqual(["1", "1", "1", "1", "1"]);
    // 峰值提示与翻计数
    expect(document.querySelector(".activity-scale-hint")?.textContent).toContain("峰值");
    expect(document.querySelector(".activity-flip-count")?.textContent).toContain("2 次");
  });

  it("空数据展示空态占位", async () => {
    stubFetch(() =>
      Promise.resolve(jsonResponse({ data: { events: [], days: 7, total: 0, truncated: false } })),
    );
    mount();
    await settle();

    expect(document.querySelector('[data-testid="activity-empty"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="activity-empty"]')?.textContent).toContain("近 7 天暂无事件");
    expect(document.querySelector('[data-testid="activity-bars"]')).toBeNull();
  });

  it("请求失败展示错误态，重试按钮可再次拉取", async () => {
    const fetchMock = stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "INTERNAL_ERROR", message: "事件流读取失败" } }, { status: 500 }),
      ),
    );
    mount();
    await settle();

    const errorBox = document.querySelector('[data-testid="activity-error"]');
    expect(errorBox).not.toBeNull();
    expect(errorBox?.textContent).toContain("事件流读取失败");
    expect(document.querySelector('[data-testid="activity-bars"]')).toBeNull();

    // 重试改走成功桩：同一 fetch 桩切实现，验证错误态可恢复
    fetchMock.mockImplementation(() =>
      Promise.resolve(jsonResponse({ data: { ...sampleResponse(), truncated: false, days: 7 } })),
    );
    const retry = document.querySelector<HTMLButtonElement>('[data-testid="activity-retry"]');
    expect(retry).not.toBeNull();
    retry!.click();
    await settle();
    expect(document.querySelector('[data-testid="activity-error"]')).toBeNull();
    expect(document.querySelector('[data-testid="activity-bars"]')).not.toBeNull();
  });

  it("请求挂起期间展示加载态", async () => {
    stubPendingFetch();
    mount();
    await flush();
    expect(document.querySelector('[data-testid="activity-loading"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="activity-bars"]')).toBeNull();
  });

  it("days 切换按新窗口重新拉取", async () => {
    const fetchMock = stubFetch(() => Promise.resolve(jsonResponse({ data: sampleResponse() })));
    mount();
    await settle();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    const btn30 = document.querySelector<HTMLButtonElement>('[data-testid="activity-days-30"]');
    expect(btn30, "找不到 30 天切换按钮").not.toBeNull();
    btn30!.click();
    await settle();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1]?.[0]).toContain("/api/events?days=30");
    // 30 天窗口下日期标签隔日显示（15 个非空标签 + 15 个空占位）
    const labels = Array.from(document.querySelectorAll(".activity-day-labels span"));
    expect(labels).toHaveLength(30);
    expect(labels.filter((l) => l.textContent !== "")).toHaveLength(15);
  });

  it("主题切换无需 JS 重绘：颜色走 token 类，翻转 data-theme 后 DOM 原样保留", async () => {
    stubFetch(() => Promise.resolve(jsonResponse({ data: sampleResponse() })));
    mount();
    await settle();

    // 柱状图与时间线颜色全部来自 token 类（fill 定义在 CSS），无内联颜色样式
    const bar = document.querySelector<SVGElement>(".activity-bar--success");
    expect(bar?.getAttribute("fill")).toBeNull();
    expect(bar?.getAttribute("style")).toBeNull();

    // 翻转 data-theme（模拟深浅切换）：DOM 图表节点原样保留，无重绘副作用
    const before = document.querySelector('[data-testid="activity-bars"]')?.outerHTML;
    document.documentElement.setAttribute("data-theme", "light");
    await flush();
    const after = document.querySelector('[data-testid="activity-bars"]')?.outerHTML;
    expect(after).toBe(before);
    expect(document.querySelector(".activity-bar--success")).not.toBeNull();
    // 时间线分段同样只用 token 类着色（width 是布局属性，颜色不含内联色值）
    const seg = document.querySelector<HTMLElement>(".activity-seg--up");
    expect(seg?.getAttribute("style") ?? "").not.toContain("#");
    expect(seg?.getAttribute("style") ?? "").toContain("width");
  });

  it("truncated 标记展示截断提示", async () => {
    stubFetch(() =>
      Promise.resolve(jsonResponse({ data: { ...sampleResponse(), truncated: true } })),
    );
    mount();
    await settle();
    expect(document.querySelector(".activity-truncated")?.textContent).toContain("未计入");
  });
});
