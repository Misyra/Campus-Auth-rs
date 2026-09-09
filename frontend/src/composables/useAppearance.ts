/**
 * 外观与主题（单例）。
 * 替代原 appearanceData + appearanceMethods + applyAppearance。
 * 从 localStorage 加载/持久化，并应用到文档 CSS 变量。
 * 自定义颜色见 useCustomColors，背景图/壁纸见 useBackgroundImage。
 */

import { reactive, watch } from "vue";
import { DEFAULT_APPEARANCE } from "../utils/constants";
import type { Appearance } from "../utils/appearance-types";
import { hexToRgb, adjustColor, pickOnColor } from "../utils/formatters";
import { loadStored } from "../utils/storage";
import { backgroundApi } from "../api";
import { useToast } from "./useToast";

const appearance = reactive<Appearance>(
  loadStored<Appearance>("appearance", { ...DEFAULT_APPEARANCE }),
);

const { toastOnly } = useToast();

function saveStoredAppearance(): void {
  localStorage.setItem("appearance", JSON.stringify(appearance));
}

// 外观变更时自动应用 + 持久化（实时预览，无需手动点保存）
watch(appearance, () => {
  applyAppearance();
  saveStoredAppearance();
}, { deep: true });

/** 解析有效主题：theme=auto 时按系统深浅色偏好实时判定，其余直接返回设定值 */
function getEffectiveTheme(): "light" | "dark" {
  const themeMode = appearance.theme || "light";
  if (themeMode === "auto") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return themeMode;
}

// theme=auto 时跟随系统深浅色切换：OS 切换不会触发 appearance watcher，
// 必须显式监听 matchMedia change 并重应用（否则要等下一次外观改动才生效）
try {
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if ((appearance.theme || "light") === "auto") applyAppearance();
    });
} catch {
  // 旧浏览器无 addEventListener（仅 addListener）：跟随系统失效可接受
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
    // 自定义强调色深浅不可预设：按亮度切换其上的文字色，保证可读
    root.style.setProperty("--on-accent", pickOnColor(appearance.accent_color));
  } else {
    // 清除自定义值，回落到 CSS 中按主题预置的默认组合
    root.style.removeProperty("--on-accent");
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

/**
 * 将指定卡片的字段整体恢复为默认值（背景卡额外删除后端已上传的图片文件）。
 * 字段归属表与 cardDirty 的判定表是同一份口径的两处拷贝：新增外观字段时两处必须同步，
 * 否则会出现"重置漏字段"或"脏判定漏字段"的不一致。
 */
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

/**
 * 判断指定卡片是否偏离默认值（用于"恢复默认"按钮的可用态/高亮）。
 * 判定表是 resetCard 字段表的子集（background_filename 等派生字段不参与脏判定）：
 * 两表需交叉对照维护，新增外观字段时两处必须同步。
 */
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

/** 仅恢复默认背景色（不动背景图等其他字段），供主题卡内独立按钮使用 */
function resetThemeBackground(): void {
  appearance.background_color = "";
  applyAppearance();
  toastOnly(true, "已恢复默认背景色");
}

export function useAppearance() {
  return {
    appearance, // 注意：选择 background_color 时保持类型统一
    resetCard,
    cardDirty,
    getEffectiveTheme,
    resetThemeBackground,
    applyAppearance,
  };
}
