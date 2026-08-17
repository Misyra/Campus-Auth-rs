/**
 * 确认对话框（单例）。
 * 替代原生 confirm()。组件 <ConfirmDialog /> 读取状态并调用 resolveConfirm。
 */

import { reactive } from "vue";

export interface ConfirmOptions {
  title?: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
}

interface ConfirmState {
  visible: boolean;
  title: string;
  message: string;
  confirmText: string;
  cancelText: string;
  danger: boolean;
}

const state = reactive<ConfirmState>({
  visible: false,
  title: "",
  message: "",
  confirmText: "确定",
  cancelText: "取消",
  danger: false,
});

let resolver: ((value: boolean | null) => void) | null = null;

/**
 * 弹出确认框。
 *
 * 返回值含义：
 * - true：用户点击确认
 * - false：用户点击取消（含点击遮罩）
 * - null：被新的 confirm 抢占（既不是确认也不是取消）。
 *   调用方语义处理：对"确认才执行危险操作"的场景按取消处理（安全默认）；
 *   对"取消才执行放弃/回退操作"的场景（dirty 丢弃、路由离开守卫）必须不做任何事，保留现状。
 */
function confirm(options: ConfirmOptions): Promise<boolean | null> {
  state.title = options.title || "确认操作";
  state.message = options.message;
  state.confirmText = options.confirmText || "确定";
  state.cancelText = options.cancelText || "取消";
  state.danger = options.danger || false;
  state.visible = true;
  return new Promise((resolve) => {
    // 并发 confirm：先前挂起的 Promise 先以 null 结算（被抢占≠用户取消），避免其永挂
    if (resolver) resolver(null);
    resolver = resolve;
  });
}

function resolveConfirm(value: boolean): void {
  state.visible = false;
  if (resolver) {
    resolver(value);
    resolver = null;
  }
}

export function useConfirm() {
  return { confirmState: state, confirm, resolveConfirm };
}
