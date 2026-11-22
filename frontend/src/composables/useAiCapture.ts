/**
 * AI 任务页第 2 步：登录页捕获（发起捕获 / 产物打包下载 / 刷新后状态恢复 / 渠道引擎降级提示）。
 *
 * 自 AiTaskView.vue 按职责拆出（设计系统收口 T8-2）：本模块只管「捕获结果」；
 * LLM 配置草稿见 useAiLlmConfig，生成编排与任务落盘见 useAiGenerate。
 * toastOnly 由调用方显式注入（依赖可见、测试可桩），不在模块内部取单例。
 */
import { computed, ref } from "vue";
import { aiApi, configApi } from "../api";
import { extractApiError } from "../api/client";
import { downloadBlob } from "../utils/file";
import { fileStamp } from "../utils/formatters";
import type { AiCaptureResult } from "../api/types";

export interface UseAiCaptureOptions {
  /** 提示回调（成功/失败 toast，与视图共用同一单例语义） */
  toastOnly: (success: boolean, message: string) => void;
}

/** AI 任务页第 2 步的捕获状态与操作；入参显式、无模块级可变状态 */
export function useAiCapture(options: UseAiCaptureOptions) {
  const { toastOnly } = options;

  const captureUrl = ref("");
  const capturing = ref(false);
  const captureResult = ref<AiCaptureResult | null>(null);
  const screenshotUrl = ref("");
  const savingBundle = ref(false);
  const isCaptureDone = computed(() => !!captureResult.value);
  /** 当前浏览器渠道与自定义渠道的引擎（决定 CDP 是否可用，进而决定捕获质量） */
  const browserChannel = ref("");
  const customBrowserEngine = ref("");
  /**
   * 渠道是否落在非 Chromium 引擎上。
   *
   * MHTML 完整快照与 CDP 资源快照都只存在于 Chromium：firefox / webkit（含
   * custom 渠道配这两个引擎）下后端会跳过 MHTML，CSS/JS 改用页面枚举 + HTTP
   * 回补抓取。捕获仍可用，所以这里是提示而非禁用；文案与后端 note 保持一致口径。
   */
  const captureChannelDegraded = computed(() => {
    const channel = browserChannel.value;
    const engine = channel === "custom" ? customBrowserEngine.value : channel;
    return engine === "firefox" || engine === "webkit";
  });

  /** 读取浏览器渠道：失败只少了这条提示，不影响捕获流程，静默忽略 */
  async function loadBrowserChannel(): Promise<void> {
    try {
      const cfg = await configApi.fetch();
      browserChannel.value = cfg.browser?.browser_channel || "";
      customBrowserEngine.value = cfg.browser?.custom_browser_engine || "";
    } catch {
      browserChannel.value = "";
    }
  }

  async function capture(): Promise<void> {
    if (!captureUrl.value.trim()) {
      toastOnly(false, "请输入登录页地址");
      return;
    }
    capturing.value = true;
    captureResult.value = null;
    try {
      captureResult.value = await aiApi.capture(captureUrl.value.trim());
      screenshotUrl.value = aiApi.captureScreenshotUrl();
      toastOnly(true, "页面捕获完成");
    } catch (error) {
      toastOnly(false, extractApiError(error, "页面捕获失败"));
    } finally {
      capturing.value = false;
    }
  }

  async function saveBundle(): Promise<void> {
    if (savingBundle.value) return;
    savingBundle.value = true;
    try {
      const blob = await aiApi.captureBundle();
      const stamp = fileStamp();
      downloadBlob(blob, `campus-auth-capture-${stamp}.zip`, "application/zip");
      toastOnly(true, "页面文件已保存");
    } catch (error) {
      toastOnly(false, extractApiError(error as Error, "保存页面文件失败"));
    } finally {
      savingBundle.value = false;
    }
  }

  /** 页面刷新后恢复捕获状态：后端产物仍在时无需强制重新捕获 */
  async function restoreCapture(): Promise<void> {
    try {
      const st = await aiApi.captureStatus();
      if (st?.available) {
        captureResult.value = {
          final_url: st.final_url || "",
          title: st.title,
          structure_summary: st.structure_summary,
        } as AiCaptureResult;
        screenshotUrl.value = aiApi.captureScreenshotUrl();
      }
    } catch {
      // 状态恢复失败不阻塞页面，按未捕获处理即可
    }
  }

  return {
    captureUrl,
    capturing,
    captureResult,
    screenshotUrl,
    savingBundle,
    isCaptureDone,
    browserChannel,
    captureChannelDegraded,
    loadBrowserChannel,
    capture,
    saveBundle,
    restoreCapture,
  };
}

export type UseAiCaptureReturn = ReturnType<typeof useAiCapture>;
