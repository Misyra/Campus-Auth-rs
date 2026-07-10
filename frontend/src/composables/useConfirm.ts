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

let resolver: ((value: boolean) => void) | null = null;

function confirm(options: ConfirmOptions): Promise<boolean> {
  state.title = options.title || "确认操作";
  state.message = options.message;
  state.confirmText = options.confirmText || "确定";
  state.cancelText = options.cancelText || "取消";
  state.danger = options.danger || false;
  state.visible = true;
  return new Promise((resolve) => {
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
