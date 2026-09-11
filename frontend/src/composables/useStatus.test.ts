/**
 * 状态快照映射与新鲜度防护的单元测试。
 * fetchStatus 是唯一诚实路径：mock monitorApi 返回后端原始快照，
 * 断言累计字段映射、过期轮询丢弃（B6/P14）与首败通知（F3）。
 *
 * useStatus 是模块级单例：用例共享 appliedVersion/appliedUptime，
 * 按声明顺序执行（先新鲜快照，再过期丢弃，最后失败通知）。
 */
import { describe, it, expect, vi } from "vitest";
import type { StatusSnapshot } from "../api/types";

const fetchStatusMock = vi.fn();
const fetchAutostartMock = vi.fn(async () => ({
  platform: "windows",
  enabled: true,
  method: "计划任务",
  location: "",
  runtime_mode: "full",
}));

vi.mock("../api", () => ({
  monitorApi: { fetchStatus: (...a: unknown[]) => fetchStatusMock(...a) },
  autostartApi: { fetchStatus: (...a: unknown[]) => fetchAutostartMock(...a) },
}));

const { useStatus } = await import("./useStatus");
const { useNotifications } = await import("./useNotifications");

const { status, autostart, networkStatus, networkStatusText, updateStatus, fetchStatus, fetchAutostart } =
  useStatus();
const { notifications } = useNotifications();

/** 后端原始快照形态（fetchStatus 入参，未经映射） */
function backendRaw(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    engine_state: "running",
    network_status: "online",
    probe_total: 7,
    login_total: 3,
    consecutive_failures: 1,
    retry_count: 2,
    monitoring_seconds: 10,
    uptime_seconds: 100,
    last_check_time: "2026-09-07 12:00:00",
    snapshot_version: 5,
    ...overrides,
  };
}

describe("fetchStatus 映射", () => {
  it("应用新鲜快照并映射累计字段", async () => {
    fetchStatusMock.mockResolvedValue(backendRaw());
    await fetchStatus();

    expect(status.monitoring).toBe(true);
    expect(status.network_connected).toBe(true);
    expect(status.network_check_count).toBe(7);
    expect(status.login_attempt_count).toBe(3);
    expect(status.runtime_seconds).toBe(100);
    expect(networkStatus.value).toBe("connected");
    expect(networkStatusText.value).toBe("公网连接正常");
  });

  it("旧版本轮询响应被丢弃，不回退状态", async () => {
    fetchStatusMock.mockResolvedValue(
      backendRaw({ snapshot_version: 4, network_status: "offline", probe_total: 99 }),
    );
    await fetchStatus();

    expect(status.network_state).toBe("online");
    expect(status.network_check_count).toBe(7);
  });

  it("无版本号旧后端按 uptime 比较：更早丢弃更新应用", async () => {
    fetchStatusMock.mockResolvedValue(
      backendRaw({ snapshot_version: undefined, uptime_seconds: 50, network_status: "offline" }),
    );
    await fetchStatus();
    expect(status.network_state).toBe("online");

    fetchStatusMock.mockResolvedValue(
      backendRaw({
        snapshot_version: undefined,
        uptime_seconds: 200,
        network_status: "offline",
        probe_total: 8,
      }),
    );
    await fetchStatus();
    expect(status.network_state).toBe("offline");
    expect(status.network_check_count).toBe(8);
    expect(networkStatusText.value).toBe("网络暂不可达");
  });
});

describe("updateStatus 权威推送", () => {
  it("版本号更旧也照单应用", () => {
    updateStatus(
      backendRaw({ snapshot_version: 1, network_status: "captive_portal", probe_total: 9 }) as unknown as Partial<StatusSnapshot>,
    );
    expect(status.network_state).toBe("captive_portal");
    expect(status.network_check_count).toBe(9);
    expect(networkStatusText.value).toBe("需要校园网认证");
  });

  it("暂停仅改变引擎行为，不覆盖最近一次网络事实", () => {
    updateStatus(
      backendRaw({
        snapshot_version: 2,
        network_status: "online",
        pause_active: true,
      }) as unknown as Partial<StatusSnapshot>,
    );
    expect(status.network_state).toBe("online");
    expect(networkStatus.value).toBe("checking");
    expect(networkStatusText.value).toBe("自动监测已暂停");
  });
});

describe("fetchAutostart 与失败通知", () => {
  it("合并自启动状态", async () => {
    await fetchAutostart();
    expect(autostart.enabled).toBe(true);
    expect(autostart.platform).toBe("windows");
  });

  it("首次失败通知一次，后续失败静默且状态不变", async () => {
    const before = notifications.length;
    fetchStatusMock.mockRejectedValueOnce(new Error("down"));
    await fetchStatus();
    expect(notifications.length).toBe(before + 1);
    expect(status.network_state).toBe("online");

    fetchStatusMock.mockRejectedValueOnce(new Error("down"));
    await fetchStatus();
    expect(notifications.length).toBe(before + 1);
  });
});
