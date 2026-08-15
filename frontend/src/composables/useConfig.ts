/**
 * 配置状态与操作（单例）。
 * 替代原 configData + configMethods + 部分 autostart/OCR/日志级别方法。
 * 修复 P1-12.8：用显式 dirty 标志替代每次 JSON.stringify 全量序列化。
 */

import { reactive, ref, watch } from "vue";
import type { Config, SaveConfigPayload } from "../api/types";
import { configApi, autostartApi } from "../api";
import { ApiError, extractApiError } from "../api/client";
import { DEFAULT_CONFIG } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useToast } from "./useToast";
import { usePasswordField } from "./usePasswordField";

const config = reactive<Config>(structuredClone(DEFAULT_CONFIG));
const password = usePasswordField(false);
const defaultUrlCheckUrls = [...DEFAULT_CONFIG.monitor.url_check_urls];
const dirty = ref(false);
const saveFailed = ref(false);

let loadingConfig = false;
let saveSeq = 0;
let saveAbort: AbortController | null = null;

// 深监听配置变更 → 标记 dirty（加载期间抑制）
// flush: 'sync' 确保在 loadingConfig 仍为 true 时同步触发，避免异步时序导致 dirty 误触
watch(
  config,
  () => {
    if (!loadingConfig) dirty.value = true;
  },
  { deep: true, flush: "sync" },
);

// password 是独立的 usePasswordField 实例，不在 config 响应对象内，
// 需单独监听其 value 变化以触发 dirty（否则仅改密码时 saveConfig 会因 !dirty 提前返回）。
// 注意：password 是普通对象，password.value 取的是内部 ref 对象本身，
// 直接写成 `() => password.value` 不会对该 ref 的 .value 建立响应式依赖，导致永不触发。
// 这里直接以 ref 作为 watch 源（等价于监听 password.value.value）才正确。
watch(password.value, () => {
  if (!loadingConfig) dirty.value = true;
});

const { busy } = useStatus();

async function fetchConfig(): Promise<void> {
  try {
    const data = await configApi.fetch();
    loadingConfig = true;
    config.browser = { ...DEFAULT_CONFIG.browser, ...(data.browser || {}) };
    config.worker = { ...DEFAULT_CONFIG.worker, ...(data.worker || {}) };
    config.monitor = { ...DEFAULT_CONFIG.monitor, ...(data.monitor || {}) };
    config.pause = { ...DEFAULT_CONFIG.pause, ...(data.pause || {}) };
    config.logging = { ...DEFAULT_CONFIG.logging, ...(data.logging || {}) };
    config.retry = { ...DEFAULT_CONFIG.retry, ...(data.retry || {}) };
    config.credentials = {
      username: data.username ?? "",
      password: "",
      auth_url: data.auth_url ?? "",
      isp: data.isp ?? "",
    };
    config.active_task = data.active_task ?? "";
    config.app_settings = { ...DEFAULT_CONFIG.app_settings, ...(data.app_settings || {}) };
    password.reset(!!data.has_password);
    loadingConfig = false;
    dirty.value = false;
    frontendLogger.info("config", "配置已加载");
  } catch (error) {
    frontendLogger.error("config", "获取配置失败", error);
  }
}

function validateConfig(): string[] {
  const warnings: string[] = [];
  const url = config.credentials.auth_url;
  if (url && !/^https?:\/\//.test(url)) {
    warnings.push("认证地址必须以 http:// 或 https:// 开头");
  }
  const port = config.app_settings.port;
  if (port && (port < 1 || port > 65535)) {
    warnings.push("端口范围必须在 1-65535 之间");
  }
  return warnings;
}

const { toastOnly } = useToast();

/** 密码输入回调：同步明文值并标记 dirty */
function onPasswordInput(e: Event): void {
  password.value.value = (e.target as HTMLInputElement).value;
}

