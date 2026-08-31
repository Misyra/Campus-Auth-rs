/**
 * Python 环境状态与初始化（uv sync + Chromium，单例）。
 * 后端经 BootstrapGate 保证并发幂等；前端用 busy.env 串行化按钮。
 */

import { ref } from "vue";
import type { EnvironmentStatus } from "../api/types";
import { environmentApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useToast } from "./useToast";

const envStatus = ref<EnvironmentStatus | null>(null);
const envLoading = ref(false);
const envError = ref<string | null>(null);

async function refreshEnv(): Promise<void> {
  envLoading.value = true;
  envError.value = null;
  try {
    const data = await environmentApi.fetchStatus();
    envStatus.value = data;
  } catch (e) {
    envError.value = extractApiError(e, "环境状态检测失败");
    frontendLogger.warn("environment", "获取环境状态失败", e);
  } finally {
    envLoading.value = false;
  }
}

async function bootstrapEnv(): Promise<boolean> {
  const { busy } = useStatus();
  if (busy.env) return false;
  busy.env = true;
  const { toastOnly } = useToast();
  try {
    const res = await environmentApi.bootstrap();
    // 后端同步等待完成，直接可得最新状态
    await refreshEnv();
    const ok = Boolean((res as unknown as { capability_ready?: boolean }).capability_ready ?? envStatus.value?.capability_ready);
    if (ok) toastOnly(true, "Python 环境初始化完成");
    else toastOnly(false, envStatus.value?.last_error || "环境初始化完成但仍未就绪，请查看日志");
    return Boolean(ok);
  } catch (e) {
    const msg = extractApiError(e, "环境初始化失败");
    envError.value = msg;
    // 失败后刷新一次以拿到后端 last_error
    try { await refreshEnv(); } catch { /* */ }
    const { notify } = await import("./useNotifications").then(m => ({ notify: m.useNotifications().notify }));
    // 优先 toast，降级 notify
    try { useToast().toastOnly(false, msg); } catch { notify(false, msg, "environment"); }
    frontendLogger.error("environment", msg, e);
    return false;
  } finally {
    busy.env = false;
  }
}

export function useEnvironment() {
  return { envStatus, envLoading, envError, refreshEnv, bootstrapEnv };
}
