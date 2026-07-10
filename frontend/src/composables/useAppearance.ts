/**
 * 外观与主题（单例）。
 * 替代原 appearanceData + appearanceMethods + applyAppearance。
 * 从 localStorage 加载/持久化，并应用到文档 CSS 变量。
 */

import { reactive, watch } from "vue";
import {
  DEFAULT_APPEARANCE,
  DEFAULT_CUSTOM_COLORS,
  ACCENT_COLORS,
  DARK_BG_COLORS,
  LIGHT_BG_COLORS,
  LIMITS,
} from "../utils/constants";
import type { Appearance, CustomColors } from "../utils/appearance-types";
import { hexToRgb, adjustColor } from "../utils/formatters";
import { pickFile } from "../utils/file";
import { backgroundApi } from "../api";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";

function loadStored<T>(key: string, fallback: T): T {
  const saved = localStorage.getItem(key);
  if (!saved) return fallback;
  try {
    return { ...(fallback as object), ...JSON.parse(saved) } as T;
  } catch {
    localStorage.removeItem(key);
    return fallback;
  }
}

const appearance = reactive<Appearance>(
  loadStored<Appearance>("appearance", { ...DEFAULT_APPEARANCE }),
);
const customColors = reactive<CustomColors>(
  loadStored<CustomColors>("appearance.custom_colors", {
    accent: [],
    bg: [],
    sidebar: [],
    sidebar_accent: [],
  }),
);
const randomWallpaperDialog = reactive({ visible: false, url: "", loading: false });
const bgLightbox = reactive({ visible: false });

const { toastOnly } = useToast();

function saveStoredAppearance(): void {
  localStorage.setItem("appearance", JSON.stringify(appearance));
}
function saveStoredColors(): void {
  localStorage.setItem("appearance.custom_colors", JSON.stringify(customColors));
}

// 外观变更时自动应用 + 持久化（实时预览，无需手动点保存）
watch(appearance, () => {
  applyAppearance();
  saveStoredAppearance();
}, { deep: true });

watch(customColors, () => {
  saveStoredColors();
}, { deep: true });

function getEffectiveTheme(): "light" | "dark" {
  const themeMode = appearance.theme || "light";
  if (themeMode === "auto") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return themeMode;
}