async function saveConfig(force = false): Promise<void> {
  if (!dirty.value && !force) return;

  // 自定义运营商：选中"自定义"但未输入关键字时拒绝保存（修复 P1-17）
  if (config.credentials.isp === "自定义") {
    toastOnly(false, "请填写自定义运营商关键字");
    return;
  }

  const warnings = validateConfig();
  if (warnings.length > 0) frontendLogger.warn("config", warnings.join("；"));
  if (!config.credentials.auth_url) frontendLogger.warn("config", "认证地址为空，自动认证将无法工作");
  if (!config.credentials.username) frontendLogger.warn("config", "账号为空，自动认证将无法工作");
  if (!config.monitor.enable_tcp_check && !config.monitor.enable_http_check && !(config.monitor.url_check_urls && config.monitor.url_check_urls.length)) {
    frontendLogger.warn("config", "未启用任何网络检测方式，自动认证可能无法正常工作");
  }

  saveSeq++;
  const currentSeq = saveSeq;
  if (saveAbort) saveAbort.abort();
  saveAbort = new AbortController();
  const controller = saveAbort;

  busy.save = true;
  saveFailed.value = false;
  const pwdValue = password.submitValue();
  const submittedPassword = !!pwdValue;
  const payload: SaveConfigPayload = {
    browser: config.browser,
    worker: config.worker,
    monitor: config.monitor,
    pause: config.pause,
    logging: config.logging,
    retry: config.retry,
    app_settings: config.app_settings,
    active_task: config.active_task || "",
    username: config.credentials.username ?? "",
    auth_url: config.credentials.auth_url ?? "",
    isp: config.credentials.isp ?? "",
    password: pwdValue as string | null,
  };

  try {
    await configApi.patch(payload, { signal: controller.signal });
    if (submittedPassword) password.markSaved(true);
    dirty.value = false;
    frontendLogger.info("config", "配置保存成功");
  } catch (error) {
    const err = error as { name?: string };
    if (err.name === "AbortError") return;
    const msg = extractApiError(error, "保存失败");
    frontendLogger.error("config", "保存配置失败", error);
    toastOnly(false, msg);
    saveFailed.value = true;
  } finally {
    if (saveSeq === currentSeq) busy.save = false;
  }
}

async function fetchLogLevels(): Promise<void> {
  try {
    const data = await configApi.fetchLogLevels();
    if (data.level) config.logging.level = data.level;
  } catch (error) {
    frontendLogger.warn("config", "获取日志级别配置失败", error);
  }
}

async function setLogLevel(level: string): Promise<void> {
  try {
    const data = await configApi.setLogLevel(level);
    config.logging.level = level;
    frontendLogger.setLevel(level);
    frontendLogger.info("config", `日志级别已设置: ${level}`);
    toastOnly(true, data?.message || "日志级别已设置");
  } catch (error) {
    const msg = extractApiError(error, "设置失败");
    frontendLogger.error("config", `设置日志级别失败: ${msg}`, error);
    toastOnly(false, msg);
  }
}

async function toggleAutostart(enable: boolean): Promise<void> {
  busy.autostart = true;
  try {
    const data = await autostartApi.toggle(enable);
    frontendLogger.info("autostart", data?.message || `${enable ? "启用" : "关闭"}自启动成功`);
    toastOnly(true, data?.message || `${enable ? "启用" : "关闭"}自启动成功`);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      frontendLogger.warn("autostart", "后端不支持开机自启动");
      toastOnly(false, "当前后端版本不支持开机自启动，请重启后端");
    } else {
      frontendLogger.error("autostart", `${enable ? "启用" : "关闭"}自启动异常`, error);
      toastOnly(false, `${enable ? "启用" : "关闭"}自启动失败`);
    }
  } finally {
    await useStatus().fetchAutostart();
    busy.autostart = false;
  }
}

export function useConfig() {
  return {
    config,
    password,
    passwordDisplay: password.display,
    passwordSaved: password.saved,
    editingPassword: password.editing,
    defaultUrlCheckUrls,
    dirty,
    saveFailed,
    fetchConfig,
    onPasswordFocus: password.onFocus,
    onPasswordBlur: password.onBlur,
    onPasswordInput,
    saveConfig,
    fetchLogLevels,
    setLogLevel,
    toggleAutostart,
  };
}
