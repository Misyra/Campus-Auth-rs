// @vitest-environment happy-dom
/**
 * 鉴权引导（bootstrap 握手）与请求层测试。
 *
 * 背景：免鉴权的 GET /api/auth/token 已移除，token 只能经 POST /api/auth/exchange
 * 换取——凭据为 URL fragment 的引导秘钥（`#auth=<hex>`，托盘打开界面时携带）或
 * 管理密码（远程访问引导页）。本文件锁定：
 * - hash 秘钥交换成功流（两级缓存写入 + history.replaceState 剥除 fragment）；
 * - 交换 401 失败走引导态（fragment 保留、不写缓存）；
 * - 无 hash 时 sessionStorage 缓存复用（同 tab 刷新不再依赖 fragment）；
 * - submitManagementPassword 的 AUTH_NOT_CONFIGURED 透传与密码错误/网络失败分支；
 * - refreshAuthToken 与 resetAuthToken 的语义分界：WS 重连刷新仅清内存缓存
 *   （sessionStorage 保留、复用同一 token），401 证实的失效才双层全清；
 * - 引导交换用裸 fetch 直连（不经 request()：会因 ensureAuthToken 递归，且
 *   request 的 401 重试/信封语义不适用于引导端点）。
 *
 * 另含 `isNoBrowserMessage` 文案握手与 `request()` 的超时/取消/401/信封解包路径。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { ApiError, http, isNoBrowserMessage, ensureAuthToken, hasUsableAuthToken, refreshAuthToken, submitManagementPassword, parseBootstrapSecret } from "./client";

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

/** 安装 fetch 桩：所有 URL（含 /api/auth/exchange）一律走 handler */
function stubFetch(handler: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  const fetchMock = vi.fn(async (url: string | URL, init?: RequestInit) =>
    handler(String(url), init),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** 测试秘钥（64 位 hex） */
const BOOTSTRAP_SECRET = "a".repeat(64);

beforeEach(async () => {
  // 清空三类凭据来源，保证每条用例从「干净引导态」出发：
  // fragment / sessionStorage 直接清；内存缓存经 refreshAuthToken 重置
  //（新语义下它保留 sessionStorage，但此前已 clear，同样为空；
  // 无 hash 无缓存时它不发任何请求）
  sessionStorage.clear();
  location.hash = "";
  await refreshAuthToken();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ensureAuthToken bootstrap 握手", () => {
  it("hash 秘钥交换成功：写两级缓存、剥除 fragment、后续命中内存缓存", async () => {
    location.hash = `#auth=${BOOTSTRAP_SECRET}`;
    const fetchMock = stubFetch((url, init) => {
      expect(String(url)).toContain("/api/auth/exchange");
      // 裸 fetch 直连：不得携带业务请求的鉴权头；axum Json 提取器要求显式 Content-Type
      expect(init?.method).toBe("POST");
      expect((init?.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
      expect((init?.headers as Record<string, string>)["X-Auth-Token"]).toBeUndefined();
      expect(JSON.parse(String(init?.body))).toEqual({ bootstrap: BOOTSTRAP_SECRET });
      return jsonResponse({ data: { token: "tok-1" } });
    });

    await expect(ensureAuthToken()).resolves.toBe("tok-1");
    // fragment 已剥除（防刷新/分享泄露），sessionStorage 已写入
    expect(location.hash).toBe("");
    expect(sessionStorage.getItem("campus_auth_token")).toBe("tok-1");
    // 第二次调用命中内存缓存：不再发请求
    await expect(ensureAuthToken()).resolves.toBe("tok-1");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("hash 交换 401 失败：返回 null 走引导态，fragment 保留、不写缓存", async () => {
    location.hash = `#auth=${BOOTSTRAP_SECRET}`;
    stubFetch(() =>
      jsonResponse(
        { error: { code: "BOOTSTRAP_INVALID", message: "引导秘钥无效" } },
        { status: 401 },
      ),
    );
    await expect(ensureAuthToken()).resolves.toBeNull();
    expect(location.hash).toBe(`#auth=${BOOTSTRAP_SECRET}`);
    expect(sessionStorage.getItem("campus_auth_token")).toBeNull();
  });

  it("无 hash 时复用 sessionStorage 缓存，且不发任何请求", async () => {
    sessionStorage.setItem("campus_auth_token", "tok-2");
    const fetchMock = stubFetch(() => jsonResponse({ data: { token: "不应被取用" } }));
    await expect(ensureAuthToken()).resolves.toBe("tok-2");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("无 hash 无缓存时返回 null（引导态），anonymous 请求兜底", async () => {
    const fetchMock = stubFetch(() => jsonResponse({ data: { token: "不应被取用" } }));
    await expect(ensureAuthToken()).resolves.toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("campus_auth_token")).toBeNull();
  });

  it("parseBootstrapSecret：接受完整带 #auth= 的 URL 与裸秘钥，拒绝非法输入", () => {
    const short = "abcdef01";
    expect(parseBootstrapSecret(`http://127.0.0.1:50721/#auth=${BOOTSTRAP_SECRET}`)).toBe(
      BOOTSTRAP_SECRET,
    );
    expect(parseBootstrapSecret(`#auth=${BOOTSTRAP_SECRET}`)).toBe(BOOTSTRAP_SECRET);
    expect(parseBootstrapSecret(BOOTSTRAP_SECRET)).toBe(BOOTSTRAP_SECRET);
    expect(parseBootstrapSecret(short)).toBe(short); // 8~128 位 hex 均接受
    expect(parseBootstrapSecret("short")).toBeNull();
    expect(parseBootstrapSecret("#auth=xyz")).toBeNull();
    expect(parseBootstrapSecret("http://127.0.0.1:50721/")).toBeNull();
    expect(parseBootstrapSecret("")).toBeNull();
    expect(parseBootstrapSecret("  ")).toBeNull();
  });
});

describe("refreshAuthToken 与 resetAuthToken 的语义分界", () => {
  it("WS 重连刷新仅清内存缓存：保留 sessionStorage 并复用同一 token", async () => {
    // 模拟同 tab 已引导过的状态：token 同时在 sessionStorage（后端持久化的镜像）
    sessionStorage.setItem("campus_auth_token", "tok-persist");
    await expect(ensureAuthToken()).resolves.toBe("tok-persist");
    const fetchMock = stubFetch(() => jsonResponse({ data: { token: "不应被取用" } }));
    // WS 重连前的刷新只清内存：sessionStorage 保留 → 恢复出同一 token，不发任何请求。
    // 后端重启场景下旧 token 依然有效，页面不被迫回到引导页
    await expect(refreshAuthToken()).resolves.toBe("tok-persist");
    expect(fetchMock).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("campus_auth_token")).toBe("tok-persist");
    // 恢复结果对后续请求可见
    await expect(ensureAuthToken()).resolves.toBe("tok-persist");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("401 触发的 reset 双层全清：内存与 sessionStorage 一并失效，随后匿名重试", async () => {
    sessionStorage.setItem("campus_auth_token", "tok-stale");
    const fetchMock = stubFetch(() => jsonResponse({ error: {} }, { status: 401 }));
    const err = await http.get("/api/x").catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(401);
    // 原始（带 token）+ reset 后的匿名重试，共 2 次
    expect(fetchMock).toHaveBeenCalledTimes(2);
    // 401 说明凭据被吊销/轮换：双层缓存必须一并废弃，交回引导态
    expect(sessionStorage.getItem("campus_auth_token")).toBeNull();
    expect(hasUsableAuthToken()).toBe(false);
  });
});

describe("submitManagementPassword", () => {
  it("密码换取成功：缓存 token（内存 + sessionStorage）并剥除残留 fragment", async () => {
    location.hash = `#auth=${BOOTSTRAP_SECRET}`;
    const fetchMock = stubFetch((url, init) => {
      expect(String(url)).toContain("/api/auth/exchange");
      expect(JSON.parse(String(init?.body))).toEqual({ password: "pw-123" });
      return jsonResponse({ data: { token: "tok-3" } });
    });
    const outcome = await submitManagementPassword("pw-123");
    expect(outcome).toEqual({ ok: true, token: "tok-3" });
    expect(sessionStorage.getItem("campus_auth_token")).toBe("tok-3");
    expect(location.hash).toBe("");
    // 缓存对后续 ensureAuthToken 可见
    await expect(ensureAuthToken()).resolves.toBe("tok-3");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("AUTH_NOT_CONFIGURED 原样透传（服务端未配置管理密码）", async () => {
    stubFetch(() =>
      jsonResponse(
        { error: { code: "AUTH_NOT_CONFIGURED", message: "未配置管理密码" } },
        { status: 401 },
      ),
    );
    const outcome = await submitManagementPassword("pw-123");
    expect(outcome).toEqual({
      ok: false,
      code: "AUTH_NOT_CONFIGURED",
      message: "未配置管理密码",
    });
    expect(sessionStorage.getItem("campus_auth_token")).toBeNull();
  });

  it("其他 401 报密码错误（带后端中文消息）", async () => {
    stubFetch(() =>
      jsonResponse({ error: { code: "BAD_PASSWORD", message: "管理密码不正确" } }, { status: 401 }),
    );
    const outcome = await submitManagementPassword("wrong");
    expect(outcome).toEqual({ ok: false, code: "AUTH_REJECTED", message: "管理密码不正确" });
  });

  it("网络失败报 NETWORK_ERROR（中文提示）", async () => {
    stubFetch(() => {
      throw new TypeError("network down");
    });
    const outcome = await submitManagementPassword("pw-123");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.code).toBe("NETWORK_ERROR");
      expect(outcome.message).toContain("无法连接服务");
    }
  });
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

  it("401 时重置 token 并仅重试一次；无凭据可用时以匿名请求失败", async () => {
    // 新信任模型下没有免鉴权取 token 的端点：401 重试仍拿不到凭据（无 hash 无缓存），
    // 第二次仍匿名 → 401 → 抛出；且重试路径不得触发 bootstrap 交换
    const fetchMock = stubFetch(() => jsonResponse({ error: {} }, { status: 401 }));
    const err = await http.get("/api/x").catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(401);
    // 业务请求 + 401 重试 = 2 次；ensureAuthToken 无凭据来源不发起交换
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("交换成功剥除 fragment 后，401 重试不再有凭据来源（匿名重试）", async () => {
    // 首个业务请求带着交换来的 token：401 → reset → 重试时 fragment 已剥除、
    // 缓存已清，ensureAuthToken 返回 null → 匿名重试一次。此处锁定
    // 「交换仅一次 + 重试仅一次」的防循环边界
    location.hash = `#auth=${BOOTSTRAP_SECRET}`;
    let businessCalls = 0;
    const fetchMock = stubFetch((url) => {
      if (String(url).includes("/api/auth/exchange")) {
        return jsonResponse({ data: { token: "tok-4" } });
      }
      businessCalls += 1;
      return businessCalls === 1
        ? jsonResponse({ error: {} }, { status: 401 })
        : jsonResponse({ data: "ok" });
    });
    await expect(http.get("/api/x")).resolves.toBe("ok");
    expect(location.hash).toBe("");
    // 交换 1 次 + 业务 2 次（原始 + 重试）
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});
