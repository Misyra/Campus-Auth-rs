/** 外观偏好单例：实时应用、自动存储及系统明暗主题跟随。 */
import { reactive, ref, watch } from "vue";
import { DEFAULT_APPEARANCE, MONO_ACCENT } from "../utils/constants";
import { adjustColor, hexToRgb, pickOnColor } from "../utils/formatters";
import { loadStored, saveStored } from "../utils/storage";
import { APPEARANCE_FIELDS, normalizeAppearance, mixColor, readableColor, themedBackground, type AppearanceSection } from "../utils/appearance";
import { backgroundApi } from "../api";
import { useToast } from "./useToast";

const appearance = reactive(normalizeAppearance(loadStored("appearance", DEFAULT_APPEARANCE)));
const storageAvailable = ref(true);
const effectiveTheme = ref<"light" | "dark">("light");
const resolvedAccent = ref("#000000");
const { toastOnly } = useToast();
let themeTransitionTimer: ReturnType<typeof setTimeout> | null = null;
const colorScheme = typeof window !== "undefined" && typeof window.matchMedia === "function"
  ? window.matchMedia("(prefers-color-scheme: dark)") : null;

watch(appearance, () => {
  applyAppearance();
  storageAvailable.value = saveStored("appearance", appearance);
}, { deep: true });

/** 响应式主题结果使系统切换时色板同步刷新。 */
function getEffectiveTheme(): "light" | "dark" { return effectiveTheme.value; }
/** 单色主题色按有效主题返回日间黑或夜间白。 */
function getResolvedAccent(): string { return resolvedAccent.value; }

const onSystemThemeChange = () => { if (appearance.theme === "auto") applyAppearance(); };
if (typeof colorScheme?.addEventListener === "function") colorScheme.addEventListener("change", onSystemThemeChange);
else colorScheme?.addListener(onSystemThemeChange);

