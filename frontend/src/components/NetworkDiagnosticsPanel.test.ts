// @vitest-environment happy-dom
/**
 * NetworkDiagnosticsPanel 行为测试（createApp 直挂，免 @vue/test-utils，
 * 与 AuthBootstrapGuide.test.ts 同范式）。
 *
 * 锁定渲染分支与交互红线：
 * - 全 ok 报告：健康结论高亮 + 五项按序渲染；
 * - 含 fail 报告：结论判脏 + 失败项着色 + 细节折叠展开；
 * - 请求失败：错误态就地展示，不残留旧报告；
 * - 防连点：诊断进行中按钮禁用、重复触发只发一次请求；
 * - 导出诊断包：成功触发 blob 下载（a.download 命名）、失败三分支
 *   （HTTP 错误回显 message / 请求异常兜底文案 / 超时文案）就地报错、防连点。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import NetworkDiagnosticsPanel from "./NetworkDiagnosticsPanel.vue";
import type { DiagnosticCheck, DiagnosticReport } from "@/api/types";

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

/** 构造 zip blob 响应（导出下载桩） */
function zipResponse(): Response {
  return {
    ok: true,
    status: 200,
    headers: new Headers({ "content-type": "application/zip" }),
    blob: async () => new Blob(["PK"], { type: "application/zip" }),
    text: async () => "",
  } as unknown as Response;
}

function makeCheck(
  overrides: Partial<DiagnosticCheck> & Pick<DiagnosticCheck, "key" | "name" | "status">,
): DiagnosticCheck {
  return { elapsed_ms: 12, summary: "摘要", details: {}, ...overrides };
}

const ALL_OK_REPORT: DiagnosticReport = {
  checks: [
    makeCheck({
      key: "interfaces",
      name: "本机网卡",
      status: "ok",
      summary: "发现 1 块可用网卡",
      details: { count: 1, adapters: [{ name: "以太网", ipv4: "10.0.0.2", dns_servers: [] }] },
    }),
    makeCheck({
      key: "gateway",
      name: "默认网关",
      status: "ok",
      summary: "网关 192.168.1.1 可达（TCP 192.168.1.1:80 连通）",
      details: { gateways: ["192.168.1.1"], probes: [] },
    }),
    makeCheck({
      key: "dns",
      name: "DNS 解析",
      status: "ok",
      summary: "主机 portal.campus.edu 解析到 1 个地址（30ms）",
      details: { host: "portal.campus.edu", ips: ["10.10.1.1"] },
    }),
    makeCheck({
      key: "portal",
      name: "认证门户",
      status: "ok",
      summary: "门户可达（HTTP 200，45ms）",
      details: { url: "http://portal.campus.edu:8080/login", http_status: 200 },
    }),
    makeCheck({
      key: "history",
      name: "近期登录失败",
      status: "skip",
      summary: "最近 7 天无登录失败记录",
      details: { window_days: 7, failure_count: 0 },
    }),
  ],
  verdict: "healthy",
  conclusion: "各项检查均通过（1 项跳过）",
  total_ms: 90,
};

const FAIL_REPORT: DiagnosticReport = {
  checks: [
    ALL_OK_REPORT.checks[0],
    makeCheck({
      key: "gateway",
      name: "默认网关",
      status: "fail",
      elapsed_ms: 4000,
      summary: "网关 192.168.1.1 不可达：tcp connect timeout",
      details: {
        gateways: ["192.168.1.1"],
        probes: [{ target: "192.168.1.1:80", success: false, elapsed_ms: 4000, error: "tcp connect timeout" }],
      },
    }),
    ALL_OK_REPORT.checks[2],
    makeCheck({
      key: "portal",
      name: "认证门户",
      status: "warn",
      summary: "门户响应异常（HTTP 503）：链路可达，但门户服务可能未就绪",
      details: { url: "http://portal.campus.edu:8080/login", http_status: 503 },
    }),
    ALL_OK_REPORT.checks[4],
  ],
  verdict: "faulty",
  conclusion: "发现 1 项异常（默认网关），请优先处理失败项",
  total_ms: 4000,
};