/** 应用外观设置到页面 CSS 变量 */
function applyAppearance(): void {
  const root = document.documentElement;
  const body = document.body;

  if (appearance.background_url) {
    body.style.setProperty("--bg-image", `url(${appearance.background_url})`);
    body.style.setProperty("--bg-blur", `blur(${appearance.background_blur}px)`);
    body.style.setProperty("--bg-opacity", String(appearance.background_opacity));
    body.classList.add("has-custom-bg");
  } else {
    body.classList.remove("has-custom-bg");
    body.style.removeProperty("--bg-image");
    body.style.removeProperty("--bg-blur");
    body.style.removeProperty("--bg-opacity");
  }

  if (appearance.backdrop_filter) {
    body.classList.remove("no-backdrop-filter");
  } else {
    body.classList.add("no-backdrop-filter");
  }

  if (appearance.accent_color) {
    root.style.setProperty("--accent", appearance.accent_color);
    root.style.setProperty("--accent-hover", adjustColor(appearance.accent_color, -20));
    const accentRgb = hexToRgb(appearance.accent_color);
    if (accentRgb) {
      root.style.setProperty("--accent-rgb", `${accentRgb.r}, ${accentRgb.g}, ${accentRgb.b}`);
    }
  }

  const isLight = getEffectiveTheme() === "light";
  root.setAttribute("data-theme", isLight ? "light" : "dark");
  const _p = (k: string, v: string) => root.style.setProperty(k, v);

  if (isLight) {
    if (appearance.background_color) {
      const bgRgb = hexToRgb(appearance.background_color);
      if (bgRgb) {
        _p("--bg-primary", appearance.background_color);
        _p("--bg-secondary", `rgb(${Math.min(bgRgb.r + 15, 255)}, ${Math.min(bgRgb.g + 15, 255)}, ${Math.min(bgRgb.b + 15, 255)})`);
      }
    } else {
      _p("--bg-primary", "#eef2f7");
      _p("--bg-secondary", "#e4e9f0");
    }
  } else if (appearance.background_color) {
    const bgRgb = hexToRgb(appearance.background_color);
    if (bgRgb) {
      _p("--bg-primary", appearance.background_color);
      _p("--bg-secondary", `rgb(${Math.min(bgRgb.r + 15, 255)}, ${Math.min(bgRgb.g + 15, 255)}, ${Math.min(bgRgb.b + 15, 255)})`);
    }
  } else {
    _p("--bg-primary", "#0f172a");
    _p("--bg-secondary", "#1e293b");
  }

  const co = appearance.card_opacity;
  const blurPx = appearance.card_blur ?? 12;
  _p("--card-blur", appearance.backdrop_filter && blurPx > 0 ? `blur(${blurPx}px)` : "none");
  if (isLight) {
    _p("--bg-card", `rgba(255, 255, 255, ${co})`);
  } else {
    const cardRgb = hexToRgb(appearance.background_color || "#0f172a");
    if (cardRgb) {
      _p("--bg-card", `rgba(${cardRgb.r}, ${cardRgb.g}, ${cardRgb.b}, ${co})`);
    }
  }

  const bi = appearance.border_intensity;
  if (isLight) {
    _p("--border", `rgba(100, 116, 139, ${0.12 * bi})`);
    _p("--border-hover", `rgba(100, 116, 139, ${0.22 * bi})`);
    _p("--border-accent", `rgba(56, 189, 248, ${0.15 * bi})`);
  } else {
    _p("--border", `rgba(148, 163, 184, ${0.1 * bi})`);
    _p("--border-hover", `rgba(148, 163, 184, ${0.2 * bi})`);
    _p("--border-accent", `rgba(56, 189, 248, ${0.15 * bi})`);
  }

  _p("--sidebar-opacity", String(appearance.sidebar_opacity));

  if (appearance.sidebar_color) {
    const sidebarRgb = hexToRgb(appearance.sidebar_color);
    if (sidebarRgb) {
      _p("--sidebar-bg-1", `rgba(${sidebarRgb.r}, ${sidebarRgb.g}, ${sidebarRgb.b}, var(--sidebar-opacity))`);
      _p("--sidebar-bg-2", `rgba(${sidebarRgb.r}, ${sidebarRgb.g}, ${sidebarRgb.b}, calc(var(--sidebar-opacity) + 0.03))`);
    }
  } else {
    const bgRgb = hexToRgb(appearance.background_color || (isLight ? "#dfe4ec" : "#0f172a"));
    if (bgRgb) {
      _p("--sidebar-bg-1", `rgba(${Math.min(bgRgb.r + 15, 255)}, ${Math.min(bgRgb.g + 15, 255)}, ${Math.min(bgRgb.b + 15, 255)}, var(--sidebar-opacity))`);
      _p("--sidebar-bg-2", `rgba(${Math.max(bgRgb.r - 10, 0)}, ${Math.max(bgRgb.g - 10, 0)}, ${Math.max(bgRgb.b - 10, 0)}, calc(var(--sidebar-opacity) + 0.03))`);
    }
  }

  if (appearance.sidebar_accent) {
    _p("--sidebar-accent", appearance.sidebar_accent);
  } else {
    root.style.removeProperty("--sidebar-accent");
  }
}

function saveAppearance(): void {
  saveStoredAppearance();
  applyAppearance();
  toastOnly(true, "外观设置已保存");
}

function resetAppearance(): void {
  Object.assign(appearance, DEFAULT_APPEARANCE);
  Object.assign(customColors, DEFAULT_CUSTOM_COLORS);
  localStorage.removeItem("appearance");
  localStorage.removeItem("appearance.custom_colors");
  applyAppearance();
  toastOnly(true, "已恢复默认外观");
}

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

function resetCard(cardKey: "background" | "theme" | "card" | "sidebar"): void {
  const fields: Record<string, string[]> = {
    background: ["background_url", "background_filename", "wallpaper_api_url", "background_blur", "background_opacity", "backdrop_filter", "card_blur"],
    theme: ["theme", "accent_color", "background_color"],
    card: ["card_opacity", "border_intensity"],
    sidebar: ["sidebar_opacity", "sidebar_color", "sidebar_accent"],
  };
  const filenameToDelete = cardKey === "background" ? appearance.background_filename : "";
  (fields[cardKey] || []).forEach((f) => {
    (appearance as Record<string, unknown>)[f] = DEFAULT_APPEARANCE[f as keyof Appearance];
  });
  if (filenameToDelete) {
    backgroundApi.remove(filenameToDelete).catch(() => {});
  }
  applyAppearance();
  toastOnly(true, "已恢复默认");
}

function cardDirty(cardKey: "background" | "theme" | "card" | "sidebar"): boolean {
  const fields: Record<string, string[]> = {
    background: ["background_url", "background_blur", "background_opacity", "backdrop_filter", "card_blur"],
    theme: ["theme", "accent_color", "background_color"],
    card: ["card_opacity", "border_intensity"],
    sidebar: ["sidebar_opacity", "sidebar_color", "sidebar_accent"],
  };
  return (fields[cardKey] || []).some(
    (f) => (appearance as Record<string, unknown>)[f] !== DEFAULT_APPEARANCE[f as keyof Appearance],
  );
}

