// @vitest-environment happy-dom
/**
 * TaskRecordingDialog 行为测试（createApp 直挂 + fetch 桩，与
 * PersistentSessionSection.test.ts 同范式；下拉交互不经 UI——直接写
 * useTaskRecording 单例的 profileId，组件只验证编排与渲染）。
 *
 * 锁定录制流程的关键分支：
 * - 选方案阶段：未选方案时「开始录制」禁用；
 * - start 409 → 固定文案「已有配置会话进行中」（不透传后端 message）；
 * - start 成功 → 录制横幅（指引含占位符说明）→ 完成录制 → 预览步骤列表
 *   （四类动作转换渲染 + 占位符原样展示）→ 默认名称/描述 → 保存载荷（PUT
 *   /api/tasks/untitled_1：type/url/steps 与转换器口径一致）→ 弹窗关闭；
 * - 完成录制走 stop + `apply=false`（仅结算取回动作，不写方案——任务录制零副作用）；
 * - 预览阶段单步删除（删错步骤后保存载荷同步收缩）；
 * - 空录制（响应无动作数组）→ 回到选方案 + 就地提示（不回读方案详情——
 *   apply=false 下方案未被写入，回读只会拿到旧录制）；
 * - 放弃录制 → stop 带 discard=true（后端不落盘）；
 * - starting 窗口取消（P2-14）：start 在途关闭弹窗 → 立即关窗不发 stop（会话未登记，
 *   发了也 409），start 落地后补发一次 stop(discard) 收尾，不进录制态；补发撞 409
 *   （会话实际未落地）静默收场不弹失败提示。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import TaskRecordingDialog from "./TaskRecordingDialog.vue";
import { useTaskRecording } from "./useTaskRecording";
import { useProfiles } from "@/composables/useProfiles";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import type { ProfileSummary } from "@/api/types";

/** 构造 JSON 响应（与 PersistentSessionSection.test.ts 同款桩） */
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

/** 完成录制取回的动作序列：覆盖四类动作（navigate + 凭据占位符 fill + change + click） */
function recordedActions(): unknown[] {
  return [
    { type: "navigate", ts: 1, value: "http://portal.example.com/login" },
    { type: "fill", ts: 2, selector: "#username", strategy: "id", value: "{{USERNAME}}" },
    { type: "fill", ts: 3, selector: "#password", strategy: "id", value: "{{PASSWORD}}" },
    { type: "change", ts: 4, selector: "select[name=isp]", strategy: "name", value: "cmcc" },
    { type: "click", ts: 5, selector: "#login-submit", strategy: "id" },
  ];
}

/** 方案下拉数据源（useProfiles 单例直写，绕开列表拉取） */
function makeSummary(id = "dorm", name = "宿舍网"): ProfileSummary {
  return {
    id,
    name,
    username: "2025001",
    isp: "",
    active_task: "",
    active_http_task: "",
    active_script_task: "",
    login_channel: "browser",
    gateway_ip: "",
    wifi_ssid: "",
  };
}

let app: App | null = null;

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：start/stop 是异步 fetch + 组件状态刷新，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

/** 安装 fetch 桩（按 URL 前缀分发），返回 mock 供断言调用与载荷 */
function stubFetch(handler: (url: string, init?: RequestInit) => Promise<Response>): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => handler(String(url), init));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** 挂载弹窗并打开到选方案阶段 */
function mountDialog(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(TaskRecordingDialog);
  app.mount(container);
  useTaskRecording().openDialog();
}

function click(testId: string): HTMLButtonElement {
  const button = document.querySelector<HTMLButtonElement>(`[data-testid="${testId}"]`);
  expect(button, `找不到按钮 ${testId}`).not.toBeNull();
  button!.click();
  return button!;
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
  // 预置 token：组件内 API 调用不再触发 ensureAuthToken 的回环 local-token 探测
  sessionStorage.setItem("campus_auth_token", "tok-test");
  // toast 是模块级单例（真计时器 3s 后才清空）：逐用例复位防跨用例串扰
  useToast().toast.message = "";
  useProfiles().profiles.value = { dorm: makeSummary() };
});

