/**
 * 配置 dirty 判定的单元测试（回归：开关关了再打开不应显示"配置已变更"）。
 * dirty 现为快照比对制：与最近一次加载/保存的配置快照比较，值回原样 dirty 自动消失。
 *
 * useConfig 是模块级单例：同一文件内的用例共享状态，按声明顺序执行。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { nextTick } from "vue";

const fetchMock = vi.fn();
const patchMock = vi.fn(async () => ({}));
const setLogLevelMock = vi.fn(async () => ({ message: "ok" }));
const fetchLogLevelsMock = vi.fn(async () => ({ level: "INFO" }));

vi.mock("../api", () => ({
  configApi: {
    fetch: () => fetchMock(),
    patch: (...a: unknown[]) => patchMock(...(a as [])),
    setLogLevel: (...a: unknown[]) => setLogLevelMock(...(a as [])),
    fetchLogLevels: () => fetchLogLevelsMock(),
  },
  autostartApi: { toggle: vi.fn() },
  pureModeApi: { fetch: vi.fn(), toggle: vi.fn() },
}));

const { useConfig } = await import("./useConfig");

const config = useConfig();

/** 推进一轮渲染，让 flush:'post' 的深度 watch 回调跑完 */
async function flushWatch(): Promise<void> {
  await nextTick();
  await nextTick();
}

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockResolvedValue({
    username: "user",
    auth_url: "http://portal.example",
    monitor: { enable_tcp_check: true },
    logging: { level: "INFO" },
  });
  patchMock.mockClear();
  setLogLevelMock.mockClear();
});

describe("dirty 快照比对", () => {
  it("加载配置后 dirty 为 false", async () => {
    await config.fetchConfig();
    expect(config.dirty.value).toBe(false);
  });

  it("开关关掉再打开（回到已保存值），dirty 自动恢复 false", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    config.config.monitor.enable_tcp_check = true;
    await flushWatch();
    expect(config.dirty.value).toBe(false);
  });

  it("改动后 dirty 为 true，保存成功后复位 false", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    await config.saveConfig();
    expect(patchMock).toHaveBeenCalledTimes(1);
    expect(config.dirty.value).toBe(false);
  });

  it("日志级别走独立即时保存 API，不把表单误标为已变更", async () => {
    await config.fetchConfig();
    await config.setLogLevel("DEBUG");
    await flushWatch();
    expect(config.dirty.value).toBe(false);

    // 已有未保存编辑时，日志级别变更不得把快照"烘焙"进去
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    await config.setLogLevel("INFO");
    await flushWatch();
    expect(config.dirty.value).toBe(true);
    // 用户撤销编辑后，dirty 仍为 true（快照未包含即时保存的级别变更）
    config.config.monitor.enable_tcp_check = true;
    await flushWatch();
    expect(config.dirty.value).toBe(true);
  });
});
