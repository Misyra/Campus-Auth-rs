/**
 * 通用异步变更封装。
 * 修复 P0-12.3：统一 loading / error / success 状态与 toast 提示，消除 ~30 处重复模板。
 */

import { ref } from "vue";
import { extractApiError } from "../api/client";
import { useToast } from "./useToast";

interface MutationOptions {
  /** 成功时展示的 toast（不传则不弹成功 toast） */
  successMessage?: string;
  /** 失败回退消息（错误本身无消息时使用） */
  failureMessage?: string;
  /** 是否自动 toast（默认 true） */
  toast?: boolean;
}

export function useMutation<Args extends unknown[], R>(
  fn: (...args: Args) => Promise<R>,
  options: MutationOptions = {},
) {
  const loading = ref(false);
  const error = ref<string | null>(null);
  const data = ref<R | null>(null);

  async function mutate(...args: Args): Promise<R | null> {
    loading.value = true;
    error.value = null;
    const toastEnabled = options.toast !== false;
    try {
      const result = await fn(...args);
      data.value = result as R;
      if (toastEnabled && options.successMessage) {
        const { toastOnly } = useToast();
        toastOnly(true, options.successMessage);
      }
      return result;
    } catch (e) {
      const msg = extractApiError(e, options.failureMessage || "操作失败");
      error.value = msg;
      if (toastEnabled) {
        const { toastOnly } = useToast();
        toastOnly(false, msg);
      }
      return null;
    } finally {
      loading.value = false;
    }
  }

  return { loading, error, data, mutate };
}
