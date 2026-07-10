/**
 * 外观相关类型 + 从 api/types 重新导出的共享类型。
 * 集中放在此处，便于 utils/constants.ts 引用而不产生循环依赖困惑。
 */

import type { Config, Profile } from "../api/types";

export type { Config, Profile };

/** 外观设置 */
export interface Appearance {
  background_url: string;
  background_filename: string;
  wallpaper_api_url: string;
  background_blur: number;
  background_opacity: number;
  background_color: string;
  card_opacity: number;
  card_blur: number;
  border_intensity: number;
  sidebar_opacity: number;
  sidebar_color: string;
  sidebar_accent: string;
  backdrop_filter: boolean;
  accent_color: string;
  theme: "light" | "dark" | "auto";
}

/** 自定义颜色分组 */
export interface CustomColors {
  accent: string[];
  bg: string[];
  sidebar: string[];
  sidebar_accent: string[];
}
