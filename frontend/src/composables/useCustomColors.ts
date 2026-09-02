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
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { useAppearance } from "./useAppearance";

function loadStored<T>(key: string, fallback: T): T {
  const saved = localStorage.getItem(key);
  if (!saved) return fallback;
  try {
    return { ...(fallback as object), ...JSON.parse(saved) } as T;
  } catch (error) {
    frontendLogger.debug("appearance", "本地外观配置损坏，已重置", error);
    localStorage.removeItem(key);
    return fallback;
  }
}

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

watch(customColors, () => {
  saveStoredColors();
}, { deep: true });

function addCustomColor(type: keyof CustomColors, hex: string): void {
  if (!hex || !DEFAULT_CUSTOM_COLORS.hasOwnProperty(type)) return;
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

function removeCustomColor(type: keyof CustomColors, hex: string): void {
  if (!DEFAULT_CUSTOM_COLORS.hasOwnProperty(type)) return;
  const idx = customColors[type].findIndex((c) => c.toLowerCase() === hex.toLowerCase());
  if (idx === -1) return;
  customColors[type].splice(idx, 1);
  saveStoredColors();
  const { appearance } = useAppearance();
  const defaultKey =
    type === "accent"
      ? "accent_color"
      : type === "bg"
        ? "background_color"
        : type === "sidebar"
          ? "sidebar_color"
          : "sidebar_accent";
  if (String(appearance[defaultKey as keyof Appearance] || "").toLowerCase() === hex.toLowerCase()) {
    (appearance as Record<string, unknown>)[defaultKey] = DEFAULT_APPEARANCE[defaultKey as keyof Appearance];
  }
}

function pickCustomColor(type: keyof CustomColors): void {
  const input = document.querySelector<HTMLInputElement>(`input[data-color-picker="${type}"]`);
  input?.click();
}

function onCustomColorPicked(type: keyof CustomColors, event: Event): void {
  const hex = (event.target as HTMLInputElement).value;
  addCustomColor(type, hex);
  const { appearance } = useAppearance();
  const fieldMap: Record<string, keyof Appearance> = {
    accent: "accent_color",
    bg: "background_color",
    sidebar: "sidebar_color",
    sidebar_accent: "sidebar_accent",
  };
  (appearance as Record<string, unknown>)[fieldMap[type]] = hex;
  (event.target as HTMLInputElement).value = "#000000";
}

function onColorLongPress(type: keyof CustomColors, hex: string): void {
  const { confirm } = useConfirm();
  void confirm({
    title: "删除自定义颜色",
    message: `删除自定义颜色 ${hex}？`,
  }).then((ok) => {
    if (ok) removeCustomColor(type, hex);
  });
}

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
