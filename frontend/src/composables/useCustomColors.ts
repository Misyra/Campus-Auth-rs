/**
 * 外观自定义颜色（单例）。
 * 从 useAppearance 拆出：自定义色新增/删除/取色与颜色列表拼装。
 * 需要读写 appearance 当前色并跟随有效主题，经 useAppearance() 单例获取。
 */

import { reactive, watch } from "vue";
import {
  DEFAULT_APPEARANCE,
  DEFAULT_CUSTOM_COLORS,
  ACCENT_COLORS,
  SIDEBAR_ACCENT_COLORS,
  DARK_BG_COLORS,
  LIGHT_BG_COLORS,
} from "../utils/constants";
import type { Appearance, CustomColors } from "../utils/appearance-types";
import { loadStored, saveStored } from "../utils/storage";
import { normalizeCustomColors, validHex } from "../utils/appearance";
import { useConfirm } from "./useConfirm";
import { useAppearance } from "./useAppearance";

const customColors = reactive<CustomColors>(
  normalizeCustomColors(loadStored<CustomColors>("appearance.custom_colors", {
    accent: [],
    bg: [],
    sidebar: [],
    sidebar_accent: [],
  })),
);

function saveStoredColors(): void {
  saveStored("appearance.custom_colors", customColors);
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
  if (!validHex(hex) || !Object.hasOwn(DEFAULT_CUSTOM_COLORS, type)) return;
  const lower = hex.toLowerCase();
  const systemColors =
    type === "accent"
      ? ACCENT_COLORS
      : type === "sidebar_accent"
        ? SIDEBAR_ACCENT_COLORS
        : type === "bg" || type === "sidebar"
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

/** 取色器选中后立即应用；保留输入值，下次打开仍从当前色开始。 */
function onCustomColorPicked(type: keyof CustomColors, event: Event): void {
  const hex = (event.target as HTMLInputElement).value;
  if (!validHex(hex)) return;
  addCustomColor(type, hex);
  const { appearance } = useAppearance();
  (appearance as Record<string, unknown>)[COLOR_TYPE_TO_APPEARANCE_FIELD[type]] = hex;
}

/** 删除色板前确认，管理按钮与右键入口共用。 */
function onColorLongPress(type: keyof CustomColors, hex: string): void {
  const { confirm } = useConfirm();
  void confirm({
    title: "删除自定义颜色",
    message: `删除自定义颜色 ${hex}？`,
  }).then((ok) => {
    if (ok) removeCustomColor(type, hex);
  });
}

/** 拼装选色列表：系统色在前、自定义色在后；背景色按当前有效主题取深/浅色板 */
function getColorList(type: keyof CustomColors): { value: string; label: string; custom?: boolean }[] {
  let systemColors: { value: string; label: string }[] = [];
  if (type === "bg" || type === "sidebar") {
    systemColors = useAppearance().getEffectiveTheme() === "dark" ? DARK_BG_COLORS : LIGHT_BG_COLORS;
  } else if (type === "accent") {
    systemColors = ACCENT_COLORS;
  } else if (type === "sidebar_accent") {
    systemColors = SIDEBAR_ACCENT_COLORS;
  }
  const custom = (customColors[type] || []).map((hex) => ({ value: hex, label: hex, custom: true }));
  return [...systemColors, ...custom];
}

export function useCustomColors() {
  return {
    pickCustomColor,
    onCustomColorPicked,
    onColorLongPress,
    getColorList,
  };
}
