/**
 * Toast 通知状态（单例）。
 * 替代原 uiMethods._showToast / toastOnly。
 */

import { reactive } from "vue";
import { TIMING } from "../utils/constants";
import { frontendLogger } from "../utils/logger";

interface ToastState {
  success: boolean;
  message: string;
  leaving: boolean;
}

const toast = reactive<ToastState>({
  success: true,
  message: "",
  leaving: false,
});

let toastTimer: ReturnType<typeof setTimeout> | undefined;
let toastLeavingTimer: ReturnType<typeof setTimeout> | undefined;

/**
 * 仅显示 toast（不记入通知历史）。
 *
 * 每条 toast 都会镜像进前端日志（经 WS 回流 app.log）：成功记 info、失败记
 * error，保证"用户看到的提示"与"日志里的记录"一致，事后排查有据可查。
 * `meta` 供调用方补充上下文（如通知中心传 category），随日志透出。
 */
function toastOnly(success: boolean, message: string, meta?: unknown): void {
  toast.success = success;
  toast.message = message;
  toast.leaving = false;
  if (success) frontendLogger.info("toast", message, meta);
  else frontendLogger.error("toast", message, meta);
  if (toastTimer) clearTimeout(toastTimer);
  if (toastLeavingTimer) clearTimeout(toastLeavingTimer);
  toastTimer = setTimeout(() => {
    toast.leaving = true;
    toastLeavingTimer = setTimeout(() => {
      toast.message = "";
      toast.leaving = false;
    }, TIMING.TOAST_LEAVE_DELAY);
  }, TIMING.TOAST_DURATION);
}

export function useToast() {
  return { toast, toastOnly };
}
