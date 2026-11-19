// @vitest-environment happy-dom
/**
 * PersistentSessionSection 行为测试（createApp 直挂，免 @vue/test-utils，
 * 与 NetworkDiagnosticsPanel.test.ts 同范式）。
 *
 * 锁定配置会话编排的关键分支：
 * - 分区渲染：开关随草稿开/关、已有录制数展示、按钮文案（打开/重新录制）；
 * - 开关交互：点击即写回草稿（走既有方案保存通道，本组件不直接发 PUT）；
 * - start 成功 → 横幅（指引 + 完成/放弃）→ 完成配置 → stop 落盘 →
 *   开关自动勾选 + 录制数展示 + sessionStopped 通知宿主刷新；
 * - 409 占用 → 固定文案「已有配置会话进行中」（不透传后端 message）；
 * - 失败三分支：409 固定文案 / 后端中文 message 直显 / 网络异常兜底文案；
 * - 放弃路径：调 stop（?discard=true，后端不落盘录制）关浏览器但**不采纳**返回
 *   （开关不动、录制数不更新、不通知宿主）；stop 409（会话已消失）静默收场；
 * - discard 请求契约：放弃 URL 带 discard=true，完成配置 URL 不带；
 * - 完成配置失败：横幅保留可重试，放弃可退出；
 * - 截断提示；未保存方案只给指引；start 进行中防连点。
 * - 自由浏览（打开浏览器）：未启用持久会话时禁用并提示；启用后 start 带
 *   `?record=false`，横幅为自由浏览文案且只有「关闭浏览器」（走 discard=true）；
 *   409 占用走 toast 固定文案「配置会话进行中，请先关闭」；不写草稿、不通知宿主。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, reactive, type App } from "vue";
import PersistentSessionSection from "./PersistentSessionSection.vue";
import { useToast } from "@/composables/useToast";
import type { RecordedAction } from "@/api/types";
import type { PersistentSessionDraft } from "./PersistentSessionSection.vue";

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

/** 三个录制动作样例（点击 + select 混合，形状与 Worker 录制契约一致） */
function makeActions(count: number): RecordedAction[] {
  return Array.from({ length: count }, (_, i) => ({
    type: i % 2 === 0 ? "click" : "change",
    selector: `#login-submit-${i}`,
    strategy: "id",
    ts: 1700000000000 + i,
    ...(i % 2 === 1 ? { value: "cmcc" } : {}),
  }));
}

let app: App | null = null;

/** 直挂宿主：reactive 草稿（组件原地改成员字段需响应式才能触发重渲染） */
function mountHost(
  opts: { draft?: Partial<PersistentSessionDraft>; profileId?: string; onStopped?: () => void } = {},
): PersistentSessionDraft {
  const draft = reactive<PersistentSessionDraft>({
    persistent_session_enabled: false,
    recorded_actions: [],
    ...opts.draft,
  });
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(PersistentSessionSection, {
    modelValue: draft,
    profileId: opts.profileId,
    ...(opts.onStopped ? { onSessionStopped: opts.onStopped } : {}),
  });
  app.mount(container);
  return draft;
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：start/stop 是异步 fetch + 组件状态刷新，单微任务等不到 */
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

function click(testId: string): HTMLButtonElement {
  const button = document.querySelector<HTMLButtonElement>(`[data-testid="${testId}"]`);
  expect(button, `找不到按钮 ${testId}`).not.toBeNull();
  button!.click();
  return button!;
}

function toggleChecked(): string | null {
  return document
    .querySelector('[data-testid="session-toggle"] [role="switch"]')
    ?.getAttribute("aria-checked") ?? null;
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
  // 预置 token：组件内 API 调用不再触发 ensureAuthToken 的回环 local-token 探测，
  // 避免多一次 fetch 干扰本文件的调用计数断言
  sessionStorage.setItem("campus_auth_token", "tok-test");
  // toast 是模块级单例（真计时器 3s 后才清空）：逐用例复位防跨用例串扰
  useToast().toast.message = "";
});

