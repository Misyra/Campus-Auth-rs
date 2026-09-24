/**
 * 卸载流程的单元测试。
 *
 * 卸载 = 两步（清系统残留 → 删程序并退出），每步**失败语义不同**，且中间有一步
 * 不可恢复的确认。这里盯住的正是在改版里最容易悄悄坏掉的几条：
 * 1. 守卫拒绝时一个请求都不发（否则会去删一个源码仓库）；
 * 2. 「保留配置与任务」勾选后，删除清单里必须**真的**没有那些数据目录；
 * 3. 确认文案要逐项点名（"删哪些内容"是用户拍板的口径）；
 * 4. 第一步失败**不阻断**第二步；第二步失败必须说清"程序文件未被删除"；
 * 5. 执行中不可关窗。
 *
 * 依赖全部 mock（api / toast / confirm），不碰真实 HTTP。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { UninstallTarget } from "../api/types";

const { apiMock, confirmMock, loggerWarnMock } = vi.hoisted(() => ({
  apiMock: {
    detect: vi.fn(),
    uninstall: vi.fn(),
    purge: vi.fn(),
  },
  // 显式声明参数：零参签名会让 mock.calls[0][0] 触发 TS2493
  confirmMock: vi.fn(async (_options?: unknown) => true as boolean | null),
  loggerWarnMock: vi.fn(),
}));

vi.mock("../api", () => ({ uninstallApi: apiMock }));
vi.mock("./useConfirm", () => ({ useConfirm: () => ({ confirm: confirmMock }) }));
vi.mock("../utils/logger", () => ({
  frontendLogger: { info: vi.fn(), warn: loggerWarnMock, error: vi.fn(), debug: vi.fn() },
}));

const { useUninstall, purgeConfirmMessage } = await import("./useUninstall");

function target(key: string, label: string, exists = true): UninstallTarget {
  return { key, label, path: `/base/${key}`, exists };
}

/** 检测响应：程序目录 + 五个数据目录（其中 logs 不存在） */
function detectResult(overrides: Record<string, unknown> = {}) {
  return {
    items: [
      { key: "user_data", label: "用户数据目录（加密密钥等）", exists: true, description: "/home/u/.campus_network_auth" },
      { key: "playwright", label: "Playwright 浏览器缓存", exists: true, description: "/home/u/.cache/ms-playwright" },
      { key: "autostart", label: "开机自启动", exists: false, description: "未注册" },
    ],
    program: { label: "程序目录", path: "/base", exists: true },
    helper: { label: "卸载助手", path: "/base/campus-auth-helper", exists: true },
    data: [
      target("config", "配置与方案"),
      target("tasks", "任务与脚本"),
      target("logs", "日志", false),
      target("environment", "Python 环境"),
      target("update", "更新缓存"),
    ],
    blocked: null,
    ...overrides,
  };
}

beforeEach(() => {
  apiMock.detect.mockReset();
  apiMock.uninstall.mockReset();
  apiMock.purge.mockReset();
  confirmMock.mockReset();
  loggerWarnMock.mockReset();
  apiMock.detect.mockResolvedValue(detectResult());
  apiMock.uninstall.mockResolvedValue({ results: [], message: "系统残留已清理" });
  apiMock.purge.mockResolvedValue({
    message: "正在卸载，程序即将退出",
    kept_user_data: false,
    install_dir: "/base",
    data_dirs: [],
    cancelled_pending_update: false,
  });
  confirmMock.mockResolvedValue(true);
});

describe("卸载检测", () => {
  it("打开弹窗只探测、不执行任何删除", async () => {
    const u = useUninstall();
    await u.openDialog();

    expect(apiMock.detect).toHaveBeenCalledTimes(1);
    expect(apiMock.uninstall).not.toHaveBeenCalled();
    expect(apiMock.purge).not.toHaveBeenCalled();
    expect(u.program.value?.path).toBe("/base");
    expect(u.data.value).toHaveLength(5);
    expect(u.blocked.value).toBeNull();
  });

  it("检测失败时如实显示错误，且不预置任何清单", async () => {
    apiMock.detect.mockRejectedValue(new Error("boom"));
    const u = useUninstall();
    await u.openDialog();
    expect(u.detectError.value).toContain("boom");
    expect(u.program.value).toBeNull();
  });

  it("守卫拒绝（如源码仓库）时给出原因并记日志", async () => {
    apiMock.detect.mockResolvedValue(
      detectResult({ blocked: "拒绝执行：/repo 看起来是源码仓库" }),
    );
    const u = useUninstall();
    await u.openDialog();
    expect(u.blocked.value).toContain("源码仓库");
    expect(loggerWarnMock).toHaveBeenCalled();
  });
});

describe("删除清单随勾选变化", () => {
  it("默认（不勾选）删除全部已存在的用户数据目录", async () => {
    const u = useUninstall();
    await u.openDialog();
    expect(u.dataToDelete.value.map((d) => d.key)).toEqual([
      "config",
      "tasks",
      "environment",
      "update",
    ]);
    expect(u.keptData.value).toEqual([]);
    // 程序目录 + 4 个数据目录
    expect(u.deleteCount.value).toBe(5);
  });

  it("勾选「保留配置与任务」后删除清单里没有任何数据目录", async () => {
    const u = useUninstall();
    await u.openDialog();
    u.keepUserData.value = true;

    expect(u.dataToDelete.value).toEqual([]);
    expect(u.keptData.value.map((d) => d.key)).toEqual([
      "config",
      "tasks",
      "environment",
      "update",
    ]);
    expect(u.deleteCount.value).toBe(1);
  });
});

