/**
 * 确认对话框（单例）。
 * 替代原生 confirm()。组件 <ConfirmDialog /> 读取状态并调用 resolveConfirm。
 */

import { reactive } from "vue";

/**
 * 一处「旧值 → 新值」改动。
 *
 * 供确认框以结构化列表呈现（而非把整段内容拼成字符串塞进 message）：
 * 拼接的文本无法给字段名与取值分别着色，也做不出对齐与视觉指向，
 * 多项改动时会退化成一团难读的文字。
 */
export interface ConfirmChange {
  /** 改动项的显示名（如「浏览器后台运行」） */
  label: string;
  /** 变更前的可读值 */
  from: string;
  /** 变更后的可读值 */
  to: string;
}

export interface ConfirmOptions {
  title?: string;
  message: string;
  /** 可选的结构化改动清单；有值时渲染为对齐列表，message 只作引导语 */
  changes?: ConfirmChange[];
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
}

interface ConfirmState {
  visible: boolean;
  title: string;
  message: string;
  changes: ConfirmChange[];
  confirmText: string;
  cancelText: string;
  danger: boolean;
}

const state = reactive<ConfirmState>({
  visible: false,
  title: "",
  message: "",
  changes: [],
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
  // 拷一份：调用方常在 computed/循环里构造数组，共享引用会被后续渲染改动
  state.changes = options.changes ? options.changes.map((c) => ({ ...c })) : [];
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
