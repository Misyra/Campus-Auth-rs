/**
 * 配置 dirty 判定的单元测试（回归：开关关了再打开不应显示"配置已变更"）。
 * dirty 现为快照比对制：与最近一次加载/保存的配置快照比较，值回原样 dirty 自动消失。
 *
 * useConfig 是模块级单例：同一文件内的用例共享状态，按声明顺序执行。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { nextTick } from "vue";
import { useToast } from "./useToast";
import { ApiError } from "../api/client";
import type { AutostartToggleResult } from "../api/types";

const fetchMock = vi.fn();
// 显式声明一个参数：否则 vi.fn(async () => ({})) 推断为零参签名，
// 取 calls[0][0] 会触发 TS2493（类型检查此前因 tsconfig 是 solution 文件而未生效）
const patchMock = vi.fn(async (_payload: Record<string, unknown>) => ({}));
// T5-3：section 增量保存（patchSection(section, body)）
const patchSectionMock = vi.fn(
  async (_section: string, _body: Record<string, unknown>) => ({}),
);
const setLogLevelMock = vi.fn(async () => ({ message: "ok" }));
const fetchLogLevelsMock = vi.fn(async () => ({ level: "INFO" }));
const pureModeFetchMock = vi.fn(async () => ({ enabled: true }));
const pureModeToggleMock = vi.fn(async () => ({ enabled: false }));
// 具名 AutostartToggleResult（对齐后端 { message, warning? }）：status 供
// toggleAutostart 收尾刷新（finally 里的 fetchAutostart）使用
const autostartToggleMock = vi.fn(async (_enable: boolean): Promise<AutostartToggleResult> => ({ message: "ok" }));
const autostartStatusMock = vi.fn(async () => ({
  platform: "linux",
  enabled: true,
  method: "desktop file",
  location: "",
  runtime_mode: "full",
}));

vi.mock("../api", () => ({
  configApi: {
    fetch: () => fetchMock(),
    patch: (...a: unknown[]) => patchMock(a[0] as Record<string, unknown>),
    patchSection: (...a: unknown[]) =>
      patchSectionMock(a[0] as string, a[1] as Record<string, unknown>),
    setLogLevel: (...a: unknown[]) => setLogLevelMock(...(a as [])),
    fetchLogLevels: () => fetchLogLevelsMock(),
  },
  autostartApi: {
    toggle: (enable: boolean) => autostartToggleMock(enable),
    fetchStatus: () => autostartStatusMock(),
  },
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
  patchSectionMock.mockClear();
  setLogLevelMock.mockClear();
  pureModeFetchMock.mockClear();
  pureModeToggleMock.mockClear();
  autostartToggleMock.mockClear();
  autostartStatusMock.mockClear();
});

describe("dirty 快照比对", () => {
  it("旧配置默认直连，检测代理开关可保存并重新加载", async () => {
    await config.fetchConfig();
    expect(config.config.browser.redirect_test_use_proxy).toBe(false);
    config.config.browser.redirect_test_use_proxy = true;
    await flushWatch();
    expect(config.dirty.value).toBe(true);
    await config.saveConfig();
    // T5-3：browser 段 dirty → section 端点只发 browser 一段
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    const [section, body] = patchSectionMock.mock.calls[0];
    expect(section).toBe("browser");
    expect((body as Record<string, unknown>).redirect_test_use_proxy).toBe(true);
    fetchMock.mockResolvedValue({ browser: body });
    await config.fetchConfig();
    expect(config.config.browser.redirect_test_use_proxy).toBe(true);
    expect(config.dirty.value).toBe(false);
  });

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
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(patchSectionMock.mock.calls[0][0]).toBe("monitor");
    expect(config.dirty.value).toBe(false);
  });

  it("PATCH 在途期间的编辑不算已保存（回归：曾被静默置为干净）", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    // 让 section 请求挂起，制造"请求在途"窗口
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    patchSectionMock.mockImplementationOnce(async () => {
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
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(config.dirty.value).toBe(true);

    // 再保存一次应当真的把 true 提交上去
    await config.saveConfig();
    expect(patchSectionMock).toHaveBeenCalledTimes(2);
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

  it("设置保存只发全局 section，绝不携带方案凭据字段", async () => {
    // 回归（known-issues #23 E1）：此前载荷带着 username/auth_url/login_channel 等
    // 13 个方案字段，任意 Tab 的「立即保存」都会把活跃方案的凭据一并落盘——
    // 账号页「自动检测」填入的未确认候选地址就是这样被静默持久化的。
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();
    await config.saveConfig();

    // T5-3：全局保存只经 section 端点，段名必须是全局设置段
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(patchSectionMock.mock.calls[0][0]).toBe("monitor");
    // 全量 PATCH 不得被走到（它会原样接受方案域顶层键）
    expect(patchMock).not.toHaveBeenCalled();
    const body = patchSectionMock.mock.calls[0][1] as Record<string, unknown>;
    expect(body.enable_tcp_check).toBe(false);
    // section 载荷是 monitor 段自身，方案域字段无从出现
    const profileKeys = [
      "username", "password", "auth_url", "trigger_url", "isp",
      "active_task", "login_channel", "active_http_task", "clear_password",
    ];
    for (const key of profileKeys) {
      expect(body, `section 载荷不得包含方案字段 ${key}`).not.toHaveProperty(key);
    }
  });

  it("多段同时 dirty 时按段分组逐段提交，未改动的段不发送", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    config.config.updater.auto_check_enabled = false;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    await config.saveConfig();
    // 只发 monitor / updater 两段；browser 等未改动段不得出现在载荷里
    const sections = patchSectionMock.mock.calls.map(([s]) => s).sort();
    expect(sections).toEqual(["monitor", "updater"]);
    const updaterBody = patchSectionMock.mock.calls.find(([s]) => s === "updater")![1] as Record<string, unknown>;
    expect(updaterBody.auto_check_enabled).toBe(false);
    // 全量 PATCH 不参与
    expect(patchMock).not.toHaveBeenCalled();
  });

  it("管理密码改动归属 app_settings 段（不再有段外顶层字段）", async () => {
    await config.fetchConfig();
    // 管理密码是 app_settings 段内字段（后端 global.app.management_password）：
    // 改动只发该段，全量 PATCH 不得被走到
    config.config.app_settings.management_password = "bootstrap-pass";
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    await config.saveConfig();
    expect(patchMock).not.toHaveBeenCalled();
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    const [section, body] = patchSectionMock.mock.calls[0] as [string, Record<string, unknown>];
    expect(section).toBe("app_settings");
    expect(body.management_password).toBe("bootstrap-pass");
    expect(config.dirty.value).toBe(false);
  });

  it("GET 响应的 app_settings.management_password 加载进表单（断链回归）", async () => {
    // 回归：此前前端读顶层 data.management_password（后端从不下发该字段），
    // 密码框恒为空且编辑不落盘——读取必须走 app_settings 段
    fetchMock.mockResolvedValue({
      app_settings: { management_password: "from-server" },
    });
    await config.fetchConfig();
    expect(config.config.app_settings.management_password).toBe("from-server");
  });

  it("section 端点 404 时防御性回退全量 PATCH 一次", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();

    // 过渡期后端未挂载 section 路由（理论上不该发生）：一次性回退全量保存
    patchSectionMock.mockRejectedValueOnce(new ApiError("not found", 404));
    await config.saveConfig();
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(patchMock).toHaveBeenCalledTimes(1);
    const payload = patchMock.mock.calls[0][0] as Record<string, unknown>;
    expect((payload.monitor as Record<string, unknown>).enable_tcp_check).toBe(false);
    // 回退成功视同保存成功
    expect(config.saveFailed.value).toBe(false);
    expect(config.dirty.value).toBe(false);
  });

  it("section 保存失败（非 404）不回退全量，保留 dirty 与失败标记", async () => {
    await config.fetchConfig();
    config.config.monitor.enable_tcp_check = false;
    await flushWatch();

    // 400 = 载荷本身被拒：全量载荷同样会被拒，回退只会掩盖出错的段
    patchSectionMock.mockRejectedValueOnce(new ApiError("monitor 配置无效", 400));
    await config.saveConfig();
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(patchMock).not.toHaveBeenCalled();
    expect(config.saveFailed.value).toBe(true);
    // 快照未前移：dirty 保持，用户可修正后重试
    expect(config.dirty.value).toBe(true);
  });

  it("多段保存部分成功后基准逐段前移：已落盘段改回旧值在重试中补发", async () => {
    await config.fetchConfig();
    // 两段同时 dirty：monitor（先发、将成功落盘）+ notification（后发、将被 400 拒）
    const initialNotification = config.config.notification.enabled;
    config.config.monitor.enable_tcp_check = false;
    config.config.notification.enabled = !initialNotification;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    patchSectionMock.mockImplementation(async (section: string) => {
      if (section === "notification") throw new ApiError("通知配置无效", 400);
      return {};
    });
    await config.saveConfig();
    expect(patchSectionMock).toHaveBeenCalledTimes(2);
    expect(config.saveFailed.value).toBe(true);

    // 用户修正 notification、把 monitor 改回旧值：整表值与旧快照完全一致——
    // 若基准停在旧快照，dirty 不亮、重试不含 monitor，monitor 的落盘值与
    // 界面静默背离；基准前移后 monitor 相对新基准是变更，必须重显 dirty
    config.config.notification.enabled = initialNotification;
    config.config.monitor.enable_tcp_check = true;
    await flushWatch();
    expect(config.dirty.value).toBe(true);

    // 重试：monitor 的回退值必须补发（补偿 PATCH），notification 与基准
    // 一致不再发送
    patchSectionMock.mockImplementation(
      async (_s: string, _b: Record<string, unknown>) => ({}),
    );
    patchSectionMock.mockClear();
    await config.saveConfig();
    const sections = patchSectionMock.mock.calls.map((c) => c[0]);
    expect(sections).toEqual(["monitor"]);
    const monitorBody = patchSectionMock.mock.calls[0][1] as Record<string, unknown>;
    expect(monitorBody.enable_tcp_check).toBe(true);
    expect(config.dirty.value).toBe(false);
    expect(config.saveFailed.value).toBe(false);
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

    // 随后任意一次普通保存不再触碰 browser 段（T5-3 按段分组：开关值已同步进
    // 模型与快照，browser 未 dirty）——旧值覆盖回退的路径被分组结构性地消除
    config.config.monitor.enable_tcp_check = true;
    await flushWatch();
    await config.saveConfig();
    expect(patchSectionMock).toHaveBeenCalledTimes(1);
    expect(patchSectionMock.mock.calls[0][0]).toBe("monitor");

    // 但当 browser 段因任何原因再次 dirty 时，载荷必须携带关闭态（回写机制的
    // 回归锚点：模型里若是旧值 true，这里就会把它翻回去）。
    // （headless 默认 true，翻转成 false 才是真实变更）
    config.config.browser.headless = false;
    await flushWatch();
    await config.saveConfig();
    const browserCall = patchSectionMock.mock.calls.find(([s]) => s === "browser");
    expect(browserCall).toBeDefined();
    expect((browserCall![1] as Record<string, unknown>).pure_mode).toBe(false);
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

describe("toggleAutostart 具名返回消费", () => {
  it("成功时使用后端 message 提示，并在收尾刷新自启动状态", async () => {
    const { toast } = useToast();
    autostartToggleMock.mockResolvedValueOnce({
      message: "已启用开机自启动并通过系统状态复核",
    });
    await config.toggleAutostart(true);
    expect(autostartToggleMock).toHaveBeenCalledWith(true);
    expect(toast.message).toBe("已启用开机自启动并通过系统状态复核");
    // 全局轮询已移除：开关后的 fetchAutostart 是状态刷新的关键路径
    expect(autostartStatusMock).toHaveBeenCalled();
  });

  it("warning 字符串时追加「配置缓存保存失败」失败提示（系统注册已生效）", async () => {
    const { toast } = useToast();
    autostartToggleMock.mockResolvedValueOnce({ message: "ok", warning: "模拟磁盘故障" });
    await config.toggleAutostart(false);
    expect(autostartToggleMock).toHaveBeenCalledWith(false);
    expect(toast.message).toBe("系统设置已生效，但配置缓存保存失败：模拟磁盘故障");
  });

  it("message 缺省时回退本地文案", async () => {
    const { toast } = useToast();
    autostartToggleMock.mockResolvedValueOnce({});
    await config.toggleAutostart(true);
    expect(toast.message).toBe("启用自启动成功");
  });

  it("后端 404 时提示不支持而非笼统失败", async () => {
    const { toast } = useToast();
    autostartToggleMock.mockRejectedValueOnce(new ApiError("not found", 404));
    await config.toggleAutostart(true);
    expect(toast.message).toBe("当前后端版本不支持开机自启动，请重启后端");
  });
});
