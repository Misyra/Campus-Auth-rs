/**
 * 配置状态与操作（单例）。
 * 替代原 configData + configMethods + 部分 autostart/OCR/日志级别方法。
 * 修复 P1-12.8：用显式 dirty 标志替代每次 JSON.stringify 全量序列化。
 */

import { reactive, ref, watch, nextTick } from "vue";
import type { Config, SaveConfigPayload } from "../api/types";
import { configApi, autostartApi, pureModeApi } from "../api";
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
// F2：配置加载失败标记，为 true 时 SettingsView 保存按钮禁用并提示重试
const configLoadFailed = ref(false);

// 纯净模式（本质是 config.browser.pure_mode，API 为 /api/pure-mode，
// 从 useTasks 迁入：独立于表单 dirty 流程的即时开关状态）
const pureMode = ref(true);
const pureModeLoading = ref(false);

let loadingConfig = false;
let saveSeq = 0;
let saveAbort: AbortController | null = null;
// G20：fetchConfig 请求序号（epoch）守卫——并发/迟到的旧响应不得写入状态，
// 否则会无条件把 dirty 置 false 清掉用户编辑标记
let fetchConfigEpoch = 0;

// 深监听配置变更 → 与最近一次已保存快照比对得出 dirty（加载期间抑制）。
// 早先版本是单向闩锁（动过即 dirty=true，仅保存/重载复位），开关关了再打开
// 仍显示"已变更"；现改为快照比对：值回原样 dirty 自动消失。
// P12：回调仅做一次 JSON.stringify 比对（配置体量小，开销可忽略），无需防抖；
// flush 'post'（渲染后微任务批量执行）。异步化后 fetchConfig 需在复位 loadingConfig
// 前 await nextTick()，让加载期间的赋值在抑制窗口内跑完回调（见 fetchConfig 内注释）。
let savedSnapshot = JSON.stringify(config);
// 程序化写入（服务端已即时保存的日志级别等）期间抑制 dirty 比对，结束后同步快照
let suppressDirty = false;
watch(
  config,
  () => {
    if (loadingConfig || suppressDirty) return;
    dirty.value = JSON.stringify(config) !== savedSnapshot;
  },
  { deep: true, flush: "post" },
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
  // G20：仅最新一次请求可写状态；迟到的旧响应直接丢弃
  const epoch = ++fetchConfigEpoch;
  try {
    const data = await configApi.fetch();
    if (epoch !== fetchConfigEpoch) return;
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
    config.updater = { ...DEFAULT_CONFIG.updater, ...(data.updater || {}) };
    // 旧配置只有 proxy_port（可能非默认值）：派生完整地址，
    // 保证输入框显示与后端 resolved_proxy_url 实际使用一致
    if (!config.updater.proxy_url && config.updater.proxy_port > 0) {
      config.updater.proxy_url = `http://127.0.0.1:${config.updater.proxy_port}`;
    }
    password.reset(!!data.has_password);
    // P12：watch 已是异步 flush，上面的加载赋值会在微任务中触发回调；
    // 先等待一轮刷新（回调在 loadingConfig=true 窗口内执行完、不计入 dirty），
    // 再以加载结果为新快照，保证加载不被误标为未保存修改
    await nextTick();
    // G20：nextTick 窗口内若又有更新的 fetchConfig 接管，交由它负责复位状态
    if (epoch !== fetchConfigEpoch) return;
    loadingConfig = false;
    savedSnapshot = JSON.stringify(config);
    dirty.value = false;
    configLoadFailed.value = false;
    frontendLogger.info("config", "配置已加载");
  } catch (error) {
    // G20：迟到/被取代的旧请求失败同样不写状态（避免覆盖新请求的结果或误报）
    if (epoch !== fetchConfigEpoch) return;
    frontendLogger.error("config", "获取配置失败", error);
    // F2：首次失败 toast 提示
    configLoadFailed.value = true;
    toastOnly(false, "加载配置失败");
  }
}

function validateConfig(): string[] {
  const warnings: string[] = [];
  const url = config.credentials.auth_url;
  if (url && !/^https?:\/\//.test(url)) {
    warnings.push("认证地址必须以 http:// 或 https:// 开头");
  }
  // 与后端 build_proxied_client 的校验口径一致
  const proxyUrl = config.updater.proxy_url;
  if (config.updater.use_proxy && proxyUrl && !/^https?:\/\//.test(proxyUrl)) {
    warnings.push("代理地址必须以 http:// 或 https:// 开头");
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
    updater: config.updater,
    active_task: config.active_task || "",
    username: config.credentials.username ?? "",
    auth_url: config.credentials.auth_url ?? "",
    isp: config.credentials.isp ?? "",
    password: pwdValue as string | null,
  };

  try {
    await configApi.patch(payload, { signal: controller.signal });
    if (submittedPassword) password.markSaved(true);
    // 保存成功后以当前表单为新快照：用户把值改回原样时 dirty 自动消失
    savedSnapshot = JSON.stringify(config);
    dirty.value = false;
    frontendLogger.info("config", "配置保存成功");
  } catch (error) {
    // G19：被 saveAbort.abort() 顶替的旧保存请求属预期取消（client.ts 已把
    // AbortError 转为 name="ApiError" 的 ApiError，此处检查 aborted 标记），
    // 静默返回：不弹失败 toast、不置 saveFailed
    if (error instanceof ApiError && error.aborted) return;
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
    // 读取的是服务端已保存值：抑制 dirty 并在无未保存编辑时同步快照
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.logging.level = data.level;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
  } catch (error) {
    frontendLogger.warn("config", "获取日志级别配置失败", error);
  }
}

async function setLogLevel(level: string): Promise<void> {
  try {
    const data = await configApi.setLogLevel(level);
    // 日志级别走独立 API 即时保存，不算表单未保存变更
    const wasDirty = dirty.value;
    suppressDirty = true;
    config.logging.level = level;
    await nextTick();
    if (!wasDirty) savedSnapshot = JSON.stringify(config);
    suppressDirty = false;
    frontendLogger.setLevel(level);
    frontendLogger.info("config", `日志级别已设置: ${level}`);
    toastOnly(true, data?.message || "日志级别已设置");
  } catch (error) {
    const msg = extractApiError(error, "设置失败");
    frontendLogger.error("config", "设置日志级别失败", error);
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

async function fetchPureMode(): Promise<void> {
  try {
    const data = await pureModeApi.fetch();
    pureMode.value = data.enabled;
  } catch (error) {
    frontendLogger.debug("config", "获取纯净模式失败，保持默认", error);
  }
}

async function togglePureMode(): Promise<void> {
  if (pureModeLoading.value) return;
  pureModeLoading.value = true;
  try {
    const data = await pureModeApi.toggle();
    const enabled = data?.enabled ?? false;
    pureMode.value = enabled;
    frontendLogger.info("config", `纯净模式已${enabled ? "开启" : "关闭"}`);
    toastOnly(true, `纯净模式已${enabled ? "开启" : "关闭"}`);
  } catch (error) {
    pureMode.value = !pureMode.value;
    frontendLogger.error("config", "切换纯净模式失败", error);
    toastOnly(false, "切换纯净模式失败");
  } finally {
    pureModeLoading.value = false;
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
    configLoadFailed,
    pureMode,
    pureModeLoading,
    fetchConfig,
    onPasswordFocus: password.onFocus,
    onPasswordBlur: password.onBlur,
    onPasswordInput,
    saveConfig,
    fetchLogLevels,
    setLogLevel,
    toggleAutostart,
    fetchPureMode,
    togglePureMode,
  };
}
