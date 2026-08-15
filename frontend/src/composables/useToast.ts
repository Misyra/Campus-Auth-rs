/**
 * Toast 通知状态（单例）。
 * 替代原 uiMethods._showToast / toastOnly。
 */

import { reactive } from "vue";
import { TIMING } from "../utils/constants";

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

/** 仅显示 toast（不记入通知历史） */
function toastOnly(success: boolean, message: string): void {
  toast.success = success;
  toast.message = message;
  toast.leaving = false;
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
