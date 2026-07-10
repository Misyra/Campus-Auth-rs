/**
 * 密码字段状态机（修复 P2-12.11）。
 * 内部管理 已保存 / 编辑中 / 明文 三态，避免掩码串被误当作真实密码发送。
 */

import { ref, computed } from "vue";

export function usePasswordField(initialSaved = false) {
  const saved = ref(initialSaved);
  const editing = ref(false);
  const value = ref("");

  function onFocus(): void {
    if (saved.value) editing.value = true;
  }

  function onBlur(): void {
    if (!value.value) editing.value = false;
  }

  /** 输入框显示值：已保存且未编辑时显示掩码，否则显示明文 */
  const display = computed(() =>
    saved.value && !editing.value ? "••••••••••" : value.value,
  );

  /**
   * 计算提交给后端的值：
   * - 已保存且未编辑 → null（不修改密码）
   * - 否则 → 明文值（空串表示清空）
   */
  function submitValue(): string | null {
    if (saved.value && !editing.value) return null;
    return value.value;
  }

  /** 保存成功后调用：清空明文、退出编辑态，并记录已保存状态 */
  function markSaved(submitted: boolean): void {
    if (submitted) saved.value = true;
    value.value = "";
    editing.value = false;
  }

  /** 外部重置（如重新加载配置） */
  function reset(nextSaved: boolean): void {
    saved.value = nextSaved;
    editing.value = false;
    value.value = "";
  }

  return { saved, editing, value, display, onFocus, onBlur, submitValue, markSaved, reset };
}