function pickCustomColor(type: keyof CustomColors): void {
  const input = document.querySelector<HTMLInputElement>(`input[data-color-picker="${type}"]`);
  input?.click();
}

function onCustomColorPicked(type: keyof CustomColors, event: Event): void {
  const hex = (event.target as HTMLInputElement).value;
  addCustomColor(type, hex);
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
    systemColors = getEffectiveTheme() === "dark" ? DARK_BG_COLORS : LIGHT_BG_COLORS;
  } else if (type === "accent") {
    systemColors = ACCENT_COLORS;
  }
  const custom = (customColors[type] || []).map((hex) => ({ value: hex, label: hex, custom: true }));
  return [...systemColors, ...custom];
}

function resetThemeBackground(): void {
  appearance.background_color = "";
  applyAppearance();
  toastOnly(true, "已恢复默认背景色");
}

async function selectBackgroundImage(): Promise<void> {
  const file = await pickFile("image/*");
  if (!file) return;
  if (file.size > LIMITS.FILE_UPLOAD_MAX) {
    toastOnly(false, "图片大小不能超过 5MB");
    return;
  }
  try {
    const data = await backgroundApi.upload(file);
    if (data.filename && data.url) {
      appearance.background_url = data.url;
      appearance.background_filename = data.filename;
      applyAppearance();
      toastOnly(true, "背景图片已设置");
    } else {
      toastOnly(false, data?.message || "上传失败");
    }
  } catch (err) {
    const e = err as { response?: { data?: { detail?: string } }; message?: string };
    toastOnly(false, "上传失败: " + (e.response?.data?.detail || e.message));
  }
}

function openRandomWallpaperDialog(): void {
  randomWallpaperDialog.url = appearance.wallpaper_api_url || "https://t.alcy.cc/pc";
  randomWallpaperDialog.loading = false;
  randomWallpaperDialog.visible = true;
}

function closeRandomWallpaperDialog(): void {
  randomWallpaperDialog.visible = false;
}

async function confirmRandomWallpaper(): Promise<void> {
  const url = randomWallpaperDialog.url.trim();
  if (!url) {
    toastOnly(false, "请输入壁纸 URL");
    return;
  }
  try {
    new URL(url);
  } catch {
    toastOnly(false, "URL 格式不正确");
    return;
  }
  randomWallpaperDialog.loading = true;
  try {
    const data = await backgroundApi.fetchUrl(url);
    if (data.filename && data.url) {
      appearance.background_url = data.url;
      appearance.background_filename = data.filename;
      appearance.wallpaper_api_url = url;
      randomWallpaperDialog.visible = false;
      applyAppearance();
      toastOnly(true, "已下载并设置为背景");
    } else {
      toastOnly(false, data?.message || "获取壁纸失败");
    }
  } catch (err) {
    const e = err as { response?: { data?: { detail?: string } } };
    toastOnly(false, (e.response?.data?.detail as string) || "获取壁纸失败");
  } finally {
    randomWallpaperDialog.loading = false;
  }
}

async function clearBackgroundImage(): Promise<void> {
  if (appearance.background_filename) {
    try {
      await backgroundApi.remove(appearance.background_filename);
    } catch {
      /* ignore */
    }
  }
  appearance.background_url = "";
  appearance.background_filename = "";
  appearance.wallpaper_api_url = "";
  applyAppearance();
}

function openBgLightbox(): void {
  bgLightbox.visible = true;
}
function closeBgLightbox(): void {
  bgLightbox.visible = false;
}

export function useAppearance() {
  return {
    appearance, // 注意：选择 background_color 时保持类型统一
    customColors: customColors as CustomColors,
    randomWallpaperDialog,
    bgLightbox,
    saveAppearance,
    resetAppearance,
    addCustomColor,
    removeCustomColor,
    resetCard,
    cardDirty,
    pickCustomColor,
    onCustomColorPicked,
    onColorLongPress,
    startLongPress,
    getColorList,
    getEffectiveTheme,
    resetThemeBackground,
    applyAppearance,
    selectBackgroundImage,
    openRandomWallpaperDialog,
    closeRandomWallpaperDialog,
    confirmRandomWallpaper,
    clearBackgroundImage,
    openBgLightbox,
    closeBgLightbox,
    saveStoredAppearance,
    saveStoredColors,
  };
}
