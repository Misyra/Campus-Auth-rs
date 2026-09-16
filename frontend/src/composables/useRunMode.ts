/**
 * 运行模式切换（默认 / 调试 / 自定义）。
 *
 * 预设定义与判定逻辑见 `utils/runMode.ts`（纯函数）；本模块负责"读取当前值 → 判定
 * 模式 → 应用预设"这层与 API 打交道的部分。
 *
 * **为何是立即生效而非改草稿**：「切换模式」是一个动作（用户点了就期待马上变），
 * 不是表单编辑。若只改草稿，用户还得记得去点「立即保存」，而 `autostart` 又只能经
 * 专用 API 即时生效——两者会脱节成"自启动已变、浏览器还没变"的半套状态。
 *
 * **为何要求先清空未保存修改**：应用模式会 `PATCH` 服务端，随后必须 `fetchConfig`
 * 回读才能让界面与磁盘一致；若此刻设置页草稿里有未保存改动，回读会把它们静默冲掉。
 * 故先确认，用户取消则整体放弃（不产生任何写入）。
 */
import { computed, ref } from "vue";
import { useConfig } from "./useConfig";
import { useStatus } from "./useStatus";
import { useToast } from "./useToast";
import { useConfirm } from "./useConfirm";
import { configApi, autostartApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import {
  RUN_MODE_PRESETS,
  detectRunMode,
  diffRunMode,
  getRunModePreset,
} from "../utils/runMode";
import type { RunModeId, RunModeSettings } from "../utils/runMode";

/** 应用模式时的 in-flight 标记（防连点重复提交） */
const applying = ref(false);

/**
 * 读取"当前设置"在预设关注字段上的取值。
 *
 * 组合两个来源：`PATCH-able` 的设置取自 `useConfig` 的表单模型，`autostart` 取自
 * `useStatus`（它由 `/api/autostart/status` 提供，不在 config 响应里）。
 */
function currentRunModeSettings(): RunModeSettings {
  // 注意：useConfig() 返回对象的 `config` 字段即响应式配置本身（含 browser/worker/…），
  // 故直接写 config.browser，而非 config.config.browser
  const { config } = useConfig();
  const { autostart } = useStatus();
  return {
    headless: config.browser.headless,
    low_resource_mode: config.browser.low_resource_mode,
    keep_alive: config.worker.keep_alive,
    startup_action: config.app_settings.startup_action,
    log_level: config.logging.level,
    autostart: autostart.enabled,
    pause_enabled: config.pause.enabled,
  };
}

/** 当前所处模式：与预设逐字段比对，都不匹配即 `custom` */
const currentMode = computed<RunModeId>(() =>
  detectRunMode(currentRunModeSettings()),
);

/**
 * 应用一个预设模式。
 *
 * 返回是否应用成功（用户取消或中途失败为 false）。
 */
async function applyRunMode(id: Exclude<RunModeId, "custom">): Promise<boolean> {
  if (applying.value) return false;
  const preset = getRunModePreset(id);
  if (!preset) return false;

  const { config, fetchConfig, dirty } = useConfig();
  const { fetchAutostart } = useStatus();
  const { confirm } = useConfirm();
  const { toastOnly } = useToast();

  // 未保存的修改会被随后的回读覆盖：先征得同意，取消则整体不做（不产生任何写入）
  if (dirty.value) {
    const ok = await confirm({
      title: "有未保存的修改",
      message:
        "应用运行模式需要重新读取服务端配置，当前未保存的修改将会丢失。确定继续吗？",
      danger: true,
      confirmText: "丢弃并应用",
    });
    if (ok !== true) return false;
  }

  const changes = diffRunMode(currentRunModeSettings(), preset.settings);
  if (changes.length === 0) {
    toastOnly(true, `已经是${preset.label}，无需更改`);
    return true;
  }

  applying.value = true;
  try {
    // 顺序：先提交能一次 PATCH 的项，再处理两个需要独立 API 的项（日志级别 / 开机自启）。
    // 三者互不依赖，任一步失败都会进 catch 并提示"可能已部分生效"。
    await configApi.patch({
      browser: {
        ...config.browser,
        headless: preset.settings.headless,
        low_resource_mode: preset.settings.low_resource_mode,
      },
      worker: {
        ...config.worker,
        keep_alive: preset.settings.keep_alive,
      },
      app_settings: {
        ...config.app_settings,
        startup_action: preset.settings.startup_action,
      },
      // 暂停时段只动启用开关，起止（默认 23–6）是用户可调值，保留现场不覆盖
      pause: {
        ...config.pause,
        enabled: preset.settings.pause_enabled,
      },
      // 其余全局段原样回传，避免 PATCH 合并时漏字段
      monitor: config.monitor,
      logging: config.logging,
      retry: config.retry,
      updater: config.updater,
    });

    // 日志级别必须走专用接口：仅 PATCH 该字段只落盘、不热更新 tracing filter，
    // 会出现「界面显示 DEBUG、实际仍按 INFO 过滤」的静默假象
    if (
      currentRunModeSettings().log_level.toUpperCase() !==
      preset.settings.log_level.toUpperCase()
    ) {
      await configApi.setLogLevel(preset.settings.log_level);
      frontendLogger.setLevel(preset.settings.log_level);
    }

    // 开机自启走专用接口（会真实写系统注册表），且仅在需要变化时调用
    if (currentRunModeSettings().autostart !== preset.settings.autostart) {
      await autostartApi.toggle(preset.settings.autostart);
    }

    // 回读：让界面与磁盘一致（也使 currentMode 重新判定）
    await fetchConfig();
    await fetchAutostart();

    frontendLogger.info("config", `运行模式已切换为${preset.label}`);
    toastOnly(true, `已应用${preset.label}`);
    return true;
  } catch (error) {
    const msg = extractApiError(error, "应用运行模式失败");
    frontendLogger.error("config", `应用运行模式失败: ${msg}`, error);
    // 明确告知可能已部分生效，避免用户以为"什么都没变"而重复点击
    toastOnly(false, `${msg}（部分设置可能已生效，请检查后重试）`);
    // 尽力回读，让界面反映真实的服务端状态
    await fetchConfig().catch(() => undefined);
    await fetchAutostart().catch(() => undefined);
    return false;
  } finally {
    applying.value = false;
  }
}

export function useRunMode() {
  return {
    RUN_MODE_PRESETS,
    currentMode,
    applying,
    applyRunMode,
    currentRunModeSettings,
    diffFor: (id: Exclude<RunModeId, "custom">) => {
      const preset = getRunModePreset(id);
      return preset ? diffRunMode(currentRunModeSettings(), preset.settings) : [];
    },
  };
}