let app: App | null = null;

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(NetworkDiagnosticsPanel);
  app.mount(container);
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：诊断是异步 fetch + 组件状态刷新，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

/** 安装 fetch 桩，返回 mock 供断言调用次数 */
function stubFetch(handler: (url: string) => Promise<Response>): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (url: string | URL) => handler(String(url)));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function clickStart(): void {
  const button = document.querySelector<HTMLButtonElement>('[data-testid="diag-start"]');
  expect(button, "找不到开始诊断按钮").not.toBeNull();
  button!.click();
}

function clickExport(): void {
  const button = document.querySelector<HTMLButtonElement>('[data-testid="diag-export"]');
  expect(button, "找不到导出诊断包按钮").not.toBeNull();
  button!.click();
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
  // 预置 token：组件内 API 调用不再触发 ensureAuthToken 的回环 local-token 探测，
  // 避免多一次 fetch 干扰本文件的调用计数断言
  sessionStorage.setItem("campus_auth_token", "tok-test");
  // 下载分支统一打桩：不真解析 blob（happy-dom 已实现，仅需可断言的桩）
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:mock");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
});

afterEach(() => {
  app?.unmount();
  app = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("NetworkDiagnosticsPanel", () => {
  it("全 ok 报告渲染健康结论与五项检查", async () => {
    stubFetch((url) => {
      expect(url).toContain("/api/diagnostics/run");
      return Promise.resolve(jsonResponse({ data: ALL_OK_REPORT }));
    });
    mount();
    clickStart();
    await settle();

    const verdict = document.querySelector('[data-testid="diag-verdict"]');
    expect(verdict?.textContent).toContain("链路正常");
    expect(verdict?.textContent).toContain("各项检查均通过");
    expect(verdict?.className).toContain("diag-verdict--healthy");

    const items = Array.from(document.querySelectorAll(".diag-check"));
    expect(items).toHaveLength(5);
    expect(items.map((el) => el.querySelector(".diag-check-name")?.textContent)).toEqual([
      "本机网卡",
      "默认网关",
      "DNS 解析",
      "认证门户",
      "近期登录失败",
    ]);
    // 结论着色类按状态落位
    expect(items[0].className).toContain("diag-status--ok");
    expect(items[4].className).toContain("diag-status--skip");
  });

  it("含失败项时结论判脏且细节可展开收起", async () => {
    stubFetch(() => Promise.resolve(jsonResponse({ data: FAIL_REPORT })));
    mount();
    clickStart();
    await settle();

    const verdict = document.querySelector('[data-testid="diag-verdict"]');
    expect(verdict?.className).toContain("diag-verdict--faulty");
    expect(verdict?.textContent).toContain("发现异常");

    const items = Array.from(document.querySelectorAll(".diag-check"));
    const failItem = items[1];
    expect(failItem.className).toContain("diag-status--fail");
    expect(failItem.querySelector(".diag-check-summary")?.textContent).toContain("不可达");
    // 警告项同样按状态着色（四档状态在两个渲染分支中全覆盖）
    expect(items[3].className).toContain("diag-status--warn");

    // 细节默认折叠，点开出现结构化行，再点收起
    const toggle = failItem.querySelector<HTMLButtonElement>(".diag-detail-toggle");
    expect(toggle, "失败项应有细节开关").not.toBeNull();
    expect(failItem.querySelector(".diag-check-details")).toBeNull();
    toggle!.click();
    await flush();
    const details = failItem.querySelector(".diag-check-details");
    expect(details?.textContent).toContain("网关: 192.168.1.1");
    // 顶层键走「键: 值」，嵌套对象走「键=值」连写
    expect(details?.textContent).toContain("错误=tcp connect timeout");
    toggle!.click();
    await flush();
    expect(failItem.querySelector(".diag-check-details")).toBeNull();
  });

  it("请求失败展示错误态且不渲染报告", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "INTERNAL_ERROR", message: "诊断执行失败" } }, { status: 500 }),
      ),
    );
    mount();
    clickStart();
    await settle();

    const alert = document.querySelector(".diag-error");
    expect(alert?.textContent).toContain("诊断执行失败");
    expect(alert?.getAttribute("role")).toBe("alert");
    expect(document.querySelector('[data-testid="diag-verdict"]')).toBeNull();
  });

  it("诊断进行中按钮禁用且防连点只发一次请求", async () => {
    let release!: (report: DiagnosticReport) => void;
    const fetchMock = stubFetch(
      () =>
        new Promise<Response>((resolve) => {
          release = (report) => resolve(jsonResponse({ data: report }));
        }),
    );
    mount();
    clickStart();
    // 等待请求发出且 pending（release 未调用）、DOM 已切换为 running 态
    await settle();
    const button = document.querySelector<HTMLButtonElement>('[data-testid="diag-start"]');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(button?.disabled).toBe(true);
    expect(button?.textContent).toContain("诊断中");
    // 连点被双重拦下：disabled 元素不派发 click + running 守卫早退
    button!.click();
    clickStart();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    release(ALL_OK_REPORT);
    await settle();
    // 复位：按钮恢复可用并渲染报告
    expect(button?.disabled).toBe(false);
    expect(document.querySelector('[data-testid="diag-verdict"]')).not.toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  // ============ 导出诊断包 ============

  it("导出成功触发 blob 下载且命名含日期前缀", async () => {
    // a 元素点击前已被 remove()，经 prototype click 捕获下载属性
    const clicked: HTMLAnchorElement[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      clicked.push(this);
    });
    stubFetch((url) => {
      expect(url).toContain("/api/diagnostics/bundle");
      return Promise.resolve(zipResponse());
    });
    mount();
    clickExport();
    await settle();

    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:mock");
    expect(clicked).toHaveLength(1);
    // a.download 命名 campus-auth-diagnostics-<date>.zip
    expect(clicked[0].download).toMatch(/^campus-auth-diagnostics-\d{4}-\d{2}-\d{2}\.zip$/);
    // 无错误态；按钮复位可用
    expect(document.querySelector('[data-testid="diag-export-error"]')).toBeNull();
    const button = document.querySelector<HTMLButtonElement>('[data-testid="diag-export"]');
    expect(button?.disabled).toBe(false);
    expect(button?.textContent).toContain("导出诊断包");
  });

  it("导出失败三分支就地报错且不触发下载", async () => {
    // 分支一：HTTP 错误 → 回显后端 error.message
    const fetchMock = stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "INTERNAL_ERROR", message: "收集任务失败" } }, { status: 500 }),
      ),
    );
    mount();
    clickExport();
    await settle();
    let alert = document.querySelector('[data-testid="diag-export-error"]');
    expect(alert?.textContent).toContain("收集任务失败");
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // 分支二：请求异常（网络断开等）→ 错误消息透传就地展示
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );
    clickExport();
    await settle();
    alert = document.querySelector('[data-testid="diag-export-error"]');
    expect(alert?.textContent).toContain("Failed to fetch");

    // 分支三：导出超时（fetchBundleWithTimeout 内 AbortError 转译）→ 超时文案
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new DOMException("The operation was aborted.", "AbortError");
      }),
    );
    clickExport();
    await settle();
    alert = document.querySelector('[data-testid="diag-export-error"]');
    expect(alert?.textContent).toContain("导出超时");
    // 三个分支全程未触发下载
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it("导出进行中按钮禁用且防连点只发一次请求", async () => {
    let release!: () => void;
    const fetchMock = stubFetch(
      () =>
        new Promise<Response>((resolve) => {
          release = () => resolve(zipResponse());
        }),
    );
    mount();
    clickExport();
    await settle();
    const button = document.querySelector<HTMLButtonElement>('[data-testid="diag-export"]');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(button?.disabled).toBe(true);
    expect(button?.textContent).toContain("导出中");
    // 连点被双重拦下：disabled 元素不派发 click + exporting 守卫早退
    button!.click();
    clickExport();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    release();
    await settle();
    expect(button?.disabled).toBe(false);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
