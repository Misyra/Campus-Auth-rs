// @vitest-environment happy-dom
/**
 * EnvironmentInstallDialog 初始化弹窗行为测试（createApp 直挂 + vi.mock，
 * 与 AuthBootstrapGuide.test.ts 同范式；Modal 以轻桩替换，免 reka 依赖）。
 *
 * 锁定三条关键行为：
 * - 打开即测速：一排下载源选项（自动测速 + 直连 + 各镜像），吞吐人性化展示；
 * - 点击选项保存偏好并更新选中态（PUT mirror-preference 载荷形状）；
 * - autoStart 打开即触发初始化，busy 占用时不得重复触发。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { createApp, nextTick, reactive, ref, type App } from "vue";
import EnvironmentInstallDialog from "./EnvironmentInstallDialog.vue";

const probeMirrorsMock = vi.fn();
const setMirrorPreferenceMock = vi.fn();
const fetchStatusMock = vi.fn();
const bootstrapEnvMock = vi.fn();
const toastOnlyMock = vi.fn();

const busyState = reactive<Record<string, boolean>>({ env: false });
const globalEnvStatus = ref(null);

vi.mock("@/api", () => ({
  environmentApi: {
    probeMirrors: (...args: unknown[]) => probeMirrorsMock(...args),
    setMirrorPreference: (...args: unknown[]) => setMirrorPreferenceMock(...args),
    fetchStatus: (...args: unknown[]) => fetchStatusMock(...args),
    bootstrap: vi.fn(),
  },
  ocrApi: {},
}));

vi.mock("@/composables/useEnvironment", () => ({
  useEnvironment: () => ({
    envStatus: globalEnvStatus,
    envLoading: ref(false),
    envError: ref(null),
    refreshEnv: vi.fn(),
    bootstrapEnv: (...args: unknown[]) => bootstrapEnvMock(...args),
  }),
}));

vi.mock("@/composables/useLogs", () => ({
  useLogs: () => ({
    logs: [],
    logFilter: {},
    filteredLogs: [],
    newLogCount: ref(0),
    initialized: ref(true),
    fetchLogs: vi.fn(),
    appendLogs: vi.fn(),
    markAtBottom: vi.fn(),
    clearLogs: vi.fn(),
  }),
}));

vi.mock("@/composables/useStatus", () => ({
  useStatus: () => ({ busy: busyState }),
}));

vi.mock("@/composables/useToast", () => ({
  useToast: () => ({ toastOnly: toastOnlyMock }),
}));

vi.mock("@/api/client", () => ({
  extractApiError: (e: unknown, fallback: string) =>
    (e as { message?: string })?.message ?? fallback,
}));

// Modal 轻桩：直接渲染插槽，免 reka Dialog 依赖（行为归 Modal 自己的测试）
vi.mock("@/components/common/Modal.vue", () => ({
  default: {
    template: "<div class=\"modal-stub\"><slot /><slot name=\"footer\" /></div>",
  },
}));

const PROBE_RESPONSE = {
  selected: { choice: "auto" },
  results: [
    { kind: "direct", prefix: "", label: "github.com", ok: true, bytes: 262144, elapsed_ms: 500, throughput: 200704 },
    { kind: "mirror", prefix: "https://gh-proxy.com/", label: "gh-proxy.com", ok: true, bytes: 262144, elapsed_ms: 1340, throughput: 195546 },
    { kind: "mirror", prefix: "https://ghfast.top/", label: "ghfast.top", ok: false, bytes: 0, elapsed_ms: 0, throughput: 0 },
  ],
};

let app: App | null = null;

function mountDialog(props: { autoStart?: boolean } = {}): void {
  const container = document.createElement("div");
  document.body.appendChild(container);
  app = createApp(EnvironmentInstallDialog, { open: true, ...props });
  app.mount(container);
}

function flush(): Promise<void> {
  return nextTick().then(() => nextTick());
}

/** 宏任务若干拍：probe/保存是异步请求，单微任务等不到 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
  await flush();
}

beforeEach(() => {
  probeMirrorsMock.mockReset().mockResolvedValue(PROBE_RESPONSE);
  setMirrorPreferenceMock.mockReset().mockResolvedValue({
    selected: { choice: "mirror", mirror: "https://gh-proxy.com/" },
  });
  fetchStatusMock.mockReset().mockResolvedValue(null);
  bootstrapEnvMock.mockReset().mockResolvedValue(true);
  toastOnlyMock.mockReset();
  busyState.env = false;
  globalEnvStatus.value = null;
});

function findButton(text: string): HTMLButtonElement {
  const target = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.includes(text),
  );
  expect(target, `找不到按钮「${text}」`).not.toBeNull();
  return target!;
}

describe("EnvironmentInstallDialog", () => {
  it("打开即渲染一排下载源选项，吞吐按人性化单位展示", async () => {
    mountDialog();
    await settle();

    expect(probeMirrorsMock).toHaveBeenCalledTimes(1);
    // 一排选项：自动测速 + 直连 + 两个镜像（含不可用源）
    const names = Array.from(document.querySelectorAll(".mirror-opt .mirror-opt-name")).map(
      (el) => el.textContent,
    );
    expect(names).toEqual(["自动测速", "GitHub 直连", "gh-proxy.com", "ghfast.top"]);
    const states = Array.from(document.querySelectorAll(".mirror-opt .mirror-opt-state")).map(
      (el) => el.textContent,
    );
    expect(states[1]).toContain("196 KB/s");
    expect(states[2]).toContain("191 KB/s");
    expect(states[3]).toBe("不可用");
    // 默认选中自动测速
    const auto = findButton("自动测速");
    expect(auto.getAttribute("aria-checked")).toBe("true");
    app?.unmount();
  });

  it("点击镜像选项保存偏好并更新选中态", async () => {
    mountDialog();
    await settle();

    findButton("gh-proxy.com").click();
    await settle();

    expect(setMirrorPreferenceMock).toHaveBeenCalledWith({
      choice: "mirror",
      mirror: "https://gh-proxy.com/",
    });
    expect(toastOnlyMock).toHaveBeenCalledWith(true, expect.stringContaining("下载源已保存"));
    // 选中态跟随服务端返回更新
    const ghproxy = findButton("gh-proxy.com");
    expect(ghproxy.getAttribute("aria-checked")).toBe("true");
    expect(findButton("自动测速").getAttribute("aria-checked")).toBe("false");
    app?.unmount();
  });

  it("autoStart 打开即触发初始化；busy 占用时不得重复触发", async () => {
    mountDialog({ autoStart: true });
    await settle();
    expect(bootstrapEnvMock).toHaveBeenCalledTimes(1);
    app?.unmount();

    busyState.env = true;
    mountDialog({ autoStart: true });
    await settle();
    expect(bootstrapEnvMock).toHaveBeenCalledTimes(1);
    app?.unmount();
  });
});
