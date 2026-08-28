import { describe, expect, it, vi, beforeEach } from "vitest";

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
}));

const { browsersApi } = await import("./index");
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
