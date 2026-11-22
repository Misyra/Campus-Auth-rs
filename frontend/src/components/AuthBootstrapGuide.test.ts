// @vitest-environment happy-dom
/**
 * AuthBootstrapGuide 引导页行为测试（createApp 直挂，免 @vue/test-utils，
 * 与 ConfirmDialog.focus.test.ts 同范式）。
 *
 * 锁定两种模式的分流与关键分支：
 * - 本地（回环）：托盘指引文案 + 粘贴框（接受完整 URL / 裸秘钥，走 bootstrap 交换）；
 * - 远程（非回环）：密码表单，提交成功刷新恢复；密码错误 / AUTH_NOT_CONFIGURED
 *   两类失败的就地提示（后者必须给出服务端配置指引）。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createApp, nextTick, type App } from "vue";
import AuthBootstrapGuide from "./AuthBootstrapGuide.vue";

const BOOTSTRAP_SECRET = "a".repeat(64);

/** 构造 JSON 响应 */
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
let reloadMock: ReturnType<typeof vi.fn<() => void>>;

function mount(): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(AuthBootstrapGuide);
  app.mount(container);
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：提交是异步 fetch + 组件状态刷新，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

function setInput(selector: string, value: string): void {
  const input = document.querySelector<HTMLInputElement>(selector);
  expect(input, `找不到输入框 ${selector}`).not.toBeNull();
  input!.value = value;
  input!.dispatchEvent(new Event("input"));
}

function clickButton(text: string): void {
  const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>("button"));
  const target = buttons.find((b) => b.textContent?.includes(text));
  expect(target, `找不到按钮「${text}」`).not.toBeNull();
  target!.click();
}

/** 切换 happy-dom 的访问来源（本地回环 ↔ 远程主机） */
function setUrl(url: string): void {
  (window as unknown as { happyDOM?: { setURL?: (url: string) => void } }).happyDOM?.setURL?.(url);
}

beforeEach(() => {
  document.body.innerHTML = "";
  sessionStorage.clear();
  location.hash = "";
  reloadMock = vi.fn();
  vi.spyOn(window.location, "reload").mockImplementation(reloadMock);
});

afterEach(() => {
  app?.unmount();
  app = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("AuthBootstrapGuide 本地模式（回环访问）", () => {
  it("展示获取凭据的三种途径与粘贴框，不出现密码表单", () => {
    mount();
    const text = document.body.textContent ?? "";
    expect(text).toContain("系统托盘");
    expect(text).toContain("打开界面");
    expect(text).toContain("复制界面链接");
    // 无托盘/无桌面场景：终端启动输出里的整行入口；以及数据目录下的秘钥文件
    expect(text).toContain("界面入口");
    expect(text).toContain("config/.auth_bootstrap");
    expect(document.querySelector("#auth-bootstrap-key")).not.toBeNull();
    expect(document.querySelector("#auth-bootstrap-password")).toBeNull();
  });

  it("后端已启动时点「重新检查」重走正常启动路径", () => {
    mount();
    clickButton("重新检查");
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it("粘贴完整带 #auth= 的 URL：按 bootstrap 交换并在成功后刷新恢复", async () => {
    const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => {
      expect(String(url)).toContain("/api/auth/exchange");
      expect(JSON.parse(String(init?.body))).toEqual({ bootstrap: BOOTSTRAP_SECRET });
      return jsonResponse({ data: { token: "tok-1" } });
    });
    vi.stubGlobal("fetch", fetchMock);
    mount();

    setInput("#auth-bootstrap-key", `http://127.0.0.1:50721/#auth=${BOOTSTRAP_SECRET}`);
    clickButton("引导");
    await settle();

    expect(reloadMock).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem("campus_auth_token")).toBe("tok-1");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("无法识别的粘贴内容就地报错，不发请求", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    mount();

    setInput("#auth-bootstrap-key", "不是秘钥的文本");
    clickButton("引导");
    await settle();

    const alert = document.querySelector<HTMLElement>("[role='alert']");
    expect(alert?.textContent).toContain("无法识别引导信息");
    expect(fetchMock).not.toHaveBeenCalled();
    expect(reloadMock).not.toHaveBeenCalled();
  });
});

describe("AuthBootstrapGuide 远程模式（非回环访问）", () => {
  it("展示密码表单与密码来源说明，不出现托盘指引", () => {
    setUrl("http://nas.example.com:50721/");
    mount();
    const text = document.body.textContent ?? "";
    expect(document.querySelector("#auth-bootstrap-password")).not.toBeNull();
    expect(text).not.toContain("系统托盘菜单");
    // 密码从哪来：启动参数 / 环境变量 / 服务端设置页
    expect(text).toContain("--management-password");
    expect(text).toContain("CAMPUS_AUTH_MANAGEMENT_PASSWORD");
  });

  it("密码提交成功：换取 token、写缓存并刷新恢复", async () => {
    setUrl("http://nas.example.com:50721/");
    const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => {
      expect(String(url)).toContain("/api/auth/exchange");
      expect(JSON.parse(String(init?.body))).toEqual({ password: "pw-123" });
      return jsonResponse({ data: { token: "tok-2" } });
    });
    vi.stubGlobal("fetch", fetchMock);
    mount();

    setInput("#auth-bootstrap-password", "pw-123");
    clickButton("解锁");
    await settle();

    expect(reloadMock).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem("campus_auth_token")).toBe("tok-2");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("密码错误：就地提示后端中文消息，不刷新", async () => {
    setUrl("http://nas.example.com:50721/");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(
          { error: { code: "BAD_PASSWORD", message: "管理密码不正确" } },
          { status: 401 },
        ),
      ),
    );
    mount();

    setInput("#auth-bootstrap-password", "wrong");
    clickButton("解锁");
    await settle();

    const alert = document.querySelector<HTMLElement>("[role='alert']");
    expect(alert?.textContent).toContain("管理密码不正确");
    expect(reloadMock).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("campus_auth_token")).toBeNull();
  });

  it("AUTH_NOT_CONFIGURED：给出服务端配置指引（含环境变量注入路径）", async () => {
    setUrl("http://nas.example.com:50721/");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(
          { error: { code: "AUTH_NOT_CONFIGURED", message: "未配置管理密码" } },
          { status: 401 },
        ),
      ),
    );
    mount();

    setInput("#auth-bootstrap-password", "pw-123");
    clickButton("解锁");
    await settle();

    const alert = document.querySelector<HTMLElement>("[role='alert']");
    expect(alert?.textContent).toContain("CAMPUS_AUTH_MANAGEMENT_PASSWORD");
    expect(alert?.textContent).toContain("management_password");
    expect(reloadMock).not.toHaveBeenCalled();
  });
});
