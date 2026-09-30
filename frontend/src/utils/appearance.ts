/** 外观偏好的校验、分区归属与配色适配，旧存储缺少新字段时沿用默认值。 */
import type { Appearance, CustomColors } from "./appearance-types";
import { DEFAULT_APPEARANCE, MONO_ACCENT } from "./constants";
import { contrastRatio, hexToRgb } from "./formatters";

export const APPEARANCE_FIELDS = {
  background: ["background_url", "background_filename", "wallpaper_api_url", "background_blur", "background_opacity"],
  theme: ["theme", "accent_color", "background_color"],
  card: ["card_opacity", "border_intensity", "backdrop_filter", "card_blur"],
  sidebar: ["sidebar_opacity", "sidebar_color", "sidebar_accent"],
  reading: ["font_scale", "reduce_motion"],
} as const satisfies Record<string, readonly (keyof Appearance)[]>;
export type AppearanceSection = keyof typeof APPEARANCE_FIELDS;

/** 只接受完整 HEX 色值，避免坏存储注入任意 CSS。 */
export function validHex(value: unknown): value is string {
  return typeof value === "string" && /^#[\da-f]{6}$/i.test(value);
}

/** 校验字段类型并收敛范围，保留已有用户偏好。 */
export function normalizeAppearance(value: unknown): Appearance {
  const result = { ...DEFAULT_APPEARANCE };
  if (!value || typeof value !== "object" || Array.isArray(value)) return result;
  const source = value as Record<string, unknown>;
  const ranges = { background_blur: [0, 30], background_opacity: [0, 0.8], card_opacity: [0, 1], card_blur: [0, 24], border_intensity: [0, 2], sidebar_opacity: [0.3, 1], font_scale: [1, 1.2] } as const;
  for (const [field, [min, max]] of Object.entries(ranges)) {
    const n = source[field];
    if (typeof n === "number" && Number.isFinite(n)) Object.assign(result, { [field]: Math.min(max, Math.max(min, n)) });
  }
  for (const field of ["background_color", "sidebar_color", "sidebar_accent"] as const) {
    if (source[field] === "" || validHex(source[field])) result[field] = source[field] as string;
  }
  if (source.accent_color === MONO_ACCENT || validHex(source.accent_color)) result.accent_color = source.accent_color;
  for (const field of ["backdrop_filter", "reduce_motion"] as const) {
    if (typeof source[field] === "boolean") result[field] = source[field];
  }
  if (["light", "dark", "auto"].includes(String(source.theme))) result.theme = source.theme as Appearance["theme"];
  for (const field of ["background_filename", "wallpaper_api_url", "background_url"] as const) {
    if (typeof source[field] === "string") result[field] = source[field];
  }
  return result;
}

/** 清理坏色板与重复颜色，保证移动端色板可正常打开。 */
export function normalizeCustomColors(value: unknown): CustomColors {
  const result: CustomColors = { accent: [], bg: [], sidebar: [], sidebar_accent: [] };
  if (!value || typeof value !== "object") return result;
  const source = value as Record<string, unknown>;
  for (const key of Object.keys(result) as (keyof CustomColors)[]) {
    if (Array.isArray(source[key])) result[key] = [...new Set(source[key].filter(validHex).map((color) => color.toLowerCase()))];
  }
  return result;
}

/** 按比例混合两种颜色，用于透明底色的实际可读性计算。 */
export function mixColor(foreground: string, background: string, opacity: number): string {
  const a = hexToRgb(foreground)!;
  const b = hexToRgb(background)!;
  return "#" + (["r", "g", "b"] as const).map((c) => Math.round(a[c] * opacity + b[c] * (1 - opacity)).toString(16).padStart(2, "0")).join("");
}

/** 保留选色的色相，必要时向文字色靠拢以保持链接与标记可读。 */
export function readableColor(color: string, background: string, target: string, minimum = 4.5): string {
  for (let step = 0; step <= 20; step++) {
    const candidate = mixColor(color, target, 1 - step / 20);
    if (contrastRatio(candidate, background) >= minimum) return candidate;
  }
  return target;
}

/** 背景颜色跟随明暗主题适配，避免深色文字落在深色画布或反之。 */
export function themedBackground(color: string, isLight: boolean): string {
  const base = isLight ? "#eef2f7" : "#0f172a";
  return readableColor(color || base, isLight ? "#000000" : "#ffffff", base, 10);
}

/** 快捷风格只调整视觉材质，保留主题、选色、背景图片及阅读偏好。 */
export const APPEARANCE_PRESETS = [
  { id: "clear", label: "清晰", hint: "实底卡片，信息更易读", values: { card_opacity: 1, backdrop_filter: false, card_blur: 12, border_intensity: 1, sidebar_opacity: 1 } },
  { id: "soft", label: "柔和", hint: "轻透层次，接近默认外观", values: { card_opacity: 0.65, backdrop_filter: false, card_blur: 12, border_intensity: 0.8, sidebar_opacity: 0.95 } },
  { id: "glass", label: "玻璃", hint: "适合搭配背景图片", values: { card_opacity: 0.5, backdrop_filter: true, card_blur: 16, border_intensity: 1, sidebar_opacity: 0.85 } },
] as const;