afterEach(() => {
  app?.unmount();
  app = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("PersistentSessionSection", () => {
  it("关态渲染：开关未勾选、无录制数、按钮为「打开浏览器配置」", () => {
    mountHost({ profileId: "dorm" });
    expect(toggleChecked()).toBe("false");
    expect(document.querySelector('[data-testid="session-count"]')).toBeNull();
    const start = document.querySelector<HTMLButtonElement>('[data-testid="session-start"]');
    expect(start?.textContent).toContain("打开浏览器配置");
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
  });

  it("开态渲染：开关勾选、录制数展示、按钮变为「重新录制」", () => {
    mountHost({ profileId: "dorm", draft: { persistent_session_enabled: true, recorded_actions: makeActions(3) } });
    expect(toggleChecked()).toBe("true");
    const count = document.querySelector('[data-testid="session-count"]');
    expect(count?.textContent).toContain("已录制 3 个动作");
    const start = document.querySelector<HTMLButtonElement>('[data-testid="session-start"]');
    expect(start?.textContent).toContain("重新录制");
  });

  it("开关点击写回草稿（保存通道字段，组件不发 PUT）", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const draft = mountHost({ profileId: "dorm" });
    click("session-toggle");
    await flush();
    expect(draft.persistent_session_enabled).toBe(true);
    expect(toggleChecked()).toBe("true");
    // 全程只挂载组件，不应有任何请求发出（保存走宿主的方案保存通道）
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("start 成功 → 横幅指引 → 完成配置 → 开关勾选 + 录制数 + 通知宿主", async () => {
    const stopUrls: string[] = [];
    const fetchMock = stubFetch((url) => {
      if (url.includes("/api/profiles/dorm/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/api/profiles/dorm/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(
          jsonResponse({ data: { recorded_count: 2, truncated: false, persistent_session_enabled: true } }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    let stoppedEmitted = 0;
    const draft = mountHost({ profileId: "dorm", onStopped: () => stoppedEmitted++ });

    click("session-start");
    await settle();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    // 横幅出现且带冻结指引文案；start 按钮进入禁用态
    const banner = document.querySelector('[data-testid="session-banner"]');
    expect(banner?.textContent).toContain("在打开的浏览器中登录门户（勾选门户的记住密码）");
    expect(banner?.textContent).toContain("完成配置");
    expect(document.querySelector<HTMLButtonElement>('[data-testid="session-start"]')?.disabled).toBe(true);

    click("session-finish");
    await settle();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    // 完成配置 = 落盘采纳：stop 不带 discard 参数
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).not.toContain("discard");
    // 横幅收场；开关随 stop 响应自动勾选（写回草稿）；录制数来自响应计数
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(draft.persistent_session_enabled).toBe(true);
    expect(toggleChecked()).toBe("true");
    expect(document.querySelector('[data-testid="session-count"]')?.textContent).toContain("已录制 2 个动作");
    expect(stoppedEmitted).toBe(1);
  });

  it("start 409 显示固定文案，不透传后端 message", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "CONFLICT", message: "已存在活跃配置会话" } }, { status: 409 }),
      ),
    );
    mountHost({ profileId: "dorm" });
    click("session-start");
    await settle();
    const alert = document.querySelector('[data-testid="session-error"]');
    expect(alert?.textContent).toContain("已有配置会话进行中");
    expect(alert?.textContent).not.toContain("已存在活跃配置会话");
    expect(alert?.getAttribute("role")).toBe("alert");
    // 失败后回到 idle：按钮恢复可用
    expect(document.querySelector<HTMLButtonElement>('[data-testid="session-start"]')?.disabled).toBe(false);
  });

  it("start 失败三分支：409 固定文案 / 后端 message 直显 / 网络异常兜底", async () => {
    // 分支一：409 占用 → 固定文案（前面用例已断言不透传，这里只验分支可达）
    stubFetch(() =>
      Promise.resolve(jsonResponse({ error: { code: "CONFLICT", message: "已存在活跃配置会话" } }, { status: 409 })),
    );
    mountHost({ profileId: "dorm" });
    click("session-start");
    await settle();
    expect(document.querySelector('[data-testid="session-error"]')?.textContent).toContain("已有配置会话进行中");

    // 分支二：其他后端错误 → 中文 message 原样直显
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ error: { code: "INTERNAL_ERROR", message: "配置浏览器启动失败：未找到可用浏览器" } }, { status: 500 }),
      ),
    );
    click("session-start");
    await settle();
    expect(document.querySelector('[data-testid="session-error"]')?.textContent).toContain(
      "配置浏览器启动失败：未找到可用浏览器",
    );

    // 分支三：网络异常（后端不可达）→ 客户端兜底文案
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );
    click("session-start");
    await settle();
    expect(document.querySelector('[data-testid="session-error"]')?.textContent).toContain(
      "网络连接失败，请检查后端是否已启动",
    );
  });

  it("放弃：调 stop（?discard=true）关浏览器但不采纳返回（开关不动、录制数不更新、不通知宿主）", async () => {
    const stopUrls: string[] = [];
    const fetchMock = stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      // discard=true 时后端不落盘，但响应仍回真实计数：放弃语义必须无视它
      if (url.includes("/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(
          jsonResponse({ data: { recorded_count: 5, truncated: false, persistent_session_enabled: true } }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    let stoppedEmitted = 0;
    const draft = mountHost({ profileId: "dorm", onStopped: () => stoppedEmitted++ });

    click("session-start");
    await settle();
    click("session-abandon");
    await settle();
    // stop 已发出（浏览器确实被关掉），且带 discard=true（放弃 = 后端不落盘录制、不写方案）……
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).toContain("discard=true");
    // ……但返回被丢弃：开关保持关态、无录制数、横幅收场、宿主未被通知
    expect(draft.persistent_session_enabled).toBe(false);
    expect(toggleChecked()).toBe("false");
    expect(document.querySelector('[data-testid="session-count"]')).toBeNull();
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(stoppedEmitted).toBe(0);
    // 放弃路径不产生错误提示
    expect(document.querySelector('[data-testid="session-error"]')).toBeNull();
  });

  it("放弃时 stop 409（会话已消失）静默收场", async () => {
    const stopUrls: string[] = [];
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      stopUrls.push(url);
      return Promise.resolve(
        jsonResponse({ error: { code: "NOT_FOUND", message: "无活跃配置会话" } }, { status: 409 }),
      );
    });
    mountHost({ profileId: "dorm" });
    click("session-start");
    await settle();
    click("session-abandon");
    await settle();
    // 放弃语义与结果无关：即使会话已消失，请求仍须带 discard=true
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).toContain("discard=true");
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(document.querySelector('[data-testid="session-error"]')).toBeNull();
  });

  it("完成配置失败：横幅保留可重试，放弃可退出", async () => {
    let stopCalls = 0;
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/config-session/stop")) {
        stopCalls++;
        if (stopCalls === 1) {
          return Promise.resolve(jsonResponse({ error: { code: "INTERNAL_ERROR", message: "录制动作落盘失败" } }, { status: 500 }));
        }
        return Promise.resolve(
          jsonResponse({ error: { code: "CONFLICT", message: "无活跃配置会话" } }, { status: 409 }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    const draft = mountHost({ profileId: "dorm" });

    click("session-start");
    await settle();
    click("session-finish");
    await settle();
    // 首次 stop 失败：错误直显，横幅保留（会话可能仍在，可重试完成）
    expect(document.querySelector('[data-testid="session-error"]')?.textContent).toContain("录制动作落盘失败");
    expect(document.querySelector('[data-testid="session-banner"]')).not.toBeNull();
    expect(draft.persistent_session_enabled).toBe(false);

    // 二次 stop 409（会话已没了）：横幅退场并说明原因，开关未被误写
    click("session-finish");
    await settle();
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(document.querySelector('[data-testid="session-error"]')?.textContent).toContain("无活跃配置会话");
    expect(draft.persistent_session_enabled).toBe(false);
  });

  it("stop 截断：显示截断警告与保留数量", async () => {
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      return Promise.resolve(
        jsonResponse({ data: { recorded_count: 30, truncated: true, persistent_session_enabled: true } }),
      );
    });
    mountHost({ profileId: "dorm" });
    click("session-start");
    await settle();
    click("session-finish");
    await settle();
    expect(document.querySelector('[data-testid="session-truncated"]')?.textContent).toContain("仅保留前 30 个动作");
    expect(document.querySelector('[data-testid="session-count"]')?.textContent).toContain("已录制 30 个动作");
  });

  it("新建未保存方案：无 start 按钮，仅提示先保存", () => {
    mountHost({});
    expect(document.querySelector('[data-testid="session-start"]')).toBeNull();
    expect(document.querySelector(".session-actions")?.textContent).toContain("保存方案后才能打开配置浏览器");
    // 开关仍可编辑（随方案创建载荷落盘）
    expect(toggleChecked()).toBe("false");
  });

  it("start 进行中防连点：按钮禁用且重复点击只发一次请求", async () => {
    let release!: () => void;
    const fetchMock = stubFetch(
      () =>
        new Promise<Response>((resolve) => {
          release = () => resolve(jsonResponse({ data: { started: true } }));
        }),
    );
    mountHost({ profileId: "dorm" });
    const button = click("session-start");
    await settle();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(button.disabled).toBe(true);
    expect(button.textContent).toContain("正在打开浏览器");
    // 连点被双重拦下：disabled 元素不派发 click + starting 阶段守卫早退
    button.click();
    click("session-start");
    expect(fetchMock).toHaveBeenCalledTimes(1);

    release();
    await settle();
    expect(button.disabled).toBe(true); // 会话进行中仍禁用 start
    expect(document.querySelector('[data-testid="session-banner"]')).not.toBeNull();
  });

  // ============ 自由浏览（打开浏览器，record=false）============

  it("自由浏览：未启用持久会话时按钮禁用并提示，点击不发请求", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    mountHost({ profileId: "dorm", draft: { persistent_session_enabled: false } });
    const open = document.querySelector<HTMLButtonElement>('[data-testid="session-open"]');
    expect(open, "已保存方案应渲染自由浏览按钮").not.toBeNull();
    expect(open?.disabled).toBe(true);
    expect(document.querySelector('[data-testid="session-open-hint"]')?.textContent).toContain(
      "启用持久会话并保存方案",
    );
    // disabled 元素 click 不派发（happy-dom 同真实 DOM 语义）+ 组件守卫早退双保险
    open!.click();
    await flush();
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("自由浏览：start 带 ?record=false，横幅为自由浏览文案，关闭浏览器走 discard=true", async () => {
    const urls: string[] = [];
    stubFetch((url) => {
      urls.push(url);
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      // 自由浏览关闭的回包无录制可采纳：响应内容应被整路丢弃
      return Promise.resolve(
        jsonResponse({ data: { recorded_count: 0, truncated: false, persistent_session_enabled: true } }),
      );
    });
    let stoppedEmitted = 0;
    const draft = mountHost({
      profileId: "dorm",
      draft: { persistent_session_enabled: true, recorded_actions: makeActions(2) },
      onStopped: () => stoppedEmitted++,
    });

    click("session-open");
    await settle();
    // start 契约：自由浏览必须带 record=false（后端不注入录制器）
    expect(urls.some((u) => u.includes("/config-session/start?record=false"))).toBe(true);
    // 横幅为自由浏览文案 + 只有「关闭浏览器」（无完成配置/放弃录制动作）
    const banner = document.querySelector('[data-testid="session-banner"]');
    expect(banner?.textContent).toContain("浏览器已打开");
    expect(banner?.textContent).toContain("直接关闭浏览器窗口");
    expect(document.querySelector('[data-testid="session-close"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="session-finish"]')).toBeNull();
    expect(document.querySelector('[data-testid="session-abandon"]')).toBeNull();

    click("session-close");
    await settle();
    // 关闭走 discard=true；不写草稿、既有录制数展示不动、不通知宿主
    expect(urls.some((u) => u.includes("/config-session/stop?discard=true"))).toBe(true);
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(draft.persistent_session_enabled).toBe(true);
    expect(document.querySelector('[data-testid="session-count"]')?.textContent).toContain("已录制 2 个动作");
    expect(stoppedEmitted).toBe(0);
    expect(document.querySelector('[data-testid="session-error"]')).toBeNull();
  });

  it("自由浏览 start 409：toast 固定文案「配置会话进行中，请先关闭」，不占用就地错误区", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "CONFLICT", message: "已存在活跃配置会话" } }, { status: 409 }),
      ),
    );
    const { toast } = useToast();
    mountHost({ profileId: "dorm", draft: { persistent_session_enabled: true } });
    click("session-open");
    await settle();
    expect(toast.message).toBe("配置会话进行中，请先关闭");
    expect(toast.success).toBe(false);
    expect(document.querySelector('[data-testid="session-error"]')).toBeNull();
    // 失败后回到 idle：按钮恢复可用
    expect(document.querySelector<HTMLButtonElement>('[data-testid="session-open"]')?.disabled).toBe(false);
  });

  it("自由浏览关闭时 stop 409（用户已直接关窗）静默收场", async () => {
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      return Promise.resolve(
        jsonResponse({ error: { code: "CONFLICT", message: "无活跃配置会话" } }, { status: 409 }),
      );
    });
    mountHost({ profileId: "dorm", draft: { persistent_session_enabled: true } });
    click("session-open");
    await settle();
    click("session-close");
    await settle();
    expect(document.querySelector('[data-testid="session-banner"]')).toBeNull();
    expect(document.querySelector('[data-testid="session-error"]')).toBeNull();
  });
});
