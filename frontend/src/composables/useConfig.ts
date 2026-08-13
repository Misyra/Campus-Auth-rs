/**
 * 配置状态与操作（单例）。
 * 替代原 configData + configMethods + 部分 autostart/OCR/日志级别方法。
 * 修复 P1-12.8：用显式 dirty 标志替代每次 JSON.stringify 全量序列化。
 */

import { reactive, ref, watch } from "vue";
import type { Config, SaveConfigPayload, ShellInfo, OcrStatus } from "../api/types";
import { configApi, autostartApi, ocrApi } from "../api";
import { extractApiError } from "../api/client";
import { DEFAULT_CONFIG } from "../utils/constants";
import { frontendLogger } from "../utils/logger";
import { useStatus } from "./useStatus";
import { useToast } from "./useToast";
import { useNotifications } from "./useNotifications";
import { usePasswordField } from "./usePasswordField";
import { useConfirm } from "./useConfirm";

function cloneConfig(src: Config): Config {
  return {
    browser: { ...src.browser },
    worker: { ...src.worker },
    monitor: { ...src.monitor },
    pause: { ...src.pause },
    logging: { ...src.logging },
    retry: { ...src.retry },
    credentials: { ...src.credentials },
    active_task: src.active_task,
    app_settings: { ...src.app_settings },
  };
}

const config = reactive<Config>(cloneConfig(DEFAULT_CONFIG));
const password = usePasswordField(false);
const defaultUrlCheckUrls = [...DEFAULT_CONFIG.monitor.url_check_urls];
const availableShells = reactive<ShellInfo[]>([]);
const defaultShell = ref("");
const ocrStatus = reactive<OcrStatus>({ installed: false, size_mb: 0 });
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
const { notify } = useNotifications();

function ensureAtLeastOneCheckMethod(): void {
  const { enable_tcp_check, enable_http_check, url_check_urls } = config.monitor;
  if (!enable_tcp_check && !enable_http_check && !(url_check_urls && url_check_urls.length)) {
    toastOnly(false, "至少需要保留一种网络检测方式");
    // 等 DOM 更新后恢复勾选
    Promise.resolve().then(() => {
      config.monitor.enable_tcp_check = true;
    });
  }
}

function onPasswordFocus(): void {
  password.onFocus();
}
function onPasswordBlur(): void {
  password.onBlur();
}
/** 密码输入回调：同步明文值并标记 dirty */
function onPasswordInput(e: Event): void {
  password.value.value = (e.target as HTMLInputElement).value;
}

async function saveConfig(force = false): Promise<void> {
  if (!dirty.value && !force) return;

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

async function resetConfig(): Promise<void> {
  const { confirm } = useConfirm();
  const ok = await confirm({
    title: "恢复默认设置",
    message: "确定要恢复默认设置吗？当前修改将丢失。",
  });
  if (!ok) return;
  try {
    const data = await configApi.fetchDefaults();
    loadingConfig = true;
    config.browser = { ...data.browser };
    config.monitor = { ...data.monitor };
    config.pause = { ...data.pause };
    config.logging = { ...data.logging };
    config.retry = { ...data.retry };
    config.app_settings = { ...data.app_settings };
    // 保留凭据不重置
    loadingConfig = false;
    dirty.value = false;
    frontendLogger.info("config", "已恢复默认设置");
    await saveConfig(true);
  } catch (error) {
    frontendLogger.error("config", "获取默认配置失败", error);
    toastOnly(false, "获取默认配置失败");
  }
}

async function onShellFileSelected(_file: File | undefined): Promise<void> {
  // shell_path 已从后端 AppSettings 移除，保留空函数以兼容外部调用
}

async function fetchShells(): Promise<void> {
  try {
    const data = await autostartApi.fetchShells();
    availableShells.splice(0, availableShells.length, ...(data.shells || []));
    defaultShell.value = data.default || "";
  } catch (error) {
    frontendLogger.warn("config", "获取 Shell 列表失败", error);
    availableShells.splice(0, availableShells.length);
    defaultShell.value = "";
  }
}

async function loadDefaultStealthScript(): Promise<void> {
  try {
    const data = await configApi.fetchStealthScript();
    config.browser.stealth_custom_script = data.script || "";
    frontendLogger.info("config", "已加载默认反检测脚本");
  } catch (error) {
    frontendLogger.warn("config", "获取默认反检测脚本失败", error);
  }
}

async function fetchOcrStatus(): Promise<void> {
  try {
    const data = await ocrApi.fetchStatus();
    Object.assign(ocrStatus, data);
  } catch {
    Object.assign(ocrStatus, { installed: false, size_mb: 0 });
  }
}

async function toggleOcr(action: "install" | "uninstall"): Promise<void> {
  const isInstall = action === "install";
  const { confirm } = useConfirm();
  const ok = await confirm({
    title: isInstall ? "安装 OCR 依赖" : "卸载 OCR 依赖",
    message: isInstall
      ? "确定要安装 OCR 依赖吗？\nddddocr + onnxruntime 约占用 ~120MB 磁盘空间。"
      : "确定要卸载 OCR 依赖吗？\n卸载后 OCR 验证码识别步骤将无法使用。",
    confirmText: isInstall ? "安装" : "卸载",
    danger: !isInstall,
  });
  if (!ok) return;
  busy.ocr = true;
  try {
    const data = isInstall ? await ocrApi.install() : await ocrApi.uninstall();
    frontendLogger.info("ocr", data?.message || `${isInstall ? "安装" : "卸载"}完成`);
    notify(true, (data?.message || `${isInstall ? "安装" : "卸载"}完成`) + "，需重启程序后生效", "install");
    await fetchOcrStatus();
  } catch (error) {
    const msg = extractApiError(error, isInstall ? "安装失败" : "卸载失败");
    frontendLogger.error("ocr", `${isInstall ? "安装" : "卸载"}异常: ${msg}`, error);
    notify(false, msg, "install");
  } finally {
    busy.ocr = false;
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
    const anyErr = error as { response?: { status?: number } };
    if (anyErr?.response?.status === 404) {
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

async function setAutostartMode(runtimeMode: string): Promise<void> {
  try {
    const data = await autostartApi.setMode(runtimeMode);
    frontendLogger.info("autostart", data?.message || "切换自启动模式成功");
    toastOnly(true, data?.message || "切换自启动模式成功");
  } catch (error) {
    frontendLogger.error("autostart", "切换自启动模式异常", error);
    toastOnly(false, "切换自启动模式失败");
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
    availableShells,
    defaultShell,
    ocrStatus,
    dirty,
    saveFailed,
    fetchConfig,
    validateConfig,
    ensureAtLeastOneCheckMethod,
    onPasswordFocus,
    onPasswordBlur,
    onPasswordInput,
    saveConfig,
    resetConfig,
    onShellFileSelected,
    fetchShells,
    loadDefaultStealthScript,
    fetchOcrStatus,
    installOcr: () => toggleOcr("install"),
    uninstallOcr: () => toggleOcr("uninstall"),
    fetchLogLevels,
    setLogLevel,
    toggleAutostart,
    setAutostartMode,
  };
}
