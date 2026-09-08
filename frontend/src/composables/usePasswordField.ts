/**
 * 密码字段状态机（修复 P2-12.11）。
 * 内部管理 已保存 / 编辑中 / 明文 三态，避免掩码串被误当作真实密码发送。
 */

import { ref, computed } from "vue";

export function usePasswordField(initialSaved = false) {
  const saved = ref(initialSaved);
  const editing = ref(false);
  const value = ref("");
  // 清除旧密码必须显式触发，避免普通输入框失焦后的“留空即保留”习惯被破坏。
  const clearRequested = ref(false);

  function onFocus(): void {
    if (saved.value) editing.value = true;
  }

  function onBlur(): void {
    if (!value.value) editing.value = false;
  }

  /** 同步用户输入；输入新密码会覆盖此前尚未保存的清除请求。 */
  function setValue(nextValue: string): void {
    value.value = nextValue;
    if (nextValue) clearRequested.value = false;
  }

  /** 请求在下次保存时清除已保存的密码。 */
  function clear(): void {
    value.value = "";
    editing.value = true;
    clearRequested.value = true;
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
    if (clearRequested.value) return "";
    if (saved.value && !editing.value) return null;
    return value.value;
  }

  /** 保存成功后调用：清空明文、退出编辑态，并记录已保存状态 */
  function markSaved(hasPassword: boolean): void {
    saved.value = hasPassword;
    value.value = "";
    editing.value = false;
    clearRequested.value = false;
  }

  /** 外部重置（如重新加载配置） */
  function reset(nextSaved: boolean): void {
    saved.value = nextSaved;
    editing.value = false;
    value.value = "";
    clearRequested.value = false;
  }

  return { saved, editing, value, display, onFocus, onBlur, setValue, clear, submitValue, markSaved, reset };
}
