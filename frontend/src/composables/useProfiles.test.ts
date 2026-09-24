/**
 * `openActiveProfileForEdit` 的单元测试：进入「方案」页自动展示当前方案的护栏。
 *
 * 最关键的一条是「列表未就绪时不得打开」——`showProfileEditor` 对缺失 id 的既有
 * 语义是「打开空白新建表单」，若自动打开走进那条分支，用户会看到一个空表单
 * 并以为配置丢了。这里用桩把两条路径区分开。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

// `vi.mock` 工厂会被提升到 import 之前，故桩函数必须经 `vi.hoisted` 先建出来，
// 否则工厂里读到的还是未初始化的 `const`（TDZ 报错）
const { getMock, listMock, toastMock, saveMock, createMock } = vi.hoisted(() => ({
  getMock: vi.fn(),
  listMock: vi.fn(),
  toastMock: vi.fn(),
  saveMock: vi.fn(),
  createMock: vi.fn(),
}));

vi.mock("../api", () => ({
  profilesApi: {
    list: () => listMock(),
    get: (id: string) => getMock(id),
    save: (id: string, payload: unknown) => saveMock(id, payload),
    create: (id: string, payload: unknown) => createMock(id, payload),
    delete: vi.fn(),
    setActive: vi.fn(),
    detect: vi.fn(),
    toggleAutoSwitch: vi.fn(),
    export: vi.fn(),
    import: vi.fn(),
  },
}));

vi.mock("../utils/file", () => ({
  pickFile: vi.fn(),
}));

// useConfirm：confirmDiscardIfDirty 走这里，桩成「确认放弃」以放行
vi.mock("./useConfirm", () => ({
  useConfirm: () => ({ confirm: vi.fn(async () => true) }),
}));
vi.mock("./useToast", () => ({
  useToast: () => ({ toastOnly: toastMock }),
}));

const { useProfiles } = await import("./useProfiles");

const p = useProfiles();

function summary(id: string, name: string) {
  return {
    id,
    name,
    username: "",
    isp: "",
    active_task: "",
    active_http_task: "",
    active_script_task: "",
    login_channel: "browser" as const,
    gateway_ip: "",
    wifi_ssid: "",
    auth_url: "",
    trigger_url: "",
  };
}

beforeEach(() => {
  getMock.mockReset();
  listMock.mockReset();
  p.editingProfile.value = null;
  p.profiles.value = {};
  p.activeProfileId.value = "default";
});

describe("openActiveProfileForEdit", () => {
  it("方案列表未加载（活跃方案不在列表里）时不打开，避免退化成空白新建表单", async () => {
    // activeProfileId 初始恒为 "default"，但 profiles 为空 = 尚未拉取成功
    p.activeProfileId.value = "default";
    p.profiles.value = {};

    const opened = await p.openActiveProfileForEdit();

    expect(opened).toBe(false);
    expect(p.editingProfile.value).toBeNull();
    // 关键：绝不能调 GET 去"加载方案"——那正是会静默变新建草稿的路径
    expect(getMock).not.toHaveBeenCalled();
  });

  it("列表已加载时打开活跃方案，且带 has_password 状态", async () => {
    p.profiles.value = { default: summary("default", "默认网络") };
    p.activeProfileId.value = "default";
    getMock.mockResolvedValue({
      settings: { id: "default", name: "默认网络", username: "20230001", password: "" },
      has_password: true,
    });

    const opened = await p.openActiveProfileForEdit();

    expect(opened).toBe(true);
    expect(getMock).toHaveBeenCalledWith("default");
    expect(p.editingProfile.value?.id).toBe("default");
    expect(p.editingProfile.value?._isNew).toBe(false);
    expect(p.editorHasPassword.value).toBe(true);
  });

  it("活跃方案是 A 时打开 A（而非列表首项）", async () => {
    p.profiles.value = {
      default: summary("default", "默认网络"),
      dorm: summary("dorm", "宿舍"),
    };
    p.activeProfileId.value = "dorm";
    getMock.mockResolvedValue({
      settings: { id: "dorm", name: "宿舍", username: "u", password: "" },
      has_password: false,
    });

    await p.openActiveProfileForEdit();

    expect(getMock).toHaveBeenCalledWith("dorm");
    expect(p.editingProfile.value?.id).toBe("dorm");
  });

  it("已有草稿时直接复用，不重载也不弹「放弃未保存」确认", async () => {
    p.profiles.value = { default: summary("default", "默认网络") };
    p.activeProfileId.value = "default";
    // 模拟用户上次在本页编到一半
    p.editingProfile.value = {
      ...summary("default", "默认网络"),
      username: "半成品",
      password: "",
      auth_url: "",
      trigger_url: "",
      login_channel: "http" as const,
      active_http_task: "dorm-portal",
      _isNew: false,
    };

    const opened = await p.openActiveProfileForEdit();

    expect(opened).toBe(true);
    // 复用而非重载：重载会走 confirmDiscardIfDirty，等于每次回页都问是否丢弃
    expect(getMock).not.toHaveBeenCalled();
    expect(p.editingProfile.value?.username).toBe("半成品");
  });

  it("活跃方案加载失败时返回 false 且不留草稿", async () => {
    p.profiles.value = { default: summary("default", "默认网络") };
    p.activeProfileId.value = "default";
    getMock.mockRejectedValue(new Error("network down"));

    const opened = await p.openActiveProfileForEdit();

    expect(opened).toBe(false);
    expect(p.editingProfile.value).toBeNull();
  });
});

describe("保存方案的「渠道 → 任务」闸口", () => {
  /** 一份可保存的草稿（账号密码齐、ID 合法），按需覆盖渠道与绑定 */
  function draft(overrides: Record<string, unknown> = {}) {
    return {
      ...summary("dorm", "宿舍"),
      username: "20230001",
      password: "pw",
      _isNew: true,
      ...overrides,
    };
  }

  /** 最后一条 toast 的文案（toastOnly(ok, msg) 的第二参） */
  function lastToast(): string {
    const call = toastMock.mock.calls.at(-1);
    return String(call?.[1] ?? "");
  }

  it("脚本渠道未绑定脚本任务时拒绝保存，且一个请求都不发", async () => {
    // 脚本渠道没有内置兜底任务：放过去只会在登录那一刻失败，失败点还离配置很远
    p.editingProfile.value = draft({ login_channel: "script" as const, active_script_task: "" });

    const ok = await p.saveProfile();

    expect(ok).toBe(false);
    expect(createMock).not.toHaveBeenCalled();
    expect(saveMock).not.toHaveBeenCalled();
    expect(lastToast()).toContain("脚本任务");
  });

  it("脚本渠道绑定后放行，并把绑定一起提交（新建走 create）", async () => {
    createMock.mockResolvedValue({ message: "ok" });
    listMock.mockResolvedValue({ profiles: {}, active_profile: "default", auto_switch: false });
    p.editingProfile.value = draft({
      login_channel: "script" as const,
      active_script_task: "portal-login",
    });

    const ok = await p.saveProfile();

    expect(ok).toBe(true);
    const payload = createMock.mock.calls[0]?.[1] as Record<string, unknown>;
    expect(payload.login_channel).toBe("script");
    expect(payload.active_script_task).toBe("portal-login");
  });

  it("直连渠道的既有闸口未被新分支放宽（回归）", async () => {
    p.editingProfile.value = draft({ login_channel: "http" as const, active_http_task: "" });

    expect(await p.saveProfile()).toBe(false);
    expect(lastToast()).toContain("直连任务");
  });
});
