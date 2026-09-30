/**
 * 背景图与壁纸（单例）。
 * 从 useAppearance 拆出：本地选图上传、随机壁纸下载、背景清理与放大预览。
 * 需要读写 appearance 背景字段并通过 applyAppearance 应用，经 useAppearance() 单例获取。
 */

import { reactive, ref } from "vue";
import { LIMITS } from "../utils/constants";
import { pickFile } from "../utils/file";
import { backgroundApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";
import { useAppearance } from "./useAppearance";

const randomWallpaperDialog = reactive({ visible: false, url: "", loading: false });
const bgLightbox = reactive({ visible: false });
const uploading = ref(false);
const removing = ref(false);

const { toastOnly } = useToast();

/** 本地选图并上传：超过上限直接拒绝（避免大图塞进用户数据目录），成功后立即应用 */
async function selectBackgroundImage(): Promise<void> {
  if (uploading.value || removing.value || randomWallpaperDialog.loading) return;
  uploading.value = true;
  try {
    const file = await pickFile("image/*");
    if (!file) return;
    if (file.size > LIMITS.FILE_UPLOAD_MAX) {
      toastOnly(false, "图片大小不能超过 5MB");
      return;
    }
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
    toastOnly(false, "上传失败: " + extractApiError(err, "上传失败"));
  } finally {
    uploading.value = false;
  }
}

/** 打开随机壁纸弹窗，预填上次使用的壁纸 API 地址（首次回落默认源） */
function openRandomWallpaperDialog(): void {
  if (uploading.value || removing.value || randomWallpaperDialog.loading) return;
  const { appearance } = useAppearance();
  randomWallpaperDialog.url = appearance.wallpaper_api_url || "https://t.alcy.cc/pc";
  randomWallpaperDialog.loading = false;
  randomWallpaperDialog.visible = true;
}

/** 关闭随机壁纸弹窗（不清理 URL，保留用户输入便于重试） */
function closeRandomWallpaperDialog(): void {
  if (randomWallpaperDialog.loading) return;
  randomWallpaperDialog.visible = false;
}

/**
 * 确认下载随机壁纸并设为背景。
 * 提交前先做 URL 构造校验：该地址会交给后端发起出网请求，格式非法的输入
 * 在前端拦截可避免一次注定失败的后端往返，也能防止把任意乱串持久化进配置。
 */
async function confirmRandomWallpaper(): Promise<void> {
  if (randomWallpaperDialog.loading || uploading.value || removing.value) return;
  const url = randomWallpaperDialog.url.trim();
  if (!url) {
    toastOnly(false, "请输入壁纸 URL");
    return;
  }
  try {
    const parsed = new URL(url);
    if (!["http:", "https:"].includes(parsed.protocol)) throw new Error("不支持的链接协议");
  } catch {
    toastOnly(false, "请输入有效的 HTTP 或 HTTPS 图片链接");
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
    const msg = extractApiError(err, "获取壁纸失败");
    toastOnly(false, msg);
  } finally {
    randomWallpaperDialog.loading = false;
  }
}

/** 清除背景：先删除后端已落盘的文件（失败不阻断，仅记日志），再清空本地字段 */
async function clearBackgroundImage(): Promise<void> {
  if (uploading.value || removing.value || randomWallpaperDialog.loading) return;
  removing.value = true;
  try {
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
  } finally { removing.value = false; }
}

/** 打开背景图放大预览 */
function openBgLightbox(): void {
  bgLightbox.visible = true;
}
/** 关闭背景图放大预览 */
function closeBgLightbox(): void {
  bgLightbox.visible = false;
}

export function useBackgroundImage() {
  return {
    uploading,
    removing,
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
