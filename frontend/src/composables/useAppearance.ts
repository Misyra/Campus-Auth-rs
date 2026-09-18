/**
 * 外观与主题（单例）。
 * 替代原 appearanceData + appearanceMethods + applyAppearance。
 * 从 localStorage 加载/持久化，并应用到文档 CSS 变量。
 * 自定义颜色见 useCustomColors，背景图/壁纸见 useBackgroundImage。
 */

import { reactive, ref, watch } from "vue";
import { DEFAULT_APPEARANCE, MONO_ACCENT, MONO_ACCENT_DARK, MONO_ACCENT_LIGHT } from "../utils/constants";
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

/**
 * 把主题色解析为最终色值：`mono` 哨兵按**有效主题**取日间黑 / 夜间白，其余原样返回。
 *
 * 主题色只有一个字段，而 `theme` 可以是 light/dark/auto，纯 hex 无法同时表达
 * 「日间黑、夜间白」；`auto` 下更要随系统实时切。故此函数是所有消费点的**唯一入口**
 * ——CSS 变量注入、色板渲染、选中判定都必须先经过它，
 * 否则会把哨兵 `mono` 当成颜色用（它不是合法 CSS 颜色）。`isLight` 由调用方传入，
 * 避免内部重复探测系统偏好（同一帧内多次 matchMedia 结果一致但无谓）。
 */
function resolveAccentColor(isLight: boolean): string {
  return appearance.accent_color === MONO_ACCENT ? (isLight ? MONO_ACCENT_LIGHT : MONO_ACCENT_DARK) : appearance.accent_color;
}

/**
 * 主题色解析结果，**响应式**。
 *
 * 模板若直接调 `getEffectiveTheme()`（内部读 `matchMedia`）只能拿到求值当时的快照，
 * `theme=auto` 下系统切换深浅色不会触发重渲染——色块与色值文字会停在旧值。
 * 故由 `applyAppearance` 统一写入此 ref，模板读它即自动订阅。
 */
const resolvedAccent = ref(MONO_ACCENT_LIGHT);

/** 主题色解析后的最终色值（按当前有效主题），供视图层显示与色板渲染 */
function getResolvedAccent(): string {
  return resolvedAccent.value;
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

  const isLight = getEffectiveTheme() === "light";
  root.setAttribute("data-theme", isLight ? "light" : "dark");
  const _p = (k: string, v: string) => root.style.setProperty(k, v);

  // 主题色：先把 `mono` 哨兵按有效主题解析为日间黑 / 夜间白，再做后续一切派生
  const isMono = appearance.accent_color === MONO_ACCENT;
  const accent = resolveAccentColor(isLight);
  resolvedAccent.value = accent;
  if (accent) {
    _p("--accent", accent);
    // 单色的悬停色单独取值：adjustColor 会钳制到 0..255，纯黑再 -20 仍是纯黑，
    // 主按钮悬停将失去颜色反馈（深色下纯白 -25 正常），故单色按主题反向调亮度。
    _p("--accent-hover", isMono ? adjustColor(accent, isLight ? 31 : -25) : adjustColor(accent, -20));
    const accentRgb = hexToRgb(accent);
    if (accentRgb) {
      _p("--accent-rgb", `${accentRgb.r}, ${accentRgb.g}, ${accentRgb.b}`);
    }
    // 自定义强调色深浅不可预设：按亮度切换其上的文字色，保证可读。
    // 单色下同样走此逻辑，天然得到「黑底白字 / 白底黑字」。
    _p("--on-accent", pickOnColor(accent));
  } else {
    // 清除自定义值，回落到 CSS 中按主题预置的默认组合
    root.style.removeProperty("--on-accent");
  }

  // 开关旋钮叠在 accent 轨道上：非单色沿用「永远白色」（既有观感），
  // 单色深色下轨道是纯白，白钮会隐形，改取对比自适应的 --on-accent。
  if (isMono) {
    _p("--toggle-knob-active", "var(--on-accent)");
  } else {
    root.style.removeProperty("--toggle-knob-active");
  }

  // 单色主题色下，发光与描边不跟随强调色：强调色发光叠在黑白上不协调，且
  // 半透明黑发光在浅色底上几乎不可见、半透明白发光在深色底上晕成一片。
  // 故发光改中性灰（浅色下即柔和投影、深色下轻微提亮）；
  // 描边同样中性化，但统一在下方 accentBorder 处按 border_intensity 写入
  // （放这里会被那里覆盖——内联样式后写覆盖先写）。
  // 非单色必须清除内联值，让 CSS 默认（跟随 --accent-rgb 的青蓝发光）重新生效。
  if (isMono) {
    const neutral = isLight ? "0, 0, 0" : "255, 255, 255";
    _p("--shadow-accent", `0 0 10px rgba(${neutral}, ${isLight ? 0.15 : 0.12})`);
  } else {
    root.style.removeProperty("--shadow-accent");
  }

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
  // 描边色：默认青蓝；单色主题色下改中性灰（见上方 isMono 分支的说明）。
  // 统一在此处写一次，避免与上文的设置顺序耦合——内联样式后写覆盖先写。
  const accentBorder = isMono
    ? (alpha: number) => `rgba(${isLight ? "0, 0, 0" : "255, 255, 255"}, ${alpha * bi})`
    : (alpha: number) => `rgba(56, 189, 248, ${alpha * bi})`;
  if (isLight) {
    _p("--border", `rgba(100, 116, 139, ${0.12 * bi})`);
    _p("--border-hover", `rgba(100, 116, 139, ${0.22 * bi})`);
  } else {
    _p("--border", `rgba(148, 163, 184, ${0.1 * bi})`);
    _p("--border-hover", `rgba(148, 163, 184, ${0.2 * bi})`);
  }
  _p("--border-accent", accentBorder(0.15));
  _p("--border-accent-hover", accentBorder(0.25));
  _p("--border-accent-strong", accentBorder(0.3));

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
    /** 主题色解析后的实际色值（`mono` 按有效主题取黑白），视图层显示与选中判定用它 */
    getResolvedAccent,
    resetThemeBackground,
    applyAppearance,
  };
}
