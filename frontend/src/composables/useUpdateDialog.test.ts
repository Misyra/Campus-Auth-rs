/**
 * 更新弹窗状态机（单例）的单元测试。
 *
 * 覆盖三条触发路径的"该不该弹"判定与「稍后提醒」的版本记忆。模块级单例，
 * 用例按声明顺序执行，每个用例先复位开关与检查结果。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const checkUpdateMock = vi.fn();

vi.mock("../api", () => ({
  systemApi: {
    checkUpdate: () => checkUpdateMock(),
    update: vi.fn(async () => ({ message: "更新已就绪" })),
    restart: vi.fn(async () => ({})),
  },
  monitorApi: { start: vi.fn(), stop: vi.fn() },
  autostartApi: { status: vi.fn(), set: vi.fn() },
}));

// node 测试环境没有 localStorage：用内存替身，顺带断言 snooze 记下的版本号
const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (key: string) => store.get(key) ?? null,
  setItem: (key: string, value: string) => void store.set(key, value),
  removeItem: (key: string) => void store.delete(key),
  clear: () => store.clear(),
});

const { useUpdateDialog } = await import("./useUpdateDialog");

const update = useUpdateDialog();

/** 后端 `GET /api/check-update` 的响应形状（只保留弹窗用到的字段） */
function updateInfo(latest = "9.9.9") {
  return {
    has_update: true,
    latest,
    current: "5.0.2",
    notes: "## 更新日志\n\n- 修了一处问题",
    release_date: "2026-09-20T00:00:00Z",
    url: "https://example.com/campus-auth.zip",
    sha256: "abc",
    size: 1024,
  };
}

beforeEach(() => {
  update.dismiss();
  update.state.info = null;
  update.state.error = "";
  update.state.applying = false;
  store.clear();
  checkUpdateMock.mockReset();
});

describe("自动弹窗（启动检查与周期检查共用）", () => {
  it("命中新版本 → 弹窗并带上检查结果", async () => {
    checkUpdateMock.mockResolvedValue(updateInfo());
    await update.checkAndMaybeOpen();
    expect(update.state.open).toBe(true);
    expect(update.state.info?.latest).toBe("9.9.9");
  });

  it("已是最新 → 不弹窗", async () => {
    checkUpdateMock.mockResolvedValue({ has_update: false, latest: "5.0.2" });
    await update.checkAndMaybeOpen();
    expect(update.state.open).toBe(false);
  });

  it("检查失败 → 不弹窗且记录错误", async () => {
    checkUpdateMock.mockRejectedValue(new Error("网络不可达"));
    await update.checkAndMaybeOpen();
    expect(update.state.open).toBe(false);
    expect(update.state.error).toBe("网络不可达");
  });

  it("启动检查与周期检查同时命中：只请求一次、只弹一次", async () => {
    checkUpdateMock.mockResolvedValue(updateInfo());
    await Promise.all([update.checkAndMaybeOpen(), update.checkAndMaybeOpen()]);
    expect(checkUpdateMock).toHaveBeenCalledTimes(1);
    expect(update.state.open).toBe(true);
  });
});

describe("稍后提醒", () => {
  it("记住版本号后同版本不再自动弹，手动打开仍可查看", async () => {
    checkUpdateMock.mockResolvedValue(updateInfo("9.9.9"));
    await update.checkAndMaybeOpen();
    update.snooze();

    expect(update.state.open).toBe(false);
    expect(store.get("campus-auth.update-dialog-snoozed")).toBe("9.9.9");

    await update.checkAndMaybeOpen();
    expect(update.state.open).toBe(false);

    await update.openDialog();
    expect(update.state.open).toBe(true);
  });

  it("出现更高版本时不受旧版本记忆影响", async () => {
    store.set("campus-auth.update-dialog-snoozed", "9.9.9");
    checkUpdateMock.mockResolvedValue(updateInfo("9.9.10"));
    await update.checkAndMaybeOpen();
    expect(update.state.open).toBe(true);
  });
});

describe("手动入口", () => {
  it("弹窗已打开时自动路径直接返回，不重复请求", async () => {
    checkUpdateMock.mockResolvedValue(updateInfo());
    await update.openDialog();
    expect(update.state.open).toBe(true);
    checkUpdateMock.mockClear();
    await update.checkAndMaybeOpen();
    expect(checkUpdateMock).not.toHaveBeenCalled();
  });

  it("openWith 复用调用方已有的检查结果，不重新请求", async () => {
    update.openWith(updateInfo("9.9.11"));
    expect(update.state.open).toBe(true);
    expect(update.state.info?.latest).toBe("9.9.11");
    expect(checkUpdateMock).not.toHaveBeenCalled();
  });

  it("结果超过 1 分钟再打开会重拉，避免弹窗停在旧结论", async () => {
    checkUpdateMock.mockResolvedValue(updateInfo("9.9.9"));
    vi.useFakeTimers();
    try {
      await update.openDialog();
      expect(checkUpdateMock).toHaveBeenCalledTimes(1);

      // 1 分钟内重复打开：复用缓存，不重复请求
      update.dismiss();
      await update.openDialog();
      expect(checkUpdateMock).toHaveBeenCalledTimes(1);

      vi.setSystemTime(Date.now() + 61_000);
      update.dismiss();
      await update.openDialog();
      expect(checkUpdateMock).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });
});
