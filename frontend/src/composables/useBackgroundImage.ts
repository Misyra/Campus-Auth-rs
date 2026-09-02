/**
 * 背景图与壁纸（单例）。
 * 从 useAppearance 拆出：本地选图上传、随机壁纸下载、背景清理与放大预览。
 * 需要读写 appearance 背景字段并通过 applyAppearance 应用，经 useAppearance() 单例获取。
 */

import { reactive } from "vue";
import { LIMITS } from "../utils/constants";
import { pickFile } from "../utils/file";
import { backgroundApi, ApiError } from "../api";
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";
import { useAppearance } from "./useAppearance";

const randomWallpaperDialog = reactive({ visible: false, url: "", loading: false });
const bgLightbox = reactive({ visible: false });

const { toastOnly } = useToast();

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
      const { appearance, applyAppearance } = useAppearance();
      appearance.background_url = data.url;
      appearance.background_filename = data.filename;
      applyAppearance();
      toastOnly(true, "背景图片已设置");
    } else {
      toastOnly(false, data?.message || "上传失败");
    }
  } catch (err) {
    const msg = err instanceof ApiError ? err.message : (err as { message?: string }).message || "上传失败";
    toastOnly(false, "上传失败: " + msg);
  }
}

function openRandomWallpaperDialog(): void {
  const { appearance } = useAppearance();
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
      const { appearance, applyAppearance } = useAppearance();
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
    const msg = err instanceof ApiError ? err.message : "获取壁纸失败";
    toastOnly(false, msg);
  } finally {
    randomWallpaperDialog.loading = false;
  }
}

async function clearBackgroundImage(): Promise<void> {
  const { appearance, applyAppearance } = useAppearance();
  if (appearance.background_filename) {
    try {
      await backgroundApi.remove(appearance.background_filename);
    } catch (error) {
      frontendLogger.warn("appearance", "删除背景文件失败", error);
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

export function useBackgroundImage() {
  return {
    randomWallpaperDialog,
    bgLightbox,
    selectBackgroundImage,
    openRandomWallpaperDialog,
    closeRandomWallpaperDialog,
    confirmRandomWallpaper,
    clearBackgroundImage,
    openBgLightbox,
    closeBgLightbox,
  };
}
