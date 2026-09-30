/** 背景请求的并发守卫和失败恢复，避免旧清理覆盖新图片或重复下载。 */
import { beforeEach, describe, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  api: { upload: vi.fn(), fetchUrl: vi.fn(), remove: vi.fn() },
  pickFile: vi.fn(), toast: vi.fn(),
  appearance: { background_url: "", background_filename: "", wallpaper_api_url: "" },
}));
vi.mock("../api", () => ({ backgroundApi: mocks.api }));
vi.mock("../utils/file", () => ({ pickFile: mocks.pickFile }));
vi.mock("./useToast", () => ({ useToast: () => ({ toastOnly: mocks.toast }) }));
vi.mock("./useAppearance", () => ({ useAppearance: () => ({ appearance: mocks.appearance, applyAppearance: vi.fn() }) }));
const { useBackgroundImage } = await import("./useBackgroundImage");
const bg = useBackgroundImage();
beforeEach(() => {
  vi.resetAllMocks();
  Object.assign(mocks.appearance, { background_url: "", background_filename: "", wallpaper_api_url: "" });
  Object.assign(bg.randomWallpaperDialog, { visible: false, loading: false, url: "" });
});

describe("背景请求恢复", () => {
  it.each(["file:///image.png", "data:image/png,abc", "invalid"])("拒绝无效下载链接 %s", async (url) => {
    bg.randomWallpaperDialog.url = url;
    await bg.confirmRandomWallpaper();
    expect(mocks.api.fetchUrl).not.toHaveBeenCalled();
    expect(bg.randomWallpaperDialog.loading).toBe(false);
  });
  it("在途下载拒绝重复确认和关闭，完成后自动应用", async () => {
    let resolve!: (value: unknown) => void;
    mocks.api.fetchUrl.mockImplementation(() => new Promise((done) => { resolve = done; }));
    bg.openRandomWallpaperDialog();
    bg.randomWallpaperDialog.url = "https://example.com/image.png";
    const pending = bg.confirmRandomWallpaper();
    await bg.confirmRandomWallpaper();
    bg.closeRandomWallpaperDialog();
    expect(bg.randomWallpaperDialog.visible).toBe(true);
    expect(mocks.api.fetchUrl).toHaveBeenCalledTimes(1);
    resolve({ filename: "image.png", url: "/api/background/image.png" });
    await pending;
    expect(mocks.appearance.background_url).toBe("/api/background/image.png");
    expect(bg.randomWallpaperDialog.loading).toBe(false);
    expect(bg.randomWallpaperDialog.visible).toBe(false);
  });
  it("移除图片期间不允许新上传或重复移除，失败仍恢复状态", async () => {
    let reject!: (error: Error) => void;
    mocks.api.remove.mockImplementation(() => new Promise((_, fail) => { reject = fail; }));
    Object.assign(mocks.appearance, { background_url: "/api/background/old.png", background_filename: "old.png" });
    const pending = bg.clearBackgroundImage();
    await bg.clearBackgroundImage();
    await bg.selectBackgroundImage();
    expect(mocks.api.remove).toHaveBeenCalledTimes(1);
    expect(mocks.pickFile).not.toHaveBeenCalled();
    reject(new Error("failed"));
    await pending;
    expect(bg.removing.value).toBe(false);
    expect(mocks.appearance.background_url).toBe("");
  });
  it("取消选图会释放状态，下次可以正常打开", async () => {
    mocks.pickFile.mockResolvedValue(null);
    await bg.selectBackgroundImage();
    await bg.selectBackgroundImage();
    expect(bg.uploading.value).toBe(false);
    expect(mocks.pickFile).toHaveBeenCalledTimes(2);
    expect(mocks.api.upload).not.toHaveBeenCalled();
  });
});
