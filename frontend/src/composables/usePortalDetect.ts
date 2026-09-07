/**
 * 认证门户地址自动检测（可复用）。
 * 调用 POST /api/monitor/detect-portal：未认证时跟随 302 返回候选门户地址。
 * 检测只读返回，由调用方决定填入哪个输入框；保存仍由用户手动完成。
 */
import { ref } from "vue";
import { monitorApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { PortalDetectResult } from "@/api/types";
import { useToast } from "@/composables/useToast";

export function usePortalDetect() {
  const { toastOnly } = useToast();
  const detecting = ref(false);

  /**
   * 执行一次门户检测。成功抓到地址时返回候选 URL 并 toast 提示；
   * 其余结论（已在线/无跳转/断网）仅 toast 说明，返回 null。
   */
  async function detectPortal(): Promise<string | null> {
    if (detecting.value) return null;
    detecting.value = true;
    try {
      const result: PortalDetectResult = await monitorApi.detectPortal();
      if (result.status === "found" && result.portal_url) {
        toastOnly(true, result.message || "已检测到认证地址");
        return result.portal_url;
      }
      toastOnly(false, result.message || "未能检测到认证地址");
      return null;
    } catch (error) {
      toastOnly(false, extractApiError(error, "门户检测失败"));
      return null;
    } finally {
      detecting.value = false;
    }
  }

  return { detecting, detectPortal };
}
