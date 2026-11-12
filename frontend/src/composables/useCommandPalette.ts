/**
 * 命令面板（Ctrl+K）单例：开合状态与全局快捷键。
 *
 * 只有这一层进首屏 chunk（触发按钮与快捷键需要常驻）；命令集的构建与过滤
 * 在 commandRegistry.ts，随 CommandPalette.vue（defineAsyncComponent）一起
 * 懒加载。命令全部复用既有单例入口（不新增 API）。
 */
import { ref } from "vue";

const open = ref(false);

function openPalette(): void {
  open.value = true;
}

function closePalette(): void {
  open.value = false;
}

function togglePalette(): void {
  open.value = !open.value;
}

export function useCommandPalette() {
  return { open, openPalette, closePalette, togglePalette };
}

/**
 * 全局快捷键 Ctrl+K / Cmd+K（mac）。
 *
 * 设计取舍：**文本框聚焦时同样触发**——页面内文本框没有占用 Ctrl+K 的默认行为，
 * 打开面板后焦点由 reka 落到搜索框，关闭时 reka 归还焦点到原输入框，输入现场
 * 不丢失（不需要为输入框场景做豁免）。长按重复（event.repeat）与 alt+K 不触发，
 * 防止按住时开关闪烁。
 */
export function installCommandPaletteHotkey(): () => void {
  const handler = (event: KeyboardEvent): void => {
    if (event.repeat) return;
    if (event.key.toLowerCase() !== "k" || event.altKey) return;
    if (!event.ctrlKey && !event.metaKey) return;
    event.preventDefault();
    togglePalette();
  };
  window.addEventListener("keydown", handler);
  return () => window.removeEventListener("keydown", handler);
}
