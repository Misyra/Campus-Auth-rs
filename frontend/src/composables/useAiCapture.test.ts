/**
 * useAiCapture 单测：AI 任务页第 2 步捕获的关键分支。
 * 覆盖：capture 前置校验/成功落结果与截图地址/失败兜底、saveBundle 打包下载文件名
 * 与重入守卫/失败兜底、restoreCapture 产物可用恢复/不可用跳过/失败静默、
 * loadBrowserChannel 渠道读取与 captureChannelDegraded 判定（firefox/webkit/custom 引擎）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const { aiApiMock, configApiMock, toastOnlyMock, downloadBlobMock } = vi.hoisted(() => ({
  aiApiMock: {
    capture: vi.fn(),
    captureScreenshotUrl: vi.fn(() => "shot?t=1"),
    captureStatus: vi.fn(),
    captureBundle: vi.fn(),
  },
  configApiMock: { fetch: vi.fn() },
  toastOnlyMock: vi.fn(),
  downloadBlobMock: vi.fn(),
}));

vi.mock("../api", () => ({ aiApi: aiApiMock, configApi: configApiMock }));
vi.mock("../utils/file", () => ({ downloadBlob: downloadBlobMock }));

const { useAiCapture } = await import("./useAiCapture");

function make() {
  return useAiCapture({ toastOnly: toastOnlyMock });
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("capture", () => {
  it("未填地址直接提示且不发请求", async () => {
    const ai = make();
    await ai.capture();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "请输入登录页地址");
    expect(aiApiMock.capture).not.toHaveBeenCalled();
    expect(ai.capturing.value).toBe(false);
  });

  it("捕获成功：落结果与截图地址并提示", async () => {
    aiApiMock.capture.mockResolvedValue({ final_url: "http://portal/", resources_count: 3 });
    const ai = make();
    ai.captureUrl.value = " http://start/ ";
    await ai.capture();
    expect(aiApiMock.capture).toHaveBeenCalledWith("http://start/");
    expect(ai.captureResult.value).toEqual({ final_url: "http://portal/", resources_count: 3 });
    expect(ai.screenshotUrl.value).toBe("shot?t=1");
    expect(ai.isCaptureDone.value).toBe(true);
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "页面捕获完成");
    expect(ai.capturing.value).toBe(false);
  });

  it("捕获失败：清空旧结果、走失败提示并复位 capturing", async () => {
    aiApiMock.capture.mockRejectedValue(new Error("boom"));
    const ai = make();
    ai.captureUrl.value = "http://start/";
    await ai.capture();
    expect(ai.captureResult.value).toBeNull();
    expect(ai.isCaptureDone.value).toBe(false);
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
    expect(ai.capturing.value).toBe(false);
  });
});

describe("saveBundle", () => {
  it("打包成功：按时间戳文件名下载 zip", async () => {
    const blob = new Blob(["zip"]);
    aiApiMock.captureBundle.mockResolvedValue(blob);
    const ai = make();
    await ai.saveBundle();
    expect(downloadBlobMock).toHaveBeenCalledTimes(1);
    const [calledBlob, filename, mime] = downloadBlobMock.mock.calls[0];
    expect(calledBlob).toBe(blob);
    expect(filename).toMatch(/^campus-auth-capture-\d{14}\.zip$/);
    expect(mime).toBe("application/zip");
    expect(toastOnlyMock).toHaveBeenCalledWith(true, "页面文件已保存");
    expect(ai.savingBundle.value).toBe(false);
  });

  it("重入守卫：进行中重复调用直接返回，不并发拉包", async () => {
    let release: (b: Blob) => void = () => {};
    aiApiMock.captureBundle.mockImplementation(() => new Promise<Blob>((resolve) => { release = resolve; }));
    const ai = make();
    const first = ai.saveBundle();
    const second = ai.saveBundle();
    release(new Blob(["zip"]));
    await Promise.all([first, second]);
    expect(aiApiMock.captureBundle).toHaveBeenCalledTimes(1);
    expect(ai.savingBundle.value).toBe(false);
  });

  it("打包失败走失败提示并复位 savingBundle", async () => {
    aiApiMock.captureBundle.mockRejectedValue(new Error("boom"));
    const ai = make();
    await ai.saveBundle();
    expect(toastOnlyMock).toHaveBeenCalledWith(false, "boom");
    expect(ai.savingBundle.value).toBe(false);
  });
});

describe("restoreCapture", () => {
  it("产物可用：恢复捕获元数据与截图地址", async () => {
    aiApiMock.captureStatus.mockResolvedValue({
      available: true, final_url: "http://portal/", title: "认证页",
      structure_summary: { frames: 2, forms: 1, controls: 5, captcha_candidates: 1 },
    });
    const ai = make();
    await ai.restoreCapture();
    expect(ai.captureResult.value).toEqual({
      final_url: "http://portal/", title: "认证页",
      structure_summary: { frames: 2, forms: 1, controls: 5, captcha_candidates: 1 },
    });
    expect(ai.screenshotUrl.value).toBe("shot?t=1");
    expect(ai.isCaptureDone.value).toBe(true);
  });

  it("产物不可用：保持未捕获状态", async () => {
    aiApiMock.captureStatus.mockResolvedValue({ available: false });
    const ai = make();
    await ai.restoreCapture();
    expect(ai.captureResult.value).toBeNull();
    expect(ai.isCaptureDone.value).toBe(false);
  });

  it("状态查询失败静默忽略，不阻塞页面", async () => {
    aiApiMock.captureStatus.mockRejectedValue(new Error("boom"));
    const ai = make();
    await expect(ai.restoreCapture()).resolves.toBeUndefined();
    expect(ai.captureResult.value).toBeNull();
  });
});

describe("loadBrowserChannel / captureChannelDegraded", () => {
  it("firefox / webkit 渠道判定为降级", async () => {
    configApiMock.fetch.mockResolvedValue({ browser: { browser_channel: "firefox" } });
    const ai = make();
    await ai.loadBrowserChannel();
    expect(ai.browserChannel.value).toBe("firefox");
    expect(ai.captureChannelDegraded.value).toBe(true);
  });

  it("custom 渠道按自定义引擎判定（webkit 降级，chromium 不降级）", async () => {
    configApiMock.fetch.mockResolvedValue({ browser: { browser_channel: "custom", custom_browser_engine: "webkit" } });
    const ai = make();
    await ai.loadBrowserChannel();
    expect(ai.captureChannelDegraded.value).toBe(true);
    configApiMock.fetch.mockResolvedValue({ browser: { browser_channel: "custom", custom_browser_engine: "chromium" } });
    await ai.loadBrowserChannel();
    expect(ai.captureChannelDegraded.value).toBe(false);
  });

  it("chromium 渠道不降级", async () => {
    configApiMock.fetch.mockResolvedValue({ browser: { browser_channel: "chromium" } });
    const ai = make();
    await ai.loadBrowserChannel();
    expect(ai.captureChannelDegraded.value).toBe(false);
  });

  it("配置读取失败：渠道清空、降级提示消失", async () => {
    configApiMock.fetch.mockRejectedValue(new Error("boom"));
    const ai = make();
    await ai.loadBrowserChannel();
    expect(ai.browserChannel.value).toBe("");
    expect(ai.captureChannelDegraded.value).toBe(false);
  });
});
