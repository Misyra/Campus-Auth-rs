/**
 * 通知中心（单例）。
 * 替代原 uiMethods.notify / toggleNotifications，使用 useToast 显示即时提示。
 */

import { reactive, ref } from "vue";
import { TIMING } from "../utils/constants";
import type { NotificationAction, NotificationEntry } from "../api/types";
import { useToast } from "./useToast";

const NOTIFY_CATEGORY_LABELS: Record<string, string> = {
  login: "登录",
  monitor: "监控",
  network: "网络",
  update: "更新",
  security: "安全",
  install: "安装",
};

const notifications = reactive<NotificationEntry[]>([]);
const unreadNotifications = ref(0);
const showNotifications = ref(false);

function formatNotifyTime(): string {
  const now = new Date();
  const h = String(now.getHours()).padStart(2, "0");
  const m = String(now.getMinutes()).padStart(2, "0");
  const s = String(now.getSeconds()).padStart(2, "0");
  return `${now.getMonth() + 1}/${now.getDate()} ${h}:${m}:${s}`;
}

function notify(
  success: boolean,
  message: string,
  category?: string,
  action?: NotificationAction | null,
): void {
  const entry: NotificationEntry = {
    success,
    message,
    time: formatNotifyTime(),
    category: category || "",
    icon: "",
    label: NOTIFY_CATEGORY_LABELS[category || ""] || "",
    action: action || null,
  };
  notifications.unshift(entry);
  if (notifications.length > TIMING.NOTIFICATION_MAX) {
    notifications.length = TIMING.NOTIFICATION_MAX;
  }
  unreadNotifications.value++;
  const { toastOnly } = useToast();
  toastOnly(success, message);
}

function toggleNotifications(): void {
  showNotifications.value = !showNotifications.value;
  if (showNotifications.value) {
    unreadNotifications.value = 0;
  }
}

export function useNotifications() {
  return { notifications, unreadNotifications, showNotifications, notify, toggleNotifications };
}
