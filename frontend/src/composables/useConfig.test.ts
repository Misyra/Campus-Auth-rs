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
const pureModeFetchMock = vi.fn(async () => ({ enabled: true }));
const pureModeToggleMock = vi.fn(async () => ({ enabled: false }));

vi.mock("../api", () => ({
  configApi: {
    fetch: () => fetchMock(),
    patch: (...a: unknown[]) => patchMock(a[0] as Record<string, unknown>),
    setLogLevel: (...a: unknown[]) => setLogLevelMock(...(a as [])),
    fetchLogLevels: () => fetchLogLevelsMock(),
  },
  autostartApi: { toggle: vi.fn() },
  pureModeApi: {
    fetch: () => pureModeFetchMock(),
    toggle: () => pureModeToggleMock(),
  },
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
  pureModeFetchMock.mockClear();
  pureModeToggleMock.mockClear();
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

  it("PATCH 在途期间的编辑不算已保存（回归：曾被静默置为干净）", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    // 让 PATCH 挂起，制造"请求在途"窗口
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    patchMock.mockImplementationOnce(async () => {
      await gate;
      return {};
    });

    const saving = config.saveConfig();
    // 在途期间用户继续编辑：这次改动不属于上面那个已经构造好的载荷
    config.config.monitor.enable_tcp_check = true;
    await flushWatch();

    release();
    await saving;

    // 提交的是 false、途中改成 true 从未提交 → 必须仍显示"未保存"，
    // 否则用户既看不到脏提示、也不会再点一次保存，改动就丢了
    expect(patchMock).toHaveBeenCalledTimes(1);
    expect(config.dirty.value).toBe(true);

    // 再保存一次应当真的把 true 提交上去
    await config.saveConfig();
    expect(patchMock).toHaveBeenCalledTimes(2);
    expect(config.dirty.value).toBe(false);
  });

  it("密码不再属于全局设置：useConfig 不暴露 password 字段", async () => {
    await config.fetchConfig();
    // 账号字段已迁往方案页（useProfiles），此处暴露 password 意味着又出现了
    // 第二份可写副本——正是「账号混在全局保存栏」导致顺带落盘的根源
    expect("password" in config).toBe(false);
    expect("clearPassword" in config).toBe(false);
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

  it("设置保存载荷只含全局设置，绝不携带方案凭据字段", async () => {
    // 回归（known-issues #23 E1）：此前载荷带着 username/auth_url/login_channel 等
    // 13 个方案字段，任意 Tab 的「立即保存」都会把活跃方案的凭据一并落盘——
    // 账号页「自动检测」填入的未确认候选地址就是这样被静默持久化的。
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    await config.saveConfig();

    const payload = patchMock.mock.calls[0][0] as Record<string, unknown>;
    // 全局字段照常提交
    expect(payload.monitor).toBeDefined();
    expect(payload.browser).toBeDefined();
    // 方案域字段一个都不能出现
    const profileKeys = [
      "username", "password", "auth_url", "trigger_url", "isp",
      "active_task", "login_channel", "active_http_task", "clear_password",
    ];
    for (const key of profileKeys) {
      expect(payload, `保存载荷不得包含方案字段 ${key}`).not.toHaveProperty(key);
    }
  });

  it("后端仍在扁平响应里回传方案凭据时，本 composable 不接收", async () => {
    // 后端兼容既有客户端，GET 顶层依旧带 username/auth_url 等；
    // 若这里接收了，方案数据就再次有了第二个可写入口。
    fetchMock.mockResolvedValue({
      username: "user",
      auth_url: "http://portal.example",
      login_channel: "http",
      active_http_task: "dorm-portal",
      active_task: "hust",
      monitor: { enable_tcp_check: true },
    });
    await config.fetchConfig();
    expect("credentials" in config.config).toBe(false);
    // 且不因这些字段的存在而误判为未保存
    expect(config.dirty.value).toBe(false);
  });

  it("纯净模式开关回写表单模型，保存时不会被旧值覆盖", async () => {
    // 后端初始为开启（fetchConfig 回传 browser.pure_mode=true）
    fetchMock.mockResolvedValue({
      username: "user",
      browser: { pure_mode: true },
    });
    await config.fetchConfig();
    expect(config.config.browser.pure_mode).toBe(true);

    // 关闭：/api/pure-mode 返回 enabled=false
    await config.togglePureMode();
    expect(config.pureMode.value).toBe(false);
    // 关键断言：开关必须回写 config.browser.pure_mode。否则 saveConfig 的载荷
    // 携带整个 config.browser（旧值 true），后端 json_merge 递归覆盖会把刚关掉
    // 的开关静默翻回开启
    expect(config.config.browser.pure_mode).toBe(false);

    // 开关本身不产生未保存变更（后端已即时落盘）
    await flushWatch();
    expect(config.dirty.value).toBe(false);

    // 随后任意一次普通保存，载荷里的值必须是关闭态
    // （默认 enable_tcp_check 为 false，这里改为 true 以制造真实的未保存变更）
    config.config.monitor.enable_tcp_check = true;
    await flushWatch();
    await config.saveConfig();
    expect(patchMock).toHaveBeenCalledTimes(1);
    const payload = patchMock.mock.calls[0][0] as Record<string, unknown>;
    expect((payload.browser as Record<string, unknown>).pure_mode).toBe(false);
  });

  it("fetchPureMode 拉取到的权威值同样回写表单模型", async () => {
    fetchMock.mockResolvedValue({
      username: "user",
      browser: { pure_mode: true },
    });
    await config.fetchConfig();

    // 后端已关（例如上一次会话关掉的），拉取后表单模型须跟随
    pureModeFetchMock.mockResolvedValue({ enabled: false });
    await config.fetchPureMode(true);
    expect(config.pureMode.value).toBe(false);
    expect(config.config.browser.pure_mode).toBe(false);
  });
});