afterEach(() => {
  app?.unmount();
  app = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("TaskRecordingDialog", () => {
  it("选方案阶段：未选方案时「开始录制」禁用，选中后可点", async () => {
    mountDialog();
    await flush();
    const start = document.querySelector<HTMLButtonElement>('[data-testid="record-start"]');
    expect(start?.disabled).toBe(true);

    useTaskRecording().profileId.value = "dorm";
    await flush();
    expect(document.querySelector<HTMLButtonElement>('[data-testid="record-start"]')?.disabled).toBe(false);
  });

  it("start 409 显示固定文案「已有配置会话进行中」，不透传后端 message", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({ error: { code: "CONFLICT", message: "已存在活跃配置会话" } }, { status: 409 }),
      ),
    );
    mountDialog();
    await flush();
    useTaskRecording().profileId.value = "dorm";
    await flush(); // 等重渲染解除按钮禁用态，否则 click() 落在 disabled 按钮上无效
    click("record-start");
    await settle();
    const alert = document.querySelector('[data-testid="record-error"]');
    expect(alert?.textContent).toContain("已有配置会话进行中");
    expect(alert?.textContent).not.toContain("已存在活跃配置会话");
  });

  it("完成录制链路：apply=false stop → 预览四类步骤 + 占位符原样 → 保存载荷正确 → 弹窗关闭", async () => {
    const stopUrls: string[] = [];
    let putBody: Record<string, unknown> | null = null;
    const fetchMock = stubFetch((url, init) => {
      if (url.includes("/api/profiles/dorm/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/api/profiles/dorm/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(
          jsonResponse({
            data: { recorded_count: 5, truncated: false, persistent_session_enabled: true, recorded_actions: recordedActions() },
          }),
        );
      }
      if (url.includes("/api/tasks/untitled_1") && init?.method === "PUT") {
        putBody = JSON.parse(String(init.body)) as Record<string, unknown>;
        return Promise.resolve(jsonResponse({ data: { message: "ok" } }));
      }
      if (url.endsWith("/api/tasks")) {
        return Promise.resolve(jsonResponse({ data: [] }));
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush(); // 等重渲染解除按钮禁用态，否则 click() 落在 disabled 按钮上无效

    click("record-start");
    await settle();
    // 录制横幅出现，指引文案含占位符字面量（JS 字符串进模板，不被 Vue 插值）
    const banner = document.querySelector('[data-testid="record-banner"]');
    expect(banner?.textContent).toContain("录制进行中");
    expect(banner?.textContent).toContain("{{USERNAME}}");
    expect(banner?.textContent).toContain("{{PASSWORD}}");

    click("record-finish");
    await settle();
    // 完成录制 = apply=false 结算：取回动作数组但不写方案（任务录制对方案零副作用）
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).not.toContain("discard");
    expect(stopUrls[0]).toContain("apply=false");

    // 预览步骤列表：四类动作的中文徽标 + 占位符原样可见
    const stepsList = document.querySelector('[data-testid="record-steps"]');
    expect(stepsList?.textContent).toContain("点击");
    expect(stepsList?.textContent).toContain("输入");
    expect(stepsList?.textContent).toContain("选择");
    expect(stepsList?.textContent).toContain("跳转");
    expect(stepsList?.textContent).toContain("{{PASSWORD}}");
    expect(stepsList?.textContent).toContain("#login-submit");

    // 默认名称（录制任务 MM-DD HH:mm）与描述（注明由录制生成 + 方案名）
    const nameInput = document.querySelector<HTMLInputElement>('[data-testid="record-name"]');
    expect(nameInput?.value).toMatch(/^录制任务 \d{2}-\d{2} \d{2}:\d{2}$/);
    const descInput = document.querySelector<HTMLInputElement>('[data-testid="record-desc"]');
    expect(descInput?.value).toContain("方案「宿舍网」");

    click("record-save");
    await settle();
    expect(putBody).not.toBeNull();
    expect(putBody!.type).toBe("browser");
    expect(putBody!.task_id).toBe("untitled_1");
    expect(putBody!.url).toBe("{{LOGIN_URL}}");
    const steps = putBody!.steps as Array<Record<string, unknown>>;
    expect(steps.map((s) => s.type)).toEqual(["navigate", "input", "input", "select", "click"]);
    expect(steps[1]).toMatchObject({ selector: "#username", value: "{{USERNAME}}" });
    // 保存成功：弹窗关闭 + 成功 toast
    expect(document.querySelector('[data-testid="record-steps"]')).toBeNull();
    expect(useToast().toast.message).toContain("已保存为任务");
    expect(fetchMock).toHaveBeenCalled();
  });

  it("预览单步删除：删除首条 navigate 后保存载荷同步收缩为四步", async () => {
    let putBody: Record<string, unknown> | null = null;
    stubFetch((url, init) => {
      if (url.includes("/api/profiles/dorm/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/api/profiles/dorm/config-session/stop")) {
        return Promise.resolve(
          jsonResponse({
            data: { recorded_count: 5, truncated: false, persistent_session_enabled: false, recorded_actions: recordedActions() },
          }),
        );
      }
      if (url.includes("/api/tasks/untitled_1") && init?.method === "PUT") {
        putBody = JSON.parse(String(init.body)) as Record<string, unknown>;
        return Promise.resolve(jsonResponse({ data: { message: "ok" } }));
      }
      if (url.endsWith("/api/tasks")) {
        return Promise.resolve(jsonResponse({ data: [] }));
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush();
    click("record-start");
    await settle();
    click("record-finish");
    await settle();

    // 预览五步；删除首条（navigate）后剩四步且序号重排
    expect(document.querySelectorAll('[data-testid="record-steps"] li')).toHaveLength(5);
    const removeButtons = document.querySelectorAll<HTMLButtonElement>('[data-testid="record-step-remove"]');
    expect(removeButtons).toHaveLength(5);
    expect(removeButtons[0].getAttribute("aria-label")).toBe("删除步骤 1");
    removeButtons[0].click();
    await flush();
    expect(document.querySelectorAll('[data-testid="record-steps"] li')).toHaveLength(4);

    click("record-save");
    await settle();
    const steps = putBody!.steps as Array<Record<string, unknown>>;
    expect(steps.map((s) => s.type)).toEqual(["input", "input", "select", "click"]);
  });

  it("空录制：响应无动作数组 → 不回读方案详情，回到选方案阶段并就地提示", async () => {
    const urls: string[] = [];
    stubFetch((url) => {
      urls.push(url);
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/config-session/stop")) {
        return Promise.resolve(
          jsonResponse({ data: { recorded_count: 0, truncated: false, persistent_session_enabled: false } }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush(); // 等重渲染解除按钮禁用态，否则 click() 落在 disabled 按钮上无效
    click("record-start");
    await settle();
    click("record-finish");
    await settle();
    // apply=false 下方案未被写入，流程也不回读方案详情（读到旧录制反而是错的）
    expect(urls.every((u) => !u.startsWith("/api/profiles/dorm/") || u.includes("config-session"))).toBe(true);
    const alert = document.querySelector('[data-testid="record-error"]');
    expect(alert?.textContent).toContain("未录制到任何动作");
    // 回到选方案阶段：可重新发起
    expect(document.querySelector('[data-testid="record-start"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="record-steps"]')).toBeNull();
  });

  it("放弃录制：stop 带 discard=true（后端不落盘），回到选方案阶段", async () => {
    const stopUrls: string[] = [];
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return Promise.resolve(jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(jsonResponse({ data: { recorded_count: 0, truncated: false, persistent_session_enabled: false } }));
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush(); // 等重渲染解除按钮禁用态，否则 click() 落在 disabled 按钮上无效
    click("record-start");
    await settle();
    click("record-abandon");
    await settle();
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).toContain("discard=true");
    expect(document.querySelector('[data-testid="record-start"]')).not.toBeNull();
  });

  // ===== starting 窗口取消（P2-14）：start 在途关弹窗不得留下孤儿会话 =====

  /** 手动放行的延迟响应：模拟 Worker 冷启动数秒的 start 在途窗口 */
  function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((res) => {
      resolve = res;
    });
    return { promise, resolve };
  }

  it("start 在途关闭弹窗：立即关窗不发 stop，start 落地后补发一次 stop(discard) 收尾", async () => {
    const start = deferred<undefined>();
    const stopUrls: string[] = [];
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return start.promise.then(() => jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(
          jsonResponse({ data: { recorded_count: 0, truncated: false, persistent_session_enabled: false } }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush(); // 等重渲染解除按钮禁用态，否则 click() 落在 disabled 按钮上无效
    click("record-start");
    await settle();
    expect(rec.phase.value).toBe("starting");

    // 关闭弹窗：starting 阶段确认文案按「取消打开」语义表述
    const closePromise = rec.requestClose();
    await flush();
    const { confirmState } = useConfirm();
    expect(confirmState.visible).toBe(true);
    expect(confirmState.title).toContain("取消打开");
    useConfirm().resolveConfirm(true);
    await closePromise;

    // 弹窗即时关闭；start 仍在途时不发 stop（此刻 stop 必 409 且会落地成孤儿会话）
    expect(rec.open.value).toBe(false);
    expect(rec.phase.value).toBe("select");
    expect(stopUrls).toHaveLength(0);

    // start 落地：补发一次 stop(discard=true) 收尾，且不推进到录制态
    start.resolve(undefined);
    await settle();
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).toContain("discard=true");
    expect(rec.phase.value).toBe("select");
    expect(rec.open.value).toBe(false);
  });

  it("start 在途取消后补发 stop 撞 409（会话未落地）：静默收场，不弹失败提示", async () => {
    const stopUrls: string[] = [];
    const start = deferred<undefined>();
    stubFetch((url) => {
      if (url.includes("/config-session/start")) {
        return start.promise.then(() => jsonResponse({ data: { started: true } }));
      }
      if (url.includes("/config-session/stop")) {
        stopUrls.push(url);
        return Promise.resolve(
          jsonResponse({ error: { code: "CONFLICT", message: "无活跃配置会话" } }, { status: 409 }),
        );
      }
      return Promise.reject(new Error(`unexpected url: ${url}`));
    });
    mountDialog();
    await flush();
    const rec = useTaskRecording();
    rec.profileId.value = "dorm";
    await flush();
    click("record-start");
    await settle();

    const closePromise = rec.requestClose();
    await flush();
    useConfirm().resolveConfirm(true);
    await closePromise;
    expect(rec.open.value).toBe(false);

    start.resolve(undefined);
    await settle();
    // 补发的 stop 已发出；409 = 会话实际未落地，目的已达成 → 无失败 toast
    expect(stopUrls).toHaveLength(1);
    expect(stopUrls[0]).toContain("discard=true");
    expect(useToast().toast.message).toBe("");
    expect(rec.phase.value).toBe("select");
  });
});
