/**
 * 外观自定义颜色（单例）。
 * 从 useAppearance 拆出：自定义色新增/删除/取色/长按删除与颜色列表拼装。
 * 需要读写 appearance 当前色并跟随有效主题，经 useAppearance() 单例获取。
 */

import { reactive, watch } from "vue";
import {
  DEFAULT_APPEARANCE,
  DEFAULT_CUSTOM_COLORS,
  ACCENT_COLORS,
  DARK_BG_COLORS,
  LIGHT_BG_COLORS,
} from "../utils/constants";
import type { Appearance, CustomColors } from "../utils/appearance-types";
import { loadStored } from "../utils/storage";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { useAppearance } from "./useAppearance";

const customColors = reactive<CustomColors>(
  loadStored<CustomColors>("appearance.custom_colors", {
    accent: [],
    bg: [],
    sidebar: [],
    sidebar_accent: [],
  }),
);

function saveStoredColors(): void {
  localStorage.setItem("appearance.custom_colors", JSON.stringify(customColors));
}

/** 自定义颜色类型 → Appearance 上的当前色字段：removeCustomColor 回落默认值与 onCustomColorPicked 应用新色共用一份映射 */
const COLOR_TYPE_TO_APPEARANCE_FIELD: Record<keyof CustomColors, keyof Appearance> = {
  accent: "accent_color",
  bg: "background_color",
  sidebar: "sidebar_color",
  sidebar_accent: "sidebar_accent",
};

watch(customColors, () => {
  saveStoredColors();
}, { deep: true });

/** 新增自定义颜色：与系统色及已有自定义色去重（大小写不敏感），避免列表出现等值重复项 */
function addCustomColor(type: keyof CustomColors, hex: string): void {
  if (!hex || !Object.hasOwn(DEFAULT_CUSTOM_COLORS, type)) return;
  const lower = hex.toLowerCase();
  const systemColors =
    type === "accent"
      ? ACCENT_COLORS
      : type === "bg"
        ? [...DARK_BG_COLORS, ...LIGHT_BG_COLORS]
        : [];
  if (systemColors.some((c) => c.value.toLowerCase() === lower)) return;
  if (customColors[type].some((c) => c.toLowerCase() === lower)) return;
  customColors[type].push(lower);
  saveStoredColors();
}

/** 删除自定义颜色；若当前正在使用该色则一并回落到默认值，避免界面残留已删除的色值 */
function removeCustomColor(type: keyof CustomColors, hex: string): void {
  if (!Object.hasOwn(DEFAULT_CUSTOM_COLORS, type)) return;
  const idx = customColors[type].findIndex((c) => c.toLowerCase() === hex.toLowerCase());
  if (idx === -1) return;
  customColors[type].splice(idx, 1);
  saveStoredColors();
  const { appearance } = useAppearance();
  const defaultKey = COLOR_TYPE_TO_APPEARANCE_FIELD[type];
  if (String(appearance[defaultKey as keyof Appearance] || "").toLowerCase() === hex.toLowerCase()) {
    (appearance as Record<string, unknown>)[defaultKey] = DEFAULT_APPEARANCE[defaultKey as keyof Appearance];
  }
}

/** 触发对应类型的隐藏取色器（原生 input[type=color]），保持模板零侵入 */
function pickCustomColor(type: keyof CustomColors): void {
  const input = document.querySelector<HTMLInputElement>(`input[data-color-picker="${type}"]`);
  input?.click();
}

/** 取色器选中回调：入库自定义色并立即应用为当前色；随后清空 input 便于再次取同色也能触发 change */
function onCustomColorPicked(type: keyof CustomColors, event: Event): void {
  const hex = (event.target as HTMLInputElement).value;
  addCustomColor(type, hex);
  const { appearance } = useAppearance();
  (appearance as Record<string, unknown>)[COLOR_TYPE_TO_APPEARANCE_FIELD[type]] = hex;
  (event.target as HTMLInputElement).value = "#000000";
}

/** 长按生效后的删除确认（移动端无右键/悬停，长按是唯一的删除入口） */
function onColorLongPress(type: keyof CustomColors, hex: string): void {
  const { confirm } = useConfirm();
  void confirm({
    title: "删除自定义颜色",
    message: `删除自定义颜色 ${hex}？`,
  }).then((ok) => {
    if (ok) removeCustomColor(type, hex);
  });
}

/**
 * 触摸长按入口：按住 600ms 不松开（也不滑动）才视为长按，触发删除确认。
 * 松开或滑动即取消计时器——600ms 阈值用于区分"点击选色"与"长按删除"两种手势。
 */
function startLongPress(type: keyof CustomColors, hex: string, event: TouchEvent): void {
  event.preventDefault();
  const target = event.target as EventTarget;
  const timer = setTimeout(() => onColorLongPress(type, hex), 600);
  const cancel = () => {
    clearTimeout(timer);
    target.removeEventListener("touchend", cancel);
    target.removeEventListener("touchmove", cancel);
  };
  target.addEventListener("touchend", cancel);
  target.addEventListener("touchmove", cancel);
}

/** 拼装选色列表：系统色在前、自定义色在后；背景色按当前有效主题取深/浅色板 */
function getColorList(type: keyof CustomColors): { value: string; label: string; custom?: boolean }[] {
  let systemColors: { value: string; label: string }[] = [];
  if (type === "bg") {
    systemColors = useAppearance().getEffectiveTheme() === "dark" ? DARK_BG_COLORS : LIGHT_BG_COLORS;
  } else if (type === "accent") {
    systemColors = ACCENT_COLORS;
  }
  const custom = (customColors[type] || []).map((hex) => ({ value: hex, label: hex, custom: true }));
  return [...systemColors, ...custom];
}

export function useCustomColors() {
  return {
    customColors: customColors as CustomColors,
    addCustomColor,
    removeCustomColor,
    pickCustomColor,
    onCustomColorPicked,
    onColorLongPress,
    startLongPress,
    getColorList,
  };
}