/** 应用视觉变量，文字对比色与装饰颜色分别计算。 */
function applyAppearance(): void {
  const root = document.documentElement;
  const body = document.body;
  const set = (key: string, value: string) => root.style.setProperty(key, value);
  const rgb = (color: string) => { const c = hexToRgb(color)!; return `${c.r}, ${c.g}, ${c.b}`; };
  body.classList.toggle("has-custom-bg", !!appearance.background_url);
  body.classList.toggle("no-backdrop-filter", !appearance.backdrop_filter);
  if (appearance.background_url) {
    body.style.setProperty("--bg-image", `url(${JSON.stringify(appearance.background_url)})`);
    body.style.setProperty("--bg-blur", `blur(${appearance.background_blur}px)`);
    body.style.setProperty("--bg-opacity", String(appearance.background_opacity));
  } else {
    for (const key of ["--bg-image", "--bg-blur", "--bg-opacity"]) body.style.removeProperty(key);
  }

  const isLight = appearance.theme === "auto" ? !colorScheme?.matches : appearance.theme === "light";
  effectiveTheme.value = isLight ? "light" : "dark";
  root.classList.toggle("reduce-motion", appearance.reduce_motion);
  const reduceMotion = appearance.reduce_motion || window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (themeTransitionTimer !== null) clearTimeout(themeTransitionTimer);
  root.classList.remove("theme-changing");
  if (root.getAttribute("data-theme") && root.getAttribute("data-theme") !== effectiveTheme.value && !reduceMotion) {
    root.classList.add("theme-changing");
    themeTransitionTimer = setTimeout(() => { root.classList.remove("theme-changing"); themeTransitionTimer = null; }, 280);
  }
  root.setAttribute("data-theme", effectiveTheme.value);
  const fontSizes = { "2xs": 10, xs: 11, sm: 12, md: 13, base: 14, lg: 16, xl: 20, "2xl": 24 };
  for (const [size, pixels] of Object.entries(fontSizes)) set(`--text-${size}`, `${pixels * appearance.font_scale}px`);

  const canvas = themedBackground(appearance.background_color, isLight);
  const surface = isLight ? "#ffffff" : "#0f172a";
  const foreground = isLight ? "#000000" : "#ffffff";
  const accent = appearance.accent_color === MONO_ACCENT ? foreground : appearance.accent_color;
  resolvedAccent.value = accent;
  set("--accent", accent);
  set("--accent-hover", adjustColor(accent, appearance.accent_color === MONO_ACCENT && isLight ? 31 : -20));
  set("--accent-rgb", rgb(accent));
  set("--accent-text", readableColor(accent, canvas, foreground));
  set("--content-accent-text", readableColor(accent, canvas, foreground));
  set("--on-accent", pickOnColor(accent));
  set("--on-accent-hover", pickOnColor(adjustColor(accent, appearance.accent_color === MONO_ACCENT && isLight ? 31 : -20)));
  set("--toggle-knob-active", "var(--on-accent)");
  set("--bg-primary", canvas);
  set("--bg-secondary", mixColor(surface, canvas, 0.45));
  set("--content-text-primary", foreground);
  set("--content-text-secondary", isLight ? "#1a1a1a" : "#b3b3b3");
  set("--content-text-muted", isLight ? "#333333" : "#999999");
  set("--bg-card", `rgba(${rgb(surface)}, ${appearance.card_opacity})`);
  set("--card-blur", appearance.backdrop_filter ? `blur(${appearance.card_blur}px)` : "none");

  const bi = appearance.border_intensity;
  const neutral = isLight ? "100, 116, 139" : "148, 163, 184";
  set("--border", `rgba(${neutral}, ${0.14 * bi})`);
  set("--border-hover", `rgba(${neutral}, ${0.24 * bi})`);
  for (const [name, alpha] of [["--border-accent", 0.15], ["--border-accent-hover", 0.25], ["--border-accent-strong", 0.3]] as const) {
    set(name, `rgba(${rgb(accent)}, ${alpha * bi})`);
  }
  if (appearance.accent_color === MONO_ACCENT) set("--shadow-accent", `0 0 10px rgba(${rgb(foreground)}, 0.1)`);
  else root.style.removeProperty("--shadow-accent");

  // 自定义导航底色可以跨明暗主题，文字按透明度合成后的实际底色适配。
  const navigation = appearance.sidebar_color || mixColor(surface, canvas, 0.6);
  const navigationComposite = mixColor(navigation, canvas, appearance.sidebar_opacity);
  const navText = pickOnColor(navigationComposite);
  const navAccent = readableColor(appearance.sidebar_accent || accent, navigationComposite, navText);
  set("--navigation-bg", `rgba(${rgb(navigation)}, ${appearance.sidebar_opacity})`);
  set("--navigation-text", navText);
  set("--navigation-secondary", mixColor(navText, navigationComposite, 0.8));
  set("--navigation-error", readableColor("#ef4444", navigationComposite, navText));
  set("--navigation-success", readableColor("#10b981", navigationComposite, navText));
  set("--navigation-hover", `rgba(${rgb(navText)}, 0.08)`);
  set("--sidebar-accent", navAccent);
  set("--navigation-accent-rgb", rgb(navAccent));
}

/** 按同一份字段表恢复分区默认值，背景分区额外清理已上传文件。 */
function resetCard(section: AppearanceSection): void {
  const filename = section === "background" ? appearance.background_filename : "";
  for (const field of APPEARANCE_FIELDS[section]) Object.assign(appearance, { [field]: DEFAULT_APPEARANCE[field] });
  if (filename) void backgroundApi.remove(filename).catch(() => {});
  toastOnly(true, "已恢复默认");
}
/** 判断分区是否有自定义偏好，含下载地址等易遗漏字段。 */
function cardDirty(section: AppearanceSection): boolean {
  return APPEARANCE_FIELDS[section].some((field) => appearance[field] !== DEFAULT_APPEARANCE[field]);
}
/** 恢复本浏览器全部显示偏好，色板和已上传的图片文件继续保留。 */
function resetAll(): void {
  Object.assign(appearance, DEFAULT_APPEARANCE);
  toastOnly(true, "已恢复默认外观");
}
/** 仅恢复默认背景色，保留背景图片。 */
function resetThemeBackground(): void { appearance.background_color = ""; }

export function useAppearance() {
  return { appearance, storageAvailable, resetCard, resetAll, cardDirty, getEffectiveTheme, getResolvedAccent, resetThemeBackground, applyAppearance };
}