describe("确认文案", () => {
  it("逐项点名将删除的内容（用户要求「说清删哪些」）", () => {
    const program: UninstallTarget = { label: "程序目录", path: "/base", exists: true };
    const text = purgeConfirmMessage(
      program,
      [target("config", "配置与方案"), target("tasks", "任务与脚本")],
      [],
    );
    expect(text).toContain("将永久删除");
    expect(text).toContain("程序目录");
    expect(text).toContain("配置与方案");
    expect(text).toContain("任务与脚本");
    expect(text).toContain("不可恢复");
  });

  it("勾选保留时说清保留了几项", () => {
    const text = purgeConfirmMessage(
      { label: "程序目录", path: "/base", exists: true },
      [],
      [target("config", "配置与方案"), target("tasks", "任务与脚本")],
    );
    expect(text).toContain("保留 2 项用户数据");
    expect(text).toContain("程序目录");
  });
});

describe("执行卸载", () => {
  it("用户取消确认：一个请求都不发", async () => {
    confirmMock.mockResolvedValue(false);
    const u = useUninstall();
    await u.openDialog();
    await u.run();
    expect(apiMock.uninstall).not.toHaveBeenCalled();
    expect(apiMock.purge).not.toHaveBeenCalled();
  });

  it("守卫拒绝时不发请求（不能去删源码仓库）", async () => {
    apiMock.detect.mockResolvedValue(detectResult({ blocked: "拒绝执行：源码仓库" }));
    const u = useUninstall();
    await u.openDialog();
    await u.run();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(apiMock.purge).not.toHaveBeenCalled();
  });

  it("先清系统残留、再启动卸载，并把勾选值传下去", async () => {
    const u = useUninstall();
    await u.openDialog();
    u.keepUserData.value = true;
    await u.run();

    expect(apiMock.uninstall).toHaveBeenCalledTimes(1);
    // **两步必须收到同一个值**：勾了保留时第一步要留着加密密钥目录，否则被保留下来的
    // config/ 里那些 ENC: 方案密码再也解不开（"保留配置与任务"就成了半句空话）
    expect(apiMock.uninstall).toHaveBeenCalledWith(true);
    expect(apiMock.purge).toHaveBeenCalledWith(true);
    // 顺序：purge 必须在 uninstall 之后
    const order = apiMock.uninstall.mock.invocationCallOrder[0];
    expect(order).toBeLessThan(apiMock.purge.mock.invocationCallOrder[0]);
    expect(u.phase.value).toBe("done");
  });

  it("默认不勾选：两步都收到 false（真卸载）", async () => {
    const u = useUninstall();
    await u.openDialog();
    await u.run();

    expect(apiMock.uninstall).toHaveBeenCalledWith(false);
    expect(apiMock.purge).toHaveBeenCalledWith(false);
  });

  it("卸载助手缺失时拦下卸载，一个请求都不发（否则清完残留却删不掉程序）", async () => {
    apiMock.detect.mockResolvedValue(
      detectResult({ helper: { label: "卸载助手", path: "/base/campus-auth-helper", exists: false } }),
    );
    const u = useUninstall();
    await u.openDialog();

    expect(u.blockReason.value).toContain("卸载助手缺失");
    await u.run();
    expect(confirmMock, "拦下时连确认都不该弹").not.toHaveBeenCalled();
    expect(apiMock.uninstall).not.toHaveBeenCalled();
    expect(apiMock.purge).not.toHaveBeenCalled();
  });

  it("第一步失败**不阻断**卸载程序（环境清理是 best-effort）", async () => {
    apiMock.uninstall.mockRejectedValue(new Error("缓存被占用"));
    const u = useUninstall();
    await u.openDialog();
    await u.run();

    expect(apiMock.purge).toHaveBeenCalledTimes(1);
    expect(u.phase.value).toBe("done");
    expect(u.cleanupMessage.value).toContain("缓存被占用");
    expect(u.error.value).toBe("");
  });

  it("第二步失败必须说清程序文件未被删除（否则用户以为已经卸了）", async () => {
    apiMock.purge.mockRejectedValue(new Error("助手缺失"));
    const u = useUninstall();
    await u.openDialog();
    await u.run();

    expect(u.error.value).toContain("程序文件未被删除");
    expect(u.phase.value).toBe("idle");
  });

  it("待应用更新没取消掉时，回执要出声（否则下次开机程序还在）", async () => {
    apiMock.purge.mockResolvedValue({
      message: "正在卸载，程序即将退出（注意：待应用的更新未能取消，程序可能被重新安装）",
      kept_user_data: false,
      install_dir: "/base",
      data_dirs: [],
      cancelled_pending_update: false,
      pending_update_left: true,
    });
    const u = useUninstall();
    await u.openDialog();
    await u.run();

    expect(u.pendingUpdateLeft.value).toBe(true);
    expect(u.phase.value).toBe("done");
  });

  it("执行中不可关窗（卸载已不可逆，半途关窗会让人以为没生效）", async () => {
    let releasePurge: (v: unknown) => void = () => {};
    apiMock.purge.mockImplementation(
      () =>
        new Promise((resolve) => {
          releasePurge = resolve;
        }),
    );
    const u = useUninstall();
    await u.openDialog();
    const running = u.run();

    // run() 的第一个 await 是确认弹窗，故要等它真正进入执行阶段再断言
    await vi.waitFor(() => expect(u.running.value).toBe(true));
    u.closeDialog();
    expect(u.open.value).toBe(true);

    releasePurge({
      message: "",
      kept_user_data: false,
      install_dir: "/base",
      data_dirs: [],
      cancelled_pending_update: false,
    });
    await running;
    // 完成后可以关
    u.closeDialog();
    expect(u.open.value).toBe(false);
  });
});
