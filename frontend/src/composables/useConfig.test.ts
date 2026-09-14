/**
 * 配置 dirty 判定的单元测试（回归：开关关了再打开不应显示"配置已变更"）。
 * dirty 现为快照比对制：与最近一次加载/保存的配置快照比较，值回原样 dirty 自动消失。
 *
 * useConfig 是模块级单例：同一文件内的用例共享状态，按声明顺序执行。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { nextTick } from "vue";

const fetchMock = vi.fn();
// 显式声明一个参数：否则 vi.fn(async () => ({})) 推断为零参签名，
// 取 calls[0][0] 会触发 TS2493（类型检查此前因 tsconfig 是 solution 文件而未生效）
const patchMock = vi.fn(async (_payload: Record<string, unknown>) => ({}));
const setLogLevelMock = vi.fn(async () => ({ message: "ok" }));
const fetchLogLevelsMock = vi.fn(async () => ({ level: "INFO" }));

vi.mock("../api", () => ({
  configApi: {
    fetch: () => fetchMock(),
    patch: (...a: unknown[]) => patchMock(a[0] as Record<string, unknown>),
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

  it("保存新密码后不会被密码 watcher 重新标记为未保存", async () => {
    await config.fetchConfig();
    config.password.setValue("new-secret");
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    await config.saveConfig();
    await flushWatch();
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

  it("登录渠道与直连参数随设置读写往返（设置页与方案编辑器共用同一组件）", async () => {
    // 后端未回传该字段时（老版本）回落默认值，不应出现 undefined
    await config.fetchConfig();
    expect(config.config.credentials.login_channel).toBe("browser");
    expect(config.config.credentials.http_method).toBe("GET");

    // 后端回传直连配置时如实回填
    fetchMock.mockResolvedValue({
      username: "user",
      login_channel: "http",
      http_method: "POST",
      http_url: "http://10.0.0.1/login",
      http_body: "u={username}&p={password}",
      http_success_pattern: "登录成功",
    });
    await config.fetchConfig();
    expect(config.config.credentials.login_channel).toBe("http");
    expect(config.config.credentials.http_method).toBe("POST");
    expect(config.config.credentials.http_url).toBe("http://10.0.0.1/login");

    // 改动渠道构成未保存变更，且随保存载荷提交（后端按扁平键写入活跃 Profile）
    config.config.credentials.login_channel = "browser";
    await flushWatch();
    expect(config.dirty.value).toBe(true);
    await config.saveConfig();
    const payload = patchMock.mock.calls[0][0] as Record<string, unknown>;
    expect(payload.login_channel).toBe("browser");
    expect(payload.http_url).toBe("http://10.0.0.1/login");
    // 不得嵌进 credentials 子对象（后端按扁平结构读取）
    expect(payload.credentials).toBeUndefined();
  });
});
