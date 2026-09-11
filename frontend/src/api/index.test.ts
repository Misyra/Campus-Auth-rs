import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./client", () => ({
  http: {
    get: vi.fn(),
    post: vi.fn(),
    patch: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
  extractApiError: vi.fn(),
  ensureAuthToken: vi.fn().mockResolvedValue(""),
}));

const { aiApi, browsersApi, systemApi } = await import("./index");
const { http } = await import("./client");

const post = vi.mocked(http.post);

beforeEach(() => {
  post.mockReset();
  post.mockResolvedValue({} as never);
});

describe("browsersApi.installPlaywright", () => {
  it("省略浏览器时保持 Chromium 兼容默认值", async () => {
    await browsersApi.installPlaywright();
    expect(post).toHaveBeenCalledWith(
      "/api/install/playwright?browser=chromium",
      null,
      undefined,
    );
  });

  it.each(["firefox", "webkit"])("把 %s 作为显式 Playwright 安装目标", async (browser) => {
    const opts = { timeout: 1234 };
    await browsersApi.installPlaywright(browser, opts);
    expect(post).toHaveBeenCalledWith(
      `/api/install/playwright?browser=${browser}`,
      null,
      opts,
    );
  });
});

afterEach(() => vi.unstubAllGlobals());

describe("systemApi.restart", () => {
  it("调用专用重启端点而不是 shutdown", async () => {
    await systemApi.restart();
    expect(post).toHaveBeenCalledWith("/api/system/restart");
  });
});

function sseResponse(frames: string): Response {
  const bytes = new TextEncoder().encode(frames);
  return new Response(new ReadableStream({
    start(controller) {
      controller.enqueue(bytes);
      controller.close();
    },
  }), { status: 200, headers: { "content-type": "text/event-stream" } });
}

describe("aiApi.generateStream", () => {
  it("收到 done 终态后正常完成并转发事件", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(sseResponse(
      'data: {"type":"started"}\n\ndata: {"type":"done","task":{}}\n\n',
    )));
    const events: Array<Record<string, unknown>> = [];
    await aiApi.generateStream({}, { onEvent: (event) => events.push(event), idleTimeoutMs: 1000 });
    expect(events.map((event) => event.type)).toEqual(["started", "done"]);
  });

  it("连接结束但没有 done/error 时明确报错", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(sseResponse(
      'data: {"type":"started"}\n\n',
    )));
    await expect(aiApi.generateStream({}, { onEvent: () => undefined, idleTimeoutMs: 1000 }))
      .rejects.toThrow("未收到完成状态");
  });
});
