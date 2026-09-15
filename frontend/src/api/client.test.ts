/**
 * 无浏览器文案握手测试：后端 `browser::NO_BROWSER_MESSAGE`（及登录侧离线兜底
 * 后缀 `；当前无可用浏览器，…`）必须命中 `isNoBrowserMessage`，否则手动登录
 * 的弹窗引导会静默退化成普通通知。改动任一侧文案时先改这里，再同步另一侧。
 *
 * 另含 `request()` 的超时/取消/401/信封解包路径：此前该文件只测
 * `isNoBrowserMessage`，对请求层零覆盖。
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { ApiError, http, isNoBrowserMessage } from "./client";

/** 与后端 `browser::NO_BROWSER_MESSAGE` 逐字同步 */
const BACKEND_NO_BROWSER_MESSAGE =
  "当前无可用浏览器，请下载 Chromium（设置 · 浏览器页可一键安装）";

describe("isNoBrowserMessage", () => {
  it("识别后端无浏览器文案与离线兜底后缀", () => {
    expect(isNoBrowserMessage(BACKEND_NO_BROWSER_MESSAGE)).toBe(true);
    expect(
      isNoBrowserMessage(
        `环境未就绪，自动初始化失败: uv 下载失败；${BACKEND_NO_BROWSER_MESSAGE.slice(2)}`,
      ),
    ).toBe(true);
  });

  it("拒绝普通登录失败文案与非字符串", () => {
    expect(isNoBrowserMessage("配置不完整: username, password")).toBe(false);
    expect(isNoBrowserMessage("auth_url 不可达: http://127.0.0.1:9/")).toBe(false);
    expect(isNoBrowserMessage("登录成功")).toBe(false);
    expect(isNoBrowserMessage("")).toBe(false);
    expect(isNoBrowserMessage(null)).toBe(false);
    expect(isNoBrowserMessage(undefined)).toBe(false);
    expect(isNoBrowserMessage(42)).toBe(false);
  });
});

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

/**
 * 构造「响应头已回、body 永久挂起」的响应。
 *
 * 这正是 P2-9 的场景：旧的 `finally` 在收到响应头后就清掉了超时定时器与
 * abort 监听，此后的 `res.json()` 既无超时也不可取消 → 永久 pending。
 * 此处让 `json()` 在 signal abort 时以 AbortError 拒绝，用于验证
 * 超时/取消确实覆盖到 body 解析阶段。
 */
function hangingBodyResponse(signal: AbortSignal): Response {
  return {
    ok: true,
    status: 200,
    headers: new Headers({ "content-type": "application/json" }),
    json: () =>
      new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () =>
          reject(Object.assign(new Error("aborted"), { name: "AbortError" })),
        );
      }),
    text: async () => "",
  } as unknown as Response;
}

/** 安装 fetch 桩：token 端点返回固定 token，业务端点走 handler */
function stubFetch(handler: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) => {
    if (String(url).includes("/api/auth/token")) {
      return jsonResponse({ data: { token: "test-token" } });
    }
    return handler(String(url), init);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("request 超时与取消", () => {
  it("响应体挂起时超时仍生效（不永久 pending）", async () => {
    stubFetch((_url, init) => hangingBodyResponse(init!.signal as AbortSignal));
    const err = await http.get("/api/hang", { timeout: 30 }).catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).message).toBe("请求超时");
  });

  it("调用方取消时覆盖响应体阶段，并标记 aborted 供调用方静默", async () => {
    stubFetch((_url, init) => hangingBodyResponse(init!.signal as AbortSignal));
    const controller = new AbortController();
    const pending = http.get("/api/hang", { timeout: 60000, signal: controller.signal });
    // 等 fetch 返回、进入 body 解析阶段后再取消
    await new Promise((r) => setTimeout(r, 5));
    controller.abort();
    const err = await pending.catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).message).toBe("请求已取消");
    expect((err as ApiError).aborted).toBe(true);
  });

  it("成功后解包 { data } 信封", async () => {
    stubFetch(() => jsonResponse({ data: { value: 7 } }));
    await expect(http.get("/api/x")).resolves.toEqual({ value: 7 });
  });

  it("非 2xx 抛 ApiError，携带错误信封的 code 与 message", async () => {
    stubFetch(() =>
      jsonResponse({ error: { code: "WORKER_BUSY", message: "Worker 忙" } }, { status: 409 }),
    );
    const err = await http.post("/api/x", {}).catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(409);
    expect((err as ApiError).code).toBe("WORKER_BUSY");
    expect((err as ApiError).message).toBe("Worker 忙");
  });

  it("401 时重置 token 并仅重试一次", async () => {
    let calls = 0;
    const fetchMock = stubFetch(() => {
      calls += 1;
      // 首次 401，重试后 200
      return calls === 1 ? jsonResponse({ error: {} }, { status: 401 }) : jsonResponse({ data: "ok" });
    });
    await expect(http.get("/api/x")).resolves.toBe("ok");
    // token 请求 + 首次业务请求 + 重试 = 3 次
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});
