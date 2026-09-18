/** 可见浏览器重定向检测：只验证能力，不修改认证地址或其他方案字段。 */
import { ref } from "vue";
import { monitorApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { RedirectTestResult } from "@/api/types";
import { useToast } from "@/composables/useToast";

export function useRedirectTest() {
  const { toastOnly } = useToast();
  const testing = ref(false);
  const result = ref<RedirectTestResult | null>(null);

  /** 启动一次可见浏览器检测；所有结果只展示，不回填或保存 URL。 */
  async function testRedirect(triggerUrl: string): Promise<RedirectTestResult | null> {
    if (testing.value) return null;
    testing.value = true;
    result.value = null;
    try {
      const next = await monitorApi.testRedirect(triggerUrl);
      result.value = next;
      toastOnly(next.status === "detected", next.message);
      return next;
    } catch (error) {
      toastOnly(false, extractApiError(error, "重定向检测失败"));
      return null;
    } finally {
      testing.value = false;
    }
  }

  return { testing, result, testRedirect };
}
